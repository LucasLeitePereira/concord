//! Testes ocultos de aceite do par T-003 (não mostrados aos braços).
use sala_protocol::*;

fn all_control() -> Vec<ControlMessage> {
    vec![
        ControlMessage::Hello {
            name: "Lucas".into(),
            app_version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Capabilities {
                hw_encode: vec![VideoCodec::Av1, VideoCodec::H264],
                hw_decode: vec![VideoCodec::Hevc],
            },
        },
        ControlMessage::Status { muted: true, deafened: false, game_mode: true, sharing: false, in_room: true },
        ControlMessage::Chat { id: u64::MAX, text: "olá, ação! 🎮\nlinha 2".into(), sent_at: 1_760_000_000_000 },
        ControlMessage::ShareStart { codec: VideoCodec::Hevc, width: 2560, height: 1440, fps: 60 },
        ControlMessage::ShareStop,
        ControlMessage::KeyframeRequest,
        ControlMessage::Heartbeat,
        ControlMessage::Bye,
    ]
}

#[test]
fn h01_control_roundtrip_all_variants() {
    let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
    for m in all_control() {
        dec.push(&m.encode_frame().unwrap());
        assert_eq!(dec.next_message().unwrap(), Some(m));
        assert_eq!(dec.next_message().unwrap(), None);
    }
}

#[test]
fn h02_frame_header_is_u32_be_length() {
    let f = ControlMessage::Bye.encode_frame().unwrap();
    let len = u32::from_be_bytes([f[0], f[1], f[2], f[3]]) as usize;
    assert_eq!(len, f.len() - 4);
}

#[test]
fn h03_decoder_byte_by_byte_and_coalesced() {
    let msgs = all_control();
    let mut stream = Vec::new();
    for m in &msgs {
        stream.extend(m.encode_frame().unwrap());
    }
    // byte a byte
    let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
    let mut got = Vec::new();
    for b in &stream {
        dec.push(std::slice::from_ref(b));
        while let Some(m) = dec.next_message().unwrap() {
            got.push(m);
        }
    }
    assert_eq!(got, msgs);
    // tudo de uma vez
    let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
    dec.push(&stream);
    let mut got = Vec::new();
    while let Some(m) = dec.next_message().unwrap() {
        got.push(m);
    }
    assert_eq!(got, msgs);
}

#[test]
fn h04_decoder_rejects_oversized_declared_length() {
    let mut dec = FrameDecoder::new(1024);
    dec.push(&(1_000_000u32).to_be_bytes());
    assert!(matches!(dec.next_message(), Err(ProtocolError::TooLarge { .. })));
}

#[test]
fn h05_decoder_garbage_payload_is_error_not_panic() {
    let mut dec = FrameDecoder::new(MAX_CONTROL_FRAME);
    let mut f = 3u32.to_be_bytes().to_vec();
    f.extend([0xFF, 0xFF, 0xFF]);
    dec.push(&f);
    assert!(matches!(dec.next_message(), Err(ProtocolError::Decode(_))));
}

#[test]
fn h06_encode_frame_too_large() {
    let m = ControlMessage::Chat { id: 1, text: "x".repeat(MAX_CONTROL_FRAME + 1), sent_at: 0 };
    assert!(matches!(m.encode_frame(), Err(ProtocolError::TooLarge { .. })));
}

#[test]
fn h07_datagram_roundtrip() {
    let a = MediaDatagram::Audio { stream: AudioStream::ShareAudio, seq: u16::MAX, timestamp: 960, payload: vec![7; 200] };
    let v = MediaDatagram::Video { frame_id: 42, frag_idx: 3, frag_count: 9, keyframe: true, timestamp: 123, payload: vec![1; 500] };
    for d in [a, v] {
        let bytes = d.encode(DEFAULT_MAX_DATAGRAM).unwrap();
        assert!(bytes.len() <= DEFAULT_MAX_DATAGRAM);
        assert_eq!(MediaDatagram::decode(&bytes).unwrap(), d);
    }
}

#[test]
fn h08_max_payload_is_tight_bound() {
    for max in [DEFAULT_MAX_DATAGRAM, 1350, 600] {
        let pv = max_video_payload(max);
        let v = |n| MediaDatagram::Video { frame_id: u32::MAX, frag_idx: u16::MAX, frag_count: u16::MAX, keyframe: true, timestamp: u32::MAX, payload: vec![0xAB; n] };
        let ok = v(pv).encode(max).unwrap();
        assert!(ok.len() <= max, "video {} > {}", ok.len(), max);
        assert!(matches!(v(pv + 1).encode(max), Err(ProtocolError::TooLarge { .. })), "video bound not tight at {max}");

        let pa = max_audio_payload(max);
        let a = |n| MediaDatagram::Audio { stream: AudioStream::Voice, seq: u16::MAX, timestamp: u32::MAX, payload: vec![0xCD; n] };
        assert!(a(pa).encode(max).unwrap().len() <= max);
        assert!(matches!(a(pa + 1).encode(max), Err(ProtocolError::TooLarge { .. })), "audio bound not tight at {max}");
    }
}

#[test]
fn h09_datagram_decode_garbage_and_truncated() {
    assert!(MediaDatagram::decode(&[]).is_err());
    assert!(MediaDatagram::decode(&[0xFF; 16]).is_err());
    let d = MediaDatagram::Audio { stream: AudioStream::Voice, seq: 1, timestamp: 2, payload: vec![9; 50] };
    let bytes = d.encode(DEFAULT_MAX_DATAGRAM).unwrap();
    assert!(MediaDatagram::decode(&bytes[..bytes.len() - 10]).is_err());
}

#[test]
fn h10_ipc_single_line_even_with_newlines() {
    let m = UiToCore::SendChat { text: "a\nb\r\nc".into() };
    let line = encode_ipc_line(&m).unwrap();
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1);
    assert_eq!(decode_ipc_line::<UiToCore>(&line).unwrap(), m);
    let crlf = line.trim_end_matches('\n').to_string() + "\r\n";
    assert_eq!(decode_ipc_line::<UiToCore>(&crlf).unwrap(), m);
}

#[test]
fn h11_ipc_roundtrip_core_to_ui() {
    let msgs = vec![
        CoreToUi::ChatHistory { entries: vec![ChatEntry { id: 1, from_self: true, text: "oi".into(), sent_at: 5 }] },
        CoreToUi::Status { muted: false, deafened: true, game_mode: false, in_room: true },
        CoreToUi::Shutdown,
        CoreToUi::Error { message: "falhou".into() },
    ];
    for m in msgs {
        let line = encode_ipc_line(&m).unwrap();
        assert_eq!(decode_ipc_line::<CoreToUi>(&line).unwrap(), m);
    }
    assert!(decode_ipc_line::<CoreToUi>("{not json").is_err());
}
