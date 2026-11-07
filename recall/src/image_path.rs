//! A process's executable path (`QueryFullProcessImageNameW`), long paths included.
//!
//! The first try uses a `MAX_PATH` buffer; a game installed deeper than that
//! (`ERROR_INSUFFICIENT_BUFFER`) is asked again with the longest Win32 path
//! (32 768 chars), so it stays identifiable.

use windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::PWSTR;

/// The first buffer (`MAX_PATH`).
pub const SHORT_PATH: usize = 260;
/// The longest Win32 path, the one retry.
pub const LONG_PATH: usize = 32_768;

/// One query: fills `buf`, sets `len` to the chars written. `Err(code)` carries
/// the Win32 error.
pub type Query<'a> = dyn FnMut(&mut [u16], &mut u32) -> Result<(), u32> + 'a;

/// The path `query` reports: a [`SHORT_PATH`] buffer first, once more with
/// [`LONG_PATH`] when that one was too small.
pub fn image_path_with(query: &mut Query) -> Option<String> {
    let mut size = SHORT_PATH;
    loop {
        let mut buf = vec![0u16; size];
        let mut len = buf.len() as u32;
        match query(&mut buf, &mut len) {
            Ok(()) => return Some(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())])),
            Err(code) if code == ERROR_INSUFFICIENT_BUFFER.0 && size < LONG_PATH => size = LONG_PATH,
            Err(_) => return None,
        }
    }
}

/// The full path of `pid`'s executable image (Win32 form).
pub fn exe_path(pid: u32) -> Option<String> {
    // SAFETY: a query-only handle, closed before return; each buffer outlives its call.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let path = image_path_with(&mut |buf, len| query_handle(handle, buf, len));
        let _ = CloseHandle(handle);
        path
    }
}

fn query_handle(handle: HANDLE, buf: &mut [u16], len: &mut u32) -> Result<(), u32> {
    // SAFETY: `buf` is `*len` chars long (set by `image_path_with`) and outlives the call.
    unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), len) }
        .map_err(|e| if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() { ERROR_INSUFFICIENT_BUFFER.0 } else { e.code().0 as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `path` when the buffer holds it and its NUL, else the API's
    /// insufficient-buffer error.
    fn fake(path: &str) -> impl FnMut(&mut [u16], &mut u32) -> Result<(), u32> + '_ {
        move |buf, len| {
            let wide: Vec<u16> = path.encode_utf16().collect();
            if (*len as usize) < wide.len() + 1 {
                return Err(ERROR_INSUFFICIENT_BUFFER.0);
            }
            buf[..wide.len()].copy_from_slice(&wide);
            *len = wide.len() as u32;
            Ok(())
        }
    }

    #[test]
    fn a_path_longer_than_max_path_is_read_in_full() {
        let long = format!("D:\\{}\\Game.exe", "g".repeat(390));
        assert_eq!(long.len(), 402);
        assert_eq!(image_path_with(&mut fake(&long)).as_deref(), Some(long.as_str()));
    }

    #[test]
    fn a_short_path_needs_one_query() {
        let mut calls = 0;
        let mut inner = fake("C:\\Games\\Game.exe");
        let mut counting = |buf: &mut [u16], len: &mut u32| {
            calls += 1;
            inner(buf, len)
        };
        assert_eq!(image_path_with(&mut counting).as_deref(), Some("C:\\Games\\Game.exe"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn other_errors_are_not_retried() {
        let mut calls = 0;
        let mut denied = |_: &mut [u16], _: &mut u32| {
            calls += 1;
            Err(5u32)
        };
        assert_eq!(image_path_with(&mut denied), None);
        assert_eq!(calls, 1);
    }

    #[test]
    fn this_process_reads_its_own_path() {
        let path = exe_path(std::process::id()).expect("own image path");
        assert!(path.to_ascii_lowercase().ends_with(".exe"), "{path}");
    }
}
