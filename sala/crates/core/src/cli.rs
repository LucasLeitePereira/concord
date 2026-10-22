//! Manual command-line parsing (no CLI crate on purpose).

/// Arguments accepted by `sala-core`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CliArgs {
    /// `--profile <name>`: isolated instance (see `Profile`).
    pub profile: Option<String>,
    /// `--synthetic-audio`: use a synthetic audio source instead of a device.
    pub synthetic_audio: bool,
    /// `--synthetic-video`: use a synthetic video source instead of screen capture.
    pub synthetic_video: bool,
    /// `--probe`: print diagnostics and exit.
    pub probe: bool,
}

impl CliArgs {
    /// Parses arguments WITHOUT the program name (i.e. `env::args().skip(1)`).
    pub fn parse(args: impl Iterator<Item = String>) -> Result<CliArgs, String> {
        let mut out = CliArgs::default();
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--profile" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--profile requires a name".to_string())?;
                    if value.starts_with("--") {
                        return Err(format!("--profile requires a name, got flag {value}"));
                    }
                    out.profile = Some(value);
                }
                "--synthetic-audio" => out.synthetic_audio = true,
                "--synthetic-video" => out.synthetic_video = true,
                "--probe" => out.probe = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<CliArgs, String> {
        CliArgs::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn empty_is_default() {
        assert_eq!(parse(&[]).unwrap(), CliArgs::default());
    }

    #[test]
    fn all_flags() {
        let a = parse(&["--profile", "a", "--synthetic-audio", "--synthetic-video", "--probe"])
            .unwrap();
        assert_eq!(a.profile.as_deref(), Some("a"));
        assert!(a.synthetic_audio && a.synthetic_video && a.probe);
    }

    #[test]
    fn flags_are_independent() {
        let a = parse(&["--probe"]).unwrap();
        assert!(a.probe && !a.synthetic_audio && !a.synthetic_video && a.profile.is_none());
    }

    #[test]
    fn unknown_flag_is_error() {
        assert!(parse(&["--nope"]).unwrap_err().contains("--nope"));
        assert!(parse(&["--profile", "a", "extra"]).is_err());
    }

    #[test]
    fn profile_without_value_is_error() {
        assert!(parse(&["--profile"]).is_err());
        assert!(parse(&["--profile", "--probe"]).is_err());
    }
}
