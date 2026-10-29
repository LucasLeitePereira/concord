//! Screen-share viewer window (`screen-share`, F4): decodes the frames of a friend's share and
//! draws the newest picture with `winit` + `wgpu`.
//!
//! Threads: the *window thread* owns the event loop and the GPU; the *decode thread* owns the
//! decoder, reads the frame channel and leaves only the **latest** picture in a one-slot mailbox
//! (late pictures are overwritten, never queued). Closing hands the input channel back, so the
//! caller can open the viewer again on the same stream.
//!
//! Keys: `F11` full screen, `F3` statistics in the title, `Esc` closes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sala_protocol::VideoCodec;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::{Fullscreen, Window, WindowId};

use crate::decoder::{RgbaFrame, VideoDecoder};
use crate::fragment::Frame;

/// Minimum spacing between two keyframe requests (`inv-keyframe-request-500ms`).
pub const KEYFRAME_REQUEST_SPACING_MS: u64 = 500;

/// What the viewer consumes: one reassembled frame of a friend's share.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub friend: String,
    pub codec: VideoCodec,
    pub frame: Frame,
}

/// Called (from the decode thread) with the friend's name when a keyframe is needed.
pub type KeyframeRequester = Arc<dyn Fn(&str) + Send + Sync>;

// ------------------------------------------------------------------ pure helpers

/// Largest rectangle with the picture's aspect ratio centred in the window: `(x, y, w, h)`.
pub fn letterbox(win_w: u32, win_h: u32, img_w: u32, img_h: u32) -> (u32, u32, u32, u32) {
    if win_w == 0 || win_h == 0 || img_w == 0 || img_h == 0 {
        return (0, 0, win_w, win_h);
    }
    // Compare win_w/win_h with img_w/img_h without floats.
    let (w, h) = if u64::from(win_w) * u64::from(img_h) <= u64::from(win_h) * u64::from(img_w) {
        // The window is narrower than the picture: full width, bars above and below.
        (win_w, (u64::from(win_w) * u64::from(img_h) / u64::from(img_w)) as u32)
    } else {
        ((u64::from(win_h) * u64::from(img_w) / u64::from(img_h)) as u32, win_h)
    };
    ((win_w - w) / 2, (win_h - h) / 2, w.max(1), h.max(1))
}

/// Lets a keyframe request through at most once per [`KEYFRAME_REQUEST_SPACING_MS`].
#[derive(Debug, Default)]
pub struct KeyframeLimiter {
    last_ms: Option<u64>,
}

impl KeyframeLimiter {
    pub fn allow(&mut self, now_ms: u64) -> bool {
        if self.last_ms.is_none_or(|l| now_ms.saturating_sub(l) >= KEYFRAME_REQUEST_SPACING_MS) {
            self.last_ms = Some(now_ms);
            true
        } else {
            false
        }
    }
}

/// Numbers shown by the `F3` overlay.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StatsSnapshot {
    pub fps: f32,
    pub kbps: u32,
    /// Frames the decoder could not use in the last window (lost, gaps, errors).
    pub lost: u32,
}

/// Rolling one-second window of pictures, received bytes and lost frames.
#[derive(Debug, Default)]
pub struct StatsMeter {
    window_start_ms: u64,
    pictures: u32,
    bytes: u64,
    lost: u32,
    last: StatsSnapshot,
}

impl StatsMeter {
    const WINDOW_MS: u64 = 1000;

    fn roll(&mut self, now_ms: u64) {
        let span = now_ms.saturating_sub(self.window_start_ms);
        if span >= Self::WINDOW_MS {
            let secs = span as f32 / 1000.0;
            self.last = StatsSnapshot {
                fps: self.pictures as f32 / secs,
                kbps: (self.bytes as f32 * 8.0 / 1000.0 / secs) as u32,
                lost: self.lost,
            };
            self.window_start_ms = now_ms;
            self.pictures = 0;
            self.bytes = 0;
            self.lost = 0;
        }
    }

    pub fn on_received(&mut self, now_ms: u64, bytes: usize) {
        self.roll(now_ms);
        self.bytes += bytes as u64;
    }

    pub fn on_picture(&mut self, now_ms: u64) {
        self.roll(now_ms);
        self.pictures += 1;
    }

    pub fn on_lost(&mut self, now_ms: u64) {
        self.roll(now_ms);
        self.lost += 1;
    }

    /// The last completed window; goes to zero when nothing arrived for a while.
    pub fn snapshot(&mut self, now_ms: u64) -> StatsSnapshot {
        self.roll(now_ms);
        if now_ms.saturating_sub(self.window_start_ms) > 3 * Self::WINDOW_MS {
            self.last = StatsSnapshot::default();
        }
        self.last
    }
}

fn title(friend: &str, stats: Option<StatsSnapshot>) -> String {
    let who = if friend.is_empty() { "aguardando imagem".to_string() } else { format!("Tela de {friend}") };
    match stats {
        Some(s) => format!("Concord - {who} | {:.0} fps | {} kbps | {} perdidos", s.fps, s.kbps, s.lost),
        None => format!("Concord - {who}"),
    }
}

// ------------------------------------------------------------------ shared state

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Mailbox {
    picture: Option<RgbaFrame>,
    friend: String,
    stats: StatsSnapshot,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    /// A `NewPicture` event is already in flight (avoids flooding the event loop).
    notified: AtomicBool,
    closed: AtomicBool,
}

#[derive(Debug)]
enum UserEvent {
    NewPicture,
    Close,
}

// ------------------------------------------------------------------ public handle

/// A running viewer window. Dropping it (or [`ViewerHandle::close`]) closes the window and
/// releases the GPU.
pub struct ViewerHandle {
    proxy: EventLoopProxy<UserEvent>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Receiver<Incoming>>>,
}

impl ViewerHandle {
    /// The window is still on screen (it turns `false` when the user closes it).
    pub fn is_open(&self) -> bool {
        !self.shared.closed.load(Ordering::Acquire)
    }

    /// Closes the window, waits for the threads and returns the input channel.
    pub fn close(mut self) -> Option<Receiver<Incoming>> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Option<Receiver<Incoming>> {
        let thread = self.thread.take()?;
        let _ = self.proxy.send_event(UserEvent::Close);
        thread.join().ok()
    }
}

impl Drop for ViewerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Opens the viewer on `rx`. `request_keyframe(friend)` is called when the decoder needs one
/// (including at start, as soon as the first delta frame shows up).
pub fn open(rx: Receiver<Incoming>, request_keyframe: KeyframeRequester) -> Result<ViewerHandle, String> {
    let shared = Arc::new(Shared {
        mailbox: Mutex::new(Mailbox::default()),
        notified: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<EventLoopProxy<UserEvent>, String>>();
    let thread_shared = shared.clone();
    let thread = std::thread::Builder::new()
        .name("viewer".into())
        .spawn(move || window_thread(rx, request_keyframe, thread_shared, ready_tx))
        .map_err(|e| e.to_string())?;
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(proxy)) => Ok(ViewerHandle { proxy, shared, thread: Some(thread) }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("the viewer window did not start".into()),
    }
}

// ------------------------------------------------------------------ window thread

fn window_thread(
    rx: Receiver<Incoming>,
    request_keyframe: KeyframeRequester,
    shared: Arc<Shared>,
    ready: std::sync::mpsc::Sender<Result<EventLoopProxy<UserEvent>, String>>,
) -> Receiver<Incoming> {
    let event_loop = match EventLoop::<UserEvent>::with_user_event().with_any_thread(true).build() {
        Ok(el) => el,
        Err(e) => {
            let _ = ready.send(Err(format!("cannot create the event loop: {e}")));
            return rx;
        }
    };
    let proxy = event_loop.create_proxy();
    let _ = ready.send(Ok(proxy.clone()));

    let decode = {
        let (shared, proxy) = (shared.clone(), proxy);
        std::thread::Builder::new()
            .name("viewer-decode".into())
            .spawn(move || decode_thread(rx, request_keyframe, shared, proxy))
    };
    let Ok(decode) = decode else {
        shared.closed.store(true, Ordering::Release);
        // `rx` moved into the failed closure: nothing to hand back.
        return std::sync::mpsc::channel().1;
    };

    let mut app = App { shared: shared.clone(), gfx: None, show_stats: false, fullscreen: false, failed: false };
    if let Err(e) = event_loop.run_app(&mut app) {
        tracing::warn!("viewer event loop ended with an error: {e}");
    }
    // Window and GPU objects go away here, before the thread returns.
    drop(app);
    shared.closed.store(true, Ordering::Release);
    tracing::info!("viewer closed");
    decode.join().unwrap_or_else(|_| std::sync::mpsc::channel().1)
}

fn decode_thread(
    rx: Receiver<Incoming>,
    request_keyframe: KeyframeRequester,
    shared: Arc<Shared>,
    proxy: EventLoopProxy<UserEvent>,
) -> Receiver<Incoming> {
    let started = Instant::now();
    let now_ms = || started.elapsed().as_millis() as u64;
    let mut decoder: Option<(String, VideoDecoder)> = None;
    let mut limiter = KeyframeLimiter::default();
    let mut meter = StatsMeter::default();
    let mut last_id: Option<u32> = None;
    let mut pictures = 0u64;
    let ask = |friend: &str, limiter: &mut KeyframeLimiter, now: u64| {
        if limiter.allow(now) {
            request_keyframe(friend);
        }
    };

    while !shared.closed.load(Ordering::Acquire) {
        let inc = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(i) => i,
            Err(RecvTimeoutError::Timeout) => continue,
            // The stream is gone (session ended): idle until the window is closed.
            Err(RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let now = now_ms();
        meter.on_received(now, inc.frame.data.len());

        let same = decoder.as_ref().is_some_and(|(f, d)| *f == inc.friend && d.codec() == inc.codec);
        if !same {
            match VideoDecoder::open(inc.codec) {
                Ok(d) => {
                    tracing::info!(friend = %inc.friend, decoder = d.name(), "viewer: decoder ready");
                    decoder = Some((inc.friend.clone(), d));
                    last_id = None;
                }
                Err(e) => {
                    tracing::warn!("viewer: {e}");
                    decoder = None;
                    continue;
                }
            }
        }
        let (_, dec) = decoder.as_mut().expect("opened above");

        // A hole in the numbering (queue overflow upstream) breaks the reference chain.
        let gap = last_id.is_some_and(|l| inc.frame.frame_id != l.wrapping_add(1));
        last_id = Some(inc.frame.frame_id);
        if gap && !inc.frame.keyframe {
            meter.on_lost(now);
            ask(&inc.friend, &mut limiter, now);
            continue;
        }

        let out = dec.decode(&inc.frame);
        if out.request_keyframe {
            meter.on_lost(now);
            ask(&inc.friend, &mut limiter, now);
        }
        if let Some(pic) = out.frame {
            meter.on_picture(now);
            pictures += 1;
            if pictures == 1 || pictures.is_multiple_of(120) {
                tracing::info!(pictures, width = pic.width, height = pic.height, "viewer: decoded");
            }
            {
                let mut mb = lock(&shared.mailbox);
                mb.picture = Some(pic); // overwrites a picture the window never got to draw
                if mb.friend != inc.friend {
                    mb.friend = inc.friend.clone();
                }
                mb.stats = meter.snapshot(now);
            }
            if !shared.notified.swap(true, Ordering::AcqRel) {
                let _ = proxy.send_event(UserEvent::NewPicture);
            }
        }
    }
    rx
}

// ------------------------------------------------------------------ rendering

const SHADER: &str = "
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    // One triangle covering the whole viewport.
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(tex, smp, in.uv).rgb, 1.0);
}
";

struct Picture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

struct Gfx {
    // Field order matters: the surface must drop before the window it was created from.
    surface: wgpu::Surface<'static>,
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    picture: Option<Picture>,
}

impl Gfx {
    fn new(window: Arc<Window>) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or("no compatible GPU adapter")?;
        tracing::info!(adapter = %adapter.get_info().name, "viewer: GPU adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&adapter);
        // The picture is already display-encoded: a non-sRGB target avoids a second conversion.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).or(caps.formats.first().copied()).ok_or("no surface format")?;
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewer"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewer"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewer"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Gfx { surface, window, device, queue, config, pipeline, layout, sampler, picture: None })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return; // minimised
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Uploads a picture (re-creating the texture when the size changed).
    fn upload(&mut self, pic: &RgbaFrame) {
        if self.picture.as_ref().is_none_or(|p| p.width != pic.width || p.height != pic.height) {
            let size = wgpu::Extent3d { width: pic.width, height: pic.height, depth_or_array_layers: 1 };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("share"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("share"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.picture = Some(Picture { texture, bind_group, width: pic.width, height: pic.height });
        }
        let p = self.picture.as_ref().expect("set above");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &p.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pic.data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pic.width * 4), rows_per_image: Some(pic.height) },
            wgpu::Extent3d { width: pic.width, height: pic.height, depth_or_array_layers: 1 },
        );
    }

    fn draw(&mut self) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(e) => {
                tracing::debug!("viewer: surface error: {e}");
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(p) = &self.picture {
                let (x, y, w, h) = letterbox(self.config.width, self.config.height, p.width, p.height);
                pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &p.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.window.pre_present_notify();
        frame.present();
    }
}

struct App {
    shared: Arc<Shared>,
    gfx: Option<Gfx>,
    show_stats: bool,
    fullscreen: bool,
    failed: bool,
}

impl App {
    fn refresh_title(&self, friend: &str, stats: StatsSnapshot) {
        if let Some(g) = &self.gfx {
            g.window.set_title(&title(friend, self.show_stats.then_some(stats)));
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() || self.failed {
            return;
        }
        let attrs = Window::default_attributes().with_title(title("", None)).with_inner_size(LogicalSize::new(1280.0, 720.0));
        let result = el.create_window(attrs).map_err(|e| e.to_string()).and_then(|w| Gfx::new(Arc::new(w)));
        match result {
            Ok(g) => {
                tracing::info!("viewer: window open");
                self.gfx = Some(g);
            }
            Err(e) => {
                tracing::warn!("viewer: cannot open the window: {e}");
                self.failed = true;
                el.exit();
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Close => el.exit(),
            UserEvent::NewPicture => {
                let (pic, friend, stats) = {
                    let mut mb = lock(&self.shared.mailbox);
                    (mb.picture.take(), mb.friend.clone(), mb.stats)
                };
                self.shared.notified.store(false, Ordering::Release);
                if let Some(pic) = pic
                    && let Some(g) = self.gfx.as_mut()
                {
                    g.upload(&pic);
                    g.window.request_redraw();
                }
                self.refresh_title(&friend, stats);
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gfx.as_mut() {
                    g.resize(size.width, size.height);
                    g.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(g) = self.gfx.as_mut() {
                    g.draw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat => {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => el.exit(),
                    PhysicalKey::Code(KeyCode::F11) => {
                        self.fullscreen = !self.fullscreen;
                        if let Some(g) = &self.gfx {
                            g.window.set_fullscreen(self.fullscreen.then_some(Fullscreen::Borderless(None)));
                        }
                    }
                    PhysicalKey::Code(KeyCode::F3) => {
                        self.show_stats = !self.show_stats;
                        let (friend, stats) = {
                            let mb = lock(&self.shared.mailbox);
                            (mb.friend.clone(), mb.stats)
                        };
                        self.refresh_title(&friend, stats);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_keeps_the_aspect_ratio() {
        // 16:9 picture in a 16:9 window: fills it.
        assert_eq!(letterbox(1280, 720, 1920, 1080), (0, 0, 1280, 720));
        // Taller window: bars above and below.
        assert_eq!(letterbox(1000, 1000, 1920, 1080), (0, 219, 1000, 562));
        // Wider window: bars at the sides.
        assert_eq!(letterbox(2000, 720, 1280, 720), (360, 0, 1280, 720));
        // Degenerate input never divides by zero.
        assert_eq!(letterbox(0, 0, 1920, 1080), (0, 0, 0, 0));
        assert_eq!(letterbox(800, 600, 0, 0), (0, 0, 800, 600));
    }

    #[test]
    fn letterbox_never_overflows_the_window() {
        for (ww, wh, iw, ih) in [(1, 1, 1920, 1080), (7, 3, 3, 7), (4096, 100, 640, 360), (101, 4096, 640, 360)] {
            let (x, y, w, h) = letterbox(ww, wh, iw, ih);
            assert!(x + w <= ww.max(1) && y + h <= wh.max(1), "{ww}x{wh} -> {x},{y} {w}x{h}");
        }
    }

    #[test]
    fn keyframe_requests_are_spaced() {
        let mut l = KeyframeLimiter::default();
        assert!(l.allow(0));
        assert!(!l.allow(100));
        assert!(!l.allow(499));
        assert!(l.allow(500));
        assert!(!l.allow(700));
        assert!(l.allow(1000));
    }

    #[test]
    fn stats_report_the_last_full_window() {
        let mut m = StatsMeter::default();
        for i in 0..30u64 {
            m.on_received(i * 33, 5000);
            m.on_picture(i * 33);
        }
        m.on_lost(990);
        // Window not closed yet: nothing reported.
        assert_eq!(m.snapshot(999), StatsSnapshot::default());
        let s = m.snapshot(1000);
        assert!((s.fps - 30.0).abs() < 0.1, "fps {}", s.fps);
        assert_eq!(s.kbps, 1200); // 30 * 5000 B * 8 / 1 s
        assert_eq!(s.lost, 1);
        // Long silence: back to zero instead of showing a stale rate.
        assert_eq!(m.snapshot(10_000), StatsSnapshot::default());
    }

    #[test]
    fn title_shows_stats_only_when_asked() {
        let s = StatsSnapshot { fps: 59.6, kbps: 5400, lost: 2 };
        assert_eq!(title("Ana", None), "Concord - Tela de Ana");
        assert_eq!(title("Ana", Some(s)), "Concord - Tela de Ana | 60 fps | 5400 kbps | 2 perdidos");
        assert_eq!(title("", None), "Concord - aguardando imagem");
    }
}
