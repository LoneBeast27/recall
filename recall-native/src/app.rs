//! Application bootstrap: fonts, icon assets, the custom-titlebar window, the root view.
use std::borrow::Cow;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions,
    px, size,
};

use crate::{assets::Assets, root::Root};

pub fn run() -> Result<(), String> {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        if let Err(error) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Medium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/Lilex-Regular.ttf")),
        ]) {
            eprintln!("load Recall fonts: {error}");
        }
        let background = std::env::var_os("RECALL_NATIVE_BACKGROUND").is_some();
        let (width, height) = window_size();
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Recall".into()), appears_transparent: true, traffic_light_position: None }),
            window_min_size: Some(size(px(1000.), px(660.))),
            // GPUI's DirectComposition tree is drawn ABOVE child HWNDs, so the window is
            // transparent and Root paints everything except a hole over the native video.
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("Recall".into()),
            focus: !background,
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let root = cx.new(|cx| Root::new(window, cx));
            Root::start_polling(root.clone(), window, cx);
            root
        });
        if let Err(error) = opened {
            eprintln!("open Recall window: {error}");
            cx.quit();
            return;
        }
        if !background {
            cx.activate(true);
        }
    });
    Ok(())
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
