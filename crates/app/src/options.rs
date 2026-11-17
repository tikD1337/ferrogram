use std::path::PathBuf;
#[derive(Debug, Default)]
pub(crate) struct Options {
    pub demo: bool,
    pub demo_text_only: bool,
    pub oled: bool,
    pub snapshot: Option<PathBuf>,
    pub help: bool,
    pub demo_scroll: Option<u32>,
    pub demo_video: Option<u32>,
    pub demo_voice: Option<u32>,
    pub demo_stickers: bool,
}
impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        // No retained argument collection. Each path is limited to Windows' maximum path length.
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--demo" => options.demo = true,
                "--demo-text-only" => {
                    options.demo = true;
                    options.demo_text_only = true;
                }
                "--demo-scroll" => {
                    let seconds = args
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|s| (1..=86400).contains(s))
                        .ok_or("--demo-scroll requires 1..86400 seconds")?;
                    options.demo = true;
                    options.demo_scroll = Some(seconds);
                }
                "--demo-stickers" => {
                    options.demo = true;
                    options.demo_stickers = true;
                }
                "--demo-voice" => {
                    options.demo = true;
                    options.demo_voice = Some(
                        args.next()
                            .and_then(|s| s.parse::<u32>().ok())
                            .filter(|s| (3..=86400).contains(s))
                            .ok_or("--demo-voice requires 3..86400 seconds")?,
                    );
                }
                "--demo-video" => {
                    let seconds = args
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|s| (2..=86400).contains(s))
                        .ok_or("--demo-video requires 2..86400 seconds")?;
                    options.demo = true;
                    options.demo_video = Some(seconds);
                }
                "--theme" => match args.next().as_deref() {
                    Some("dark") => options.oled = false,
                    Some("oled") => options.oled = true,
                    _ => return Err("--theme requires dark or oled"),
                },
                "--snapshot" => {
                    let path = args.next().ok_or("--snapshot requires a PNG path")?;
                    if path.len() > 32768 || !path.to_ascii_lowercase().ends_with(".png") {
                        return Err("--snapshot requires a PNG path (at most 32768 bytes)");
                    }
                    options.snapshot = Some(path.into());
                }
                "--help" | "-h" => options.help = true,
                _ => return Err("Unknown option; use --help"),
            }
        }
        Ok(options)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flags_and_invalid_values() {
        let options = Options::parse(
            ["--demo", "--theme", "oled", "--snapshot", "demo.png"].map(str::to_owned),
        )
        .unwrap();
        assert!(options.demo && options.oled);
        let scroll = Options::parse(["--demo-scroll", "120"].map(str::to_owned)).unwrap();
        assert!(scroll.demo);
        assert_eq!(scroll.demo_scroll, Some(120));
        for value in ["0", "-1", "abc", "86401"] {
            assert!(Options::parse(["--demo-scroll", value].map(str::to_owned)).is_err());
        }
        assert_eq!(options.snapshot, Some("demo.png".into()));
        for args in [
            vec!["--theme"],
            vec!["--theme", "light"],
            vec!["--snapshot", "out.jpg"],
            vec!["--unknown"],
        ] {
            assert!(Options::parse(args.into_iter().map(str::to_owned)).is_err());
        }
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn attachment_demo_flags_are_explicit_and_voice_duration_is_bounded() {
        let s = Options::parse(["--demo-stickers".into()]).unwrap();
        assert!(s.demo && s.demo_stickers);
        let v = Options::parse(["--demo-voice".into(), "12".into()]).unwrap();
        assert!(v.demo);
        assert_eq!(v.demo_voice, Some(12));
        for value in ["0", "2", "86401", "x"] {
            assert!(Options::parse(["--demo-voice".into(), value.into()]).is_err());
        }
    }
}
