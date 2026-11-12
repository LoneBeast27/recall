//! The engine's command line: what it is asked to do (record, or one of the one-shot
//! commands), split out of `main.rs`.

/// Parsed command-line config.
#[derive(Default, Clone)]
pub(crate) struct Config {
    pub(crate) json: bool,
    pub(crate) monitor: Option<usize>,
    pub(crate) encoder_smoke: Option<String>,
    pub(crate) gpu_smoke: Option<String>,
    pub(crate) export_cfr: Option<(String, String, u32)>,
    pub(crate) prepare_editor: Option<(String, String)>,
    pub(crate) export_edit: Option<(String, String, u32, String)>,
    /// `--upscale-notice IN_W IN_H OUT_W OUT_H`: the export page's upscaler notice as JSON.
    pub(crate) upscale_notice: Option<((u32, u32), (u32, u32))>,
}

pub(crate) const USAGE: &str = "usage: recall [--json] [--monitor N]   (records; one engine per user)
       recall --prepare-editor INPUT CACHE_DIR
       recall --export-edit INPUT OUTPUT FPS PROJECT.json [--json]
       recall --export-cfr INPUT OUTPUT [FPS] [--json]
       recall --upscale-notice IN_W IN_H OUT_W OUT_H
       recall --encoder-smoke DIR | --gpu-smoke DIR
       recall --help";

/// What the command line asks for. Built once per process, so the size of `Run` does not matter
/// (moved out of `main.rs` unchanged).
#[allow(clippy::large_enum_variant)]
pub(crate) enum Args {
    Run(Config),
    Help,
    Bad(String),
}

pub(crate) fn parse_args() -> Args {
    parse_args_from(std::env::args().skip(1))
}

/// Unknown arguments are refused: an engine that ignored them would start recording.
fn parse_args_from(args: impl IntoIterator<Item = String>) -> Args {
    let mut cfg = Config::default();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => cfg.json = true,
            "--encoder-smoke" => cfg.encoder_smoke = args.next(),
            "--gpu-smoke" => cfg.gpu_smoke = args.next(),
            "--prepare-editor" => {
                cfg.prepare_editor = Some((args.next().unwrap_or_default(), args.next().unwrap_or_default()));
            }
            "--export-edit" => {
                let input = args.next().unwrap_or_default();
                let output = args.next().unwrap_or_default();
                let fps = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
                let project = args.next().unwrap_or_default();
                cfg.export_edit = Some((input, output, fps, project));
            }
            "--export-cfr" => {
                let input = args.next().unwrap_or_default();
                let output = args.next().unwrap_or_default();
                let fps = args.next().map(|s| s.parse().unwrap_or(0)).unwrap_or(60);
                cfg.export_cfr = Some((input, output, fps));
            }
            "--upscale-notice" => {
                let mut n = || args.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                let (iw, ih, ow, oh) = (n(), n(), n(), n());
                if iw == 0 || ih == 0 || ow == 0 || oh == 0 {
                    return Args::Bad("--upscale-notice takes IN_W IN_H OUT_W OUT_H".into());
                }
                cfg.upscale_notice = Some(((iw, ih), (ow, oh)));
            }
            "--monitor" => cfg.monitor = args.next().and_then(|v| v.parse().ok()),
            "--help" | "-h" | "/?" | "-?" => return Args::Help,
            other => return Args::Bad(format!("unknown argument '{other}'")),
        }
    }
    Args::Run(cfg)
}

#[cfg(test)]
mod args_tests {
    use super::{parse_args_from, Args};

    #[test]
    fn the_slot_held_exit_code_is_its_own() {
        // R7-overlay-3: abort() exits 3; the controller must not read a crash as "slot held".
        assert_eq!(crate::EXIT_ALREADY_RUNNING, 86);
    }

    fn parse(args: &[&str]) -> Args {
        parse_args_from(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn the_commands_scripts_and_the_app_send_still_parse() {
        assert!(matches!(parse(&[]), Args::Run(_)));
        assert!(matches!(parse(&["--json"]), Args::Run(c) if c.json));
        assert!(matches!(parse(&["--export-edit", "in.mp4", "out.mp4", "60", "p.json", "--json"]), Args::Run(c) if c.json && c.export_edit.is_some()));
        assert!(matches!(parse(&["--prepare-editor", "in.mp4", "cache"]), Args::Run(c) if c.prepare_editor.is_some()));
        assert!(matches!(parse(&["--upscale-notice", "1920", "1080", "2560", "1440"]), Args::Run(c) if c.upscale_notice == Some(((1920, 1080), (2560, 1440)))));
        assert!(matches!(parse(&["--upscale-notice", "1920"]), Args::Bad(_)), "a short notice request never starts a recorder");
        assert!(matches!(parse(&["--export-cfr", "in.mp4", "out.mp4", "30"]), Args::Run(c) if c.export_cfr.is_some()));
        assert!(matches!(parse(&["--gpu-smoke", "dir"]), Args::Run(c) if c.gpu_smoke.is_some()));
        assert!(matches!(parse(&["--monitor", "1"]), Args::Run(c) if c.monitor == Some(1)));
    }

    #[test]
    fn help_and_unknown_arguments_never_reach_the_recorder() {
        for flag in ["--help", "-h", "/?", "-?"] {
            assert!(matches!(parse(&[flag]), Args::Help), "{flag}");
        }
        assert!(matches!(parse(&["--json", "--help"]), Args::Help));
        assert!(matches!(parse(&["--recrod"]), Args::Bad(why) if why.contains("--recrod")));
        assert!(matches!(parse(&["stray"]), Args::Bad(_)));
    }
}
