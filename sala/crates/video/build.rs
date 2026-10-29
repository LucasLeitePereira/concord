//! Copies the FFmpeg DLLs (from `FFMPEG_DIR/bin`) next to the built executables so that
//! `concord.exe`, tests and examples find them without touching PATH.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let Some(dir) = std::env::var_os("FFMPEG_DIR") else { return };
    let bin = Path::new(&dir).join("bin");
    println!("cargo:rerun-if-changed={}", bin.display());
    let Some(out) = std::env::var_os("OUT_DIR") else { return };
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
    let profile_dir: PathBuf = Path::new(&out).ancestors().nth(3).map(Path::to_path_buf).unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(&bin) else {
        println!("cargo:warning=FFmpeg DLLs not found in {} (run scripts/fetch-ffmpeg.ps1)", bin.display());
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")) {
            for target in [profile_dir.clone(), profile_dir.join("deps")] {
                if target.is_dir() {
                    let _ = std::fs::copy(&path, target.join(entry.file_name()));
                }
            }
        }
    }
}
