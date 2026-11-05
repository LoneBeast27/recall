//! The experimental HDR display manager is shelved (Rohan, 2026-11-04: "experimental and
//! not working yet, we shelve till we have the solution ironed out").
//!
//! While shelved, the recorder never touches a display setting and never probes the
//! managed monitor: no HDR switching (even with `behavior.hdr_auto = true` in config),
//! no manual HDR toggle or its hotkey, no Advanced Color profile writes, no NvAPI
//! clamp test and no `--legacy-hdr` tray. The code stays compiled and its tests keep
//! running, so it can come back without a rewrite.
//!
//! Not shelved: the engine still tone maps an HDR desktop into SDR clips (that is how
//! recording stays correct when Windows HDR is on), and the `--restore` / startup
//! journal repair still puts a display back that an older build left switched.
//!
//! To try the feature on a dev machine, start the recorder with
//! `RECALL_EXPERIMENTAL_HDR=1` (the app reads the same variable to show its page).

use std::sync::OnceLock;

/// Environment variable that re-enables the shelved HDR features.
pub const ENV: &str = "RECALL_EXPERIMENTAL_HDR";

/// What a refused HDR command says.
pub const SHELVED_MSG: &str = "HDR switching is shelved while it is experimental (set RECALL_EXPERIMENTAL_HDR=1 to try it)";

/// True when the shelved HDR features are switched back on for this process.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| parse(std::env::var(ENV).ok().as_deref()))
}

fn parse(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1" | "true" | "on" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn hdr_is_shelved_unless_explicitly_enabled() {
        assert!(!parse(None), "unset keeps HDR shelved");
        assert!(!parse(Some("")));
        assert!(!parse(Some("0")));
        assert!(!parse(Some("false")));
        assert!(parse(Some("1")));
        assert!(parse(Some(" true ")));
    }
}
