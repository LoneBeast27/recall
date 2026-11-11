//! The tallest connected monitor in physical pixels: the highest native size a game can record
//! at here, so Settings › Recording › Resolution offers no choice above it.
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::core::BOOL;

/// Height of the tallest monitor (physical px), None when none could be read.
pub fn tallest_monitor() -> Option<u32> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        let tallest = unsafe { &mut *(out.0 as *mut u32) };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            let height = (info.rcMonitor.bottom - info.rcMonitor.top).max(0) as u32;
            *tallest = (*tallest).max(height);
        }
        true.into()
    }
    let mut tallest = 0u32;
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut tallest as *mut u32 as isize)) };
    (tallest > 0).then_some(tallest)
}

/// The `heights` (0 = native) offered under a `native` height: native always, and only caps
/// below it, since a cap at or above native records native anyway. Unknown native: all.
pub fn offered<T: Copy>(heights: &[(u32, T)], native: Option<u32>) -> Vec<(u32, T)> {
    heights.iter().copied().filter(|&(h, _)| h == 0 || native.is_none_or(|n| h < n)).collect()
}

#[cfg(test)]
mod tests {
    use super::offered;

    const H: [(u32, &str); 4] = [(0, "Native"), (1440, "1440p"), (1080, "1080p"), (720, "720p")];

    #[test]
    fn choices_above_native_are_not_offered() {
        let labels = |n| offered(&H, n).into_iter().map(|(_, l)| l).collect::<Vec<_>>();
        assert_eq!(labels(Some(2160)), ["Native", "1440p", "1080p", "720p"]);
        assert_eq!(labels(Some(1440)), ["Native", "1080p", "720p"], "1440p on a 1440p monitor is native");
        assert_eq!(labels(Some(1200)), ["Native", "1080p", "720p"], "a 16:10 laptop");
        assert_eq!(labels(Some(900)), ["Native", "720p"]);
        assert_eq!(labels(Some(720)), ["Native"]);
        assert_eq!(labels(None), ["Native", "1440p", "1080p", "720p"]);
    }
}
