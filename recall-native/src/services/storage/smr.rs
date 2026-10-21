//! Drive-managed SMR model table. These drives report themselves as ordinary HDDs (no zone
//! information reaches the OS), so the model number is the only signal.
//!
//! Sources (2020 disclosures after the WD Red SMR controversy):
//! * Seagate: Blocks & Files, 2020-04-15, "Seagate 2, 4 and 8TB Barracuda and Desktop HDD
//!   SMR" (ST2000DM008, ST4000DM004, ST8000DM004, Desktop HDD ST5000DM000, BarraCuda 2.5"
//!   ST500LM030 / ST1000LM048 / ST2000LM015 / ST3000LM024 / ST4000LM024 / ST5000LM000);
//!   seagate.com/products/cmr-smr-list (BarraCuda 3.5" 4 TB / 8 TB, BarraCuda 2.5" 2 / 4 TB,
//!   FireCuda 2.5" 1 / 2 TB = SMR); Seagate Archive v2 datasheet (ST5000AS0011,
//!   ST6000AS0002, ST8000AS0002 state SMR outright).
//! * Western Digital: WD statement and list, 2020-04 / 2020-05 (WD Red 2-6 TB `EFAX`; WD Blue
//!   3.5" WD20EZAZ / WD60EZAZ; WD Blue 2.5" WD10SPZX / WD20SPZX; WD Black 2.5" WD10SPSX).
//! * Toshiba: Toshiba statement, 2020-04 (P300 HDWD240 / HDWD260, DT02 series, L200
//!   HDWL110 / HDWL120, MQ04 1 / 2 TB).
//!
//! `ST…LM…` (Seagate 2.5" mobile) is treated as SMR as a family except the known CMR models
//! below; a false positive only changes the label and is harmless.

/// Model prefixes, matched against each whitespace token of the model string.
const SMR_PREFIXES: &[&str] = &[
    // Seagate BarraCuda / Desktop HDD 3.5"
    "ST2000DM005", "ST2000DM008", "ST3000DM007", "ST4000DM004", "ST5000DM000", "ST6000DM003", "ST8000DM004",
    // Seagate Archive v2
    "ST5000AS0011", "ST6000AS0002", "ST8000AS0002",
    // Seagate FireCuda 2.5" SSHD
    "ST1000LX015", "ST2000LX001",
    // WD Red (non-Plus) 2-6 TB
    "WD20EFAX", "WD30EFAX", "WD40EFAX", "WD60EFAX",
    // WD Blue 3.5" / 2.5", WD Black 2.5"
    "WD20EZAZ", "WD60EZAZ", "WD10SPZX", "WD20SPZX", "WD10SPSX",
    // Toshiba P300 4/6 TB, DT02, L200, MQ04
    "HDWD240", "HDWD260", "DT02ABA", "HDWL110", "HDWL120", "MQ04ABF100", "MQ04ABD200",
];

/// Seagate 2.5" models that are CMR despite the `LM` family code.
const SEAGATE_LM_CMR: &[&str] = &["ST1000LM049", "ST500LM021", "ST1000LM014"];

/// True when `model` (as the storage descriptor reports it, e.g. "ST4000DM004-2CV104" or
/// "WDC WD40EFAX-68JH4N0") is a known drive-managed SMR drive.
pub(super) fn is_drive_managed_smr(model: &str) -> bool {
    model.split_whitespace().map(str::to_ascii_uppercase).any(|token| {
        if SMR_PREFIXES.iter().any(|prefix| token.starts_with(prefix)) {
            return true;
        }
        seagate_mobile(&token) && !SEAGATE_LM_CMR.iter().any(|cmr| token.starts_with(cmr))
    })
}

/// `ST<digits>LM<digits>` (Seagate 2.5" mobile family code).
fn seagate_mobile(token: &str) -> bool {
    let Some(rest) = token.strip_prefix("ST") else { return false };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with("LM") && rest[digits + 2..].starts_with(|c: char| c.is_ascii_digit())
}
