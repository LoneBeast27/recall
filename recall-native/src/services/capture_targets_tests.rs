use super::*;

fn window(hwnd: isize, exe: &str, title: &str) -> WindowCard {
    WindowCard { hwnd, pid: hwnd as u32, title: title.into(), exe: exe.into(), path: String::new() }
}

fn monitor(device: &str, rect: [i32; 4], primary: bool) -> MonitorCard {
    MonitorCard { device: device.into(), rect, primary }
}

/// Three monitors: a 2560x1440 main, a portrait 1440x2560 on its left, a 1920x1080 on its right.
fn three() -> Vec<MonitorCard> {
    vec![
        monitor(r"\\.\DISPLAY1", [0, 0, 2560, 1440], true),
        monitor(r"\\.\DISPLAY2", [-1440, -560, 0, 2000], false),
        monitor(r"\\.\DISPLAY3", [2560, 0, 4480, 1080], false),
    ]
}

#[test]
fn a_card_names_its_window_and_its_monitor() {
    assert_eq!(window(1, "editor.exe", "notes.txt - Editor").label(), "notes.txt - Editor");
    assert_eq!(window(1, "editor.exe", "  ").label(), "editor", "no title: the exe");
    let m = &three();
    assert_eq!((m[0].name(), m[0].detail()), ("Display 1".to_string(), "2560 × 1440 · Main".to_string()));
    assert_eq!((m[1].name(), m[1].detail()), ("Display 2".to_string(), "1440 × 2560".to_string()), "portrait reads as portrait");
}

#[test]
fn the_followed_game_comes_first_and_starts_selected() {
    let windows = order_windows(vec![window(1, "browser.exe", "Docs"), window(2, "game.exe", "Game")], Some(2));
    assert_eq!(windows[0].hwnd, 2);
    let last = LastPick { kind: "monitor".into(), key: r"\\.\DISPLAY3".into() };
    assert_eq!(preselect(&windows, &three(), Some(&last), Some(2)), (Tab::Applications, Some(Choice::Window(2))));
}

#[test]
fn with_no_game_the_last_pick_starts_selected() {
    let windows = vec![window(5, "browser.exe", "Docs"), window(6, "chat.exe", "Chat")];
    let screen = LastPick { kind: "monitor".into(), key: r"\\.\display3".into() };
    assert_eq!(preselect(&windows, &three(), Some(&screen), None), (Tab::Screens, Some(Choice::Monitor(r"\\.\DISPLAY3".into()))));
    let app = LastPick { kind: "window".into(), key: "CHAT.exe".into() };
    assert_eq!(preselect(&windows, &three(), Some(&app), None), (Tab::Applications, Some(Choice::Window(6))));
    let gone = LastPick { kind: "monitor".into(), key: r"\\.\DISPLAY9".into() };
    assert_eq!(preselect(&windows, &three(), Some(&gone), None), (Tab::Screens, Some(Choice::Monitor(r"\\.\DISPLAY1".into()))), "else the main monitor");
    assert_eq!(LastPick::of(&Choice::Window(6), &windows), Some(app_key("chat.exe")));
}

fn app_key(exe: &str) -> LastPick {
    LastPick { kind: "window".into(), key: exe.into() }
}

#[test]
fn the_screen_layout_keeps_each_monitor_in_place_and_shape() {
    let rects = layout_rects(&three(), 300., 170., 10.);
    let [main, left, right] = [rects[0], rects[1], rects[2]];
    assert!(left[2] <= main[0] + 0.01 && main[2] <= right[0] + 0.01, "left to right as on the desk");
    assert!((left[3] - left[1]) > (left[2] - left[0]), "the portrait monitor stays tall");
    for r in &rects {
        assert!(r[0] >= 9.99 && r[1] >= 9.99 && r[2] <= 290.01 && r[3] <= 160.01, "inside the padded box: {r:?}");
    }
}

#[test]
fn a_preview_fits_its_card_and_is_cut_to_the_scroll_area() {
    assert_eq!(fit(1920, 1080, [0, 0, 400, 400]), [0, 87, 400, 312], "letterboxed");
    assert_eq!(fit(0, 0, [1, 2, 3, 4]), [1, 2, 3, 4], "no size yet: the card");
    let (shown, source) = clip([0, 100, 400, 325], 1920, 1080, [0, 200, 1000, 1000]).unwrap();
    assert_eq!(shown, [0, 200, 400, 325]);
    assert_eq!(source, [0, 480, 1920, 1080], "the bottom of the source");
    assert!(clip([0, 0, 10, 10], 100, 100, [0, 20, 100, 100]).is_none(), "scrolled out of view");
}

#[test]
fn the_clip_line_follows_the_resolution_rule() {
    assert_eq!(clip_line(0, 60), "Size follows what you pick · 60 fps");
    assert_eq!(clip_line(1080, 120), "1080p · 120 fps");
}
