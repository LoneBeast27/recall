//! Application bootstrap: fonts, icon assets, the custom-titlebar window, the root view.
use std::borrow::Cow;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions,
    px, size,
};

use crate::{assets::Assets, offscreen, root::Root};

/// `instance` is the single-instance guard; it is let go when the window closes, so a new start
/// opens a window while this process only finishes a running export.
pub fn run(instance: Option<crate::instance::Guard>) -> Result<(), String> {
    let offscreen = offscreen::enabled();
    if offscreen {
        offscreen::quiet_errors();
    }
    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        if let Err(error) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/geist-400.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/geist-500.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/geist-600.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/bricolage-grotesque-600.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/bricolage-grotesque-700.ttf")),
        ]) {
            eprintln!("load Recall fonts: {error}");
        }
        let background = offscreen || std::env::var_os("RECALL_NATIVE_BACKGROUND").is_some();
        let (width, height) = window_size();
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Recall".into()), appears_transparent: true, traffic_light_position: None }),
            window_min_size: Some(size(px(1000.), px(660.))),
            // GPUI's DirectComposition tree is drawn ABOVE child HWNDs; its swap chain keeps
            // per-pixel alpha, so Root paints everything except a hole over the native video.
            // Opaque only stops the desktop showing through not-yet-painted areas while resizing.
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some("Recall".into()),
            focus: !background,
            show: !background,
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let root = cx.new(|cx| Root::new(window, cx));
            Root::start_polling(root.clone(), window, cx);
            let closing = root.downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                let _ = closing.update(cx, |root, _| root.flush_on_close());
                true
            });
            root
        });
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("open Recall window: {error}");
                cx.quit();
                return;
            }
        };
        if background {
            let show = if offscreen { offscreen::show } else { show_behind };
            let _ = handle.update(cx, |_, window, _| show(window, width, height));
        }
        if !background {
            cx.activate(true);
        }
    });
    // The window is gone: a new start may open one now. Helpers end with this process, so a
    // running export finishes first.
    drop(instance);
    crate::backend::wait_for_export();
    crate::services::storage::close_helpers();
    Ok(())
}

/// Scripted checks (`RECALL_NATIVE_BACKGROUND`): show the window at the bottom of the z-order
/// without activating it, so a test instance never takes focus from the person at the PC.
fn show_behind(window: &gpui::Window, width: f32, height: f32) {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{HWND_BOTTOM, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos},
    };
    let Ok(raw) = crate::root::parent_hwnd(window) else { return };
    let s = window.scale_factor();
    let (w, h) = ((width * s).round() as i32, (height * s).round() as i32);
    let _ = unsafe { SetWindowPos(HWND(raw as *mut _), Some(HWND_BOTTOM), 24, 24, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW) };
}

/// `RECALL_NATIVE_WINDOW_SIZE=1120x720` for layout checks; default 1280×820.
fn window_size() -> (f32, f32) {
    std::env::var("RECALL_NATIVE_WINDOW_SIZE")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
        })
        .filter(|(w, h)| *w >= 1000. && *h >= 660.)
        .unwrap_or((1280., 820.))
}
