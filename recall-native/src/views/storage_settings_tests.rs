//! Settings -> Storage: typed sizes, the slider and the read-only size.
use super::*;

const G: u64 = 1_073_741_824;

#[test]
fn parses_typed_sizes() {
    assert_eq!(parse_gb("7"), Some(7.0));
    assert_eq!(parse_gb(" 7.5 "), Some(7.5));
    assert_eq!(parse_gb("7,5"), Some(7.5));
    assert_eq!(parse_gb(".5"), Some(0.5));
    assert_eq!(parse_gb(""), None);
    assert_eq!(parse_gb("0"), None);
    assert_eq!(parse_gb("."), None);
    assert_eq!(parse_gb("abc"), None);
    assert_eq!(parse_gb("inf"), None);
    assert_eq!(parse_gb("NaN"), None);
    assert_eq!(parse_gb("-5"), None);
    assert_eq!(parse_gb("1e999"), None);
}

#[test]
fn typed_sizes_round_and_hold_to_bounds() {
    let roomy = 500 * G;
    assert_eq!(typed_budget(7.0, roomy), (7 * G, false));
    assert_eq!(typed_budget(7.54, roomy), ((7.5 * GB).round() as u64, false));
    assert_eq!(typed_budget(0.5, roomy), (G, true));
    // 139.19 GB free: anything above holds at 139.1 GB.
    let max = (139.19 * GB) as u64;
    let (bytes, held) = typed_budget(200.0, max);
    assert!(held);
    assert_eq!(bytes, (139.1 * GB).round() as u64);
    assert!(bytes <= max);
    // A nearly full drive still allows the 1 GB floor.
    assert_eq!(typed_budget(5.0, G / 2), (G, true));
}

#[test]
fn knob_sits_between_stops() {
    assert_eq!(slider_fraction(G / 2), 0.0);
    assert_eq!(slider_fraction(G), 0.0);
    assert!((slider_fraction(2 * G) - 0.2).abs() < 1e-6);
    assert!((slider_fraction(10 * G) - 0.6).abs() < 1e-6);
    // 7.5 GB is halfway between the 5 and 10 GB ticks.
    assert!((slider_fraction((7.5 * GB) as u64) - 0.5).abs() < 1e-6);
    assert_eq!(slider_fraction(50 * G), 1.0);
    assert_eq!(slider_fraction(120 * G), 1.0);
}

#[test]
fn a_size_that_may_be_a_guess_is_never_saved() {
    // RI2-2: storage.json locked or damaged: the shown size is a guess, a stray click or drag
    // must not save it.
    let mut view = CacheView {
        root: PathBuf::new(),
        budget_bytes: 2 * G,
        budget_note: Some("storage.json could not be read".into()),
        budget_reset: false,
        leftovers: None,
        used_bytes: 0,
        tiers: Vec::new(),
        ssd_free: None,
        smart: String::new(),
        span: Rc::new(Cell::new(Some((0.0, 100.0)))),
        dragging: false,
        editing: false,
        painted: Cell::new(false),
    };
    assert!(!view.drag_to(100.0), "the knob does not move");
    assert_eq!(view.budget_bytes, 2 * G);
    assert!(!view.save_wanted(20 * G), "nothing is saved while the size is unknown");
    view.budget_note = None;
    assert!(view.save_wanted(20 * G), "a known size saves as before");
}

#[test]
fn labels_typed_sizes() {
    assert_eq!(gb_label(2 * G), "2 GB");
    assert_eq!(gb_label((7.5 * GB).round() as u64), "7.5 GB");
    assert_eq!(gb_label((139.1 * GB).round() as u64), "139.1 GB");
}
