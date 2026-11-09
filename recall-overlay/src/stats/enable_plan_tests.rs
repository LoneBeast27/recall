use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;

use super::*;
use super::super::present_events::PROVIDERS;

const APEX: u32 = 27816;
const VALORANT: u32 = 39944;

/// Answers each call from a rule and records it ("+DXGI@42", "+DXGI@-", "-DXGI").
struct Fake<F: Fn(&str, Option<u32>) -> WIN32_ERROR> {
    answer: F,
    calls: Vec<String>,
}

impl<F: Fn(&str, Option<u32>) -> WIN32_ERROR> Enabler for Fake<F> {
    fn enable(&mut self, spec: &ProviderSpec, pid: Option<u32>) -> WIN32_ERROR {
        let to = pid.map_or("-".to_owned(), |pid| pid.to_string());
        self.calls.push(format!("+{}@{to}", spec.name));
        (self.answer)(spec.name, pid)
    }

    fn disable(&mut self, spec: &ProviderSpec) {
        self.calls.push(format!("-{}", spec.name));
    }
}

fn fake<F: Fn(&str, Option<u32>) -> WIN32_ERROR>(answer: F) -> Fake<F> {
    Fake { answer, calls: Vec::new() }
}

/// Apex: every PID-filtered enable for its pid is access denied.
fn apex_refuses(_: &str, pid: Option<u32>) -> WIN32_ERROR {
    if pid == Some(APEX) {
        ERROR_ACCESS_DENIED
    } else {
        ERROR_SUCCESS
    }
}

static ALL: [ProviderSpec; 3] = PROVIDERS;

fn all() -> Vec<&'static ProviderSpec> {
    ALL.iter().collect()
}

#[test]
fn a_game_that_accepts_the_filter_gets_filtered_user_mode_providers() {
    let mut calls = fake(|_, _| ERROR_SUCCESS);
    let aimed = enable_for(&all(), VALORANT, true, &mut calls);
    assert_eq!(calls.calls, ["+DXGI@39944", "+D3D9@39944", "+DxgKrnl@-"]);
    assert_eq!(aimed, Aimed::new(true));
    assert!(problems(&aimed, VALORANT).is_empty());
    assert!(outcome(&aimed).is_ok());
    assert_eq!(filter_note(&aimed, false), "");
}

#[test]
fn an_access_denied_filter_goes_unfiltered_and_is_not_no_permission() {
    let mut calls = fake(apex_refuses);
    let aimed = enable_for(&all(), APEX, true, &mut calls);
    assert_eq!(calls.calls, ["+DXGI@27816", "+DXGI@-", "+D3D9@-", "+DxgKrnl@-"]);
    assert_eq!(aimed.refused_by, Some("DXGI"));
    assert!(aimed.filter_ok, "a per-game refusal keeps the filter for the next game");
    assert!(outcome(&aimed).is_ok(), "Apex counts frames instead of showing No frame timing");
    assert_eq!(filter_note(&aimed, false), " (unfiltered: DXGI refused the PID filter for this game)");
}

#[test]
fn providers_enabled_filtered_before_a_refusal_are_re_enabled_unfiltered() {
    let mut calls = fake(|name, pid| if name == "D3D9" && pid.is_some() { ERROR_ACCESS_DENIED } else { ERROR_SUCCESS });
    let aimed = enable_for(&all(), APEX, true, &mut calls);
    assert_eq!(calls.calls, ["+DXGI@27816", "+D3D9@27816", "-DXGI", "+DXGI@-", "+D3D9@-", "+DxgKrnl@-"]);
    assert_eq!(aimed.refused_by, Some("D3D9"));
}

#[test]
fn other_filter_errors_keep_the_old_behaviour_unfiltered_for_good() {
    let not_supported = |_: &str, pid: Option<u32>| if pid.is_some() { ERROR_INVALID_PARAMETER } else { ERROR_SUCCESS };
    let mut calls = fake(not_supported);
    let aimed = enable_for(&all(), VALORANT, true, &mut calls);
    assert_eq!(calls.calls, ["+DXGI@39944", "+DXGI@-", "+D3D9@-", "+DxgKrnl@-"]);
    assert_eq!((aimed.refused_by, aimed.filter_ok), (None, false));
    assert_eq!(filter_note(&aimed, false), " (unfiltered: no PID filter available)");
    assert_eq!(aimed.lost_by, Some(("DXGI", ERROR_INVALID_PARAMETER)));
    let lost = "PID filter unavailable (DXGI error 87); unfiltered for the rest of the session";
    assert_eq!(problems(&aimed, VALORANT), [lost]);
    let mut again = fake(not_supported);
    assert_eq!(retarget(&all(), APEX, aimed.filter_ok, &mut again), None, "unfiltered already covers every pid");
    assert!(again.calls.is_empty());
}

#[test]
fn a_switch_away_from_apex_asks_filtered_again() {
    let mut calls = fake(apex_refuses);
    let apex = enable_for(&all(), APEX, true, &mut calls);
    let mut calls = fake(apex_refuses);
    let valorant = retarget(&all(), VALORANT, apex.filter_ok, &mut calls).expect("re-aimed");
    assert_eq!(calls.calls, ["-DXGI", "-D3D9", "+DXGI@39944", "+D3D9@39944"], "the kernel provider is left alone");
    assert_eq!((valorant.refused_by, valorant.filter_ok), (None, true));
}

#[test]
fn a_direct_switch_into_apex_falls_back_for_apex_only() {
    let mut calls = fake(apex_refuses);
    let apex = retarget(&all(), APEX, true, &mut calls).expect("re-aimed");
    assert_eq!(calls.calls, ["-DXGI", "-D3D9", "+DXGI@27816", "+DXGI@-", "+D3D9@-"]);
    assert_eq!(apex.refused_by, Some("DXGI"));
    let mut calls = fake(apex_refuses);
    let back = retarget(&all(), VALORANT, apex.filter_ok, &mut calls).expect("re-aimed");
    assert_eq!(back.refused_by, None);
    assert_eq!(calls.calls[2..], ["+DXGI@39944", "+D3D9@39944"]);
}

#[test]
fn the_dev_switch_starts_unfiltered_and_says_so() {
    let mut calls = fake(|_, _| ERROR_SUCCESS);
    let aimed = enable_for(&all(), VALORANT, false, &mut calls);
    assert_eq!(calls.calls, ["+DXGI@-", "+D3D9@-", "+DxgKrnl@-"]);
    assert_eq!(filter_note(&aimed, true), " (unfiltered: RECALL_ETW_NO_PID_FILTER=1)");
}

#[test]
fn either_dxgi_or_the_kernel_fallback_is_enough() {
    let aimed = |dxgi, kernel| Aimed { dxgi, kernel, ..Aimed::new(true) };
    assert!(outcome(&aimed(ERROR_ACCESS_DENIED, ERROR_SUCCESS)).is_ok(), "kernel presents still count");
    assert!(outcome(&aimed(ERROR_SUCCESS, ERROR_ACCESS_DENIED)).is_ok());
    assert!(matches!(outcome(&aimed(ERROR_ACCESS_DENIED, ERROR_ACCESS_DENIED)), Err(StartError::Denied)));
    assert!(matches!(outcome(&aimed(ERROR_INVALID_PARAMETER, ERROR_ACCESS_DENIED)), Err(StartError::Failed(_))));
}

#[test]
fn providers_that_fail_even_unfiltered_are_logged_each_time() {
    let d3d9_missing = |name: &str, _: Option<u32>| {
        if name == "D3D9" {
            ERROR_INVALID_PARAMETER
        } else {
            ERROR_SUCCESS
        }
    };
    let mut calls = fake(d3d9_missing);
    let aimed = enable_for(&all(), VALORANT, true, &mut calls);
    assert_eq!(aimed.failed, [("D3D9", ERROR_INVALID_PARAMETER)]);
    assert_eq!(problems(&aimed, VALORANT)[1..], ["D3D9 not enabled for pid 39944 (error 87)"]);
    let mut calls = fake(apex_refuses);
    let kernel_off = |name: &str, pid: Option<u32>| {
        if name == "DxgKrnl" {
            ERROR_ACCESS_DENIED
        } else {
            apex_refuses(name, pid)
        }
    };
    let mut calls2 = fake(kernel_off);
    let apex = enable_for(&all(), APEX, true, &mut calls2);
    assert_eq!(problems(&apex, APEX), ["DxgKrnl not enabled for pid 27816 (error 5)"]);
    assert!(retarget(&all(), APEX, true, &mut calls).expect("re-aimed").failed.is_empty());
}
