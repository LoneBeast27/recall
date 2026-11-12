//! The Java command line of a foreground window's process, read only for a
//! Java game window, so a Minecraft window is named "Minecraft" whatever its
//! title (`known_games::is_minecraft_java`).

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(process: isize, class: u32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

/// `ProcessCommandLineInformation` (Windows 8.1+): a `UNICODE_STRING` followed
/// by its text. A query-limited handle is enough.
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
/// Longest command line read; Windows caps one at 32 767 chars.
const MAX_BYTES: u32 = 64 * 1024 + 64;

/// `UNICODE_STRING`: byte length, capacity, pointer into the same buffer.
#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum: u16,
    buffer: *const u16,
}

/// The window is Minecraft Java Edition: a Java known-game window
/// (`known_games::is_java_known_game`) whose command line names Minecraft,
/// or whose title does.
pub(super) fn is_minecraft_window(pid: u32, stem: &str, title: &str, class: &str) -> bool {
    if !crate::known_games::is_java_known_game(stem, title, class) {
        return false;
    }
    let command = command_line(pid);
    crate::known_games::trace_java(title, class, command.as_deref());
    crate::known_games::is_minecraft_java(title, command.as_deref())
}

/// `pid`'s command line, or `None` when the process refuses the query.
fn command_line(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut buf = vec![0u64; (MAX_BYTES as usize).div_ceil(8)];
    // SAFETY: a query-only handle, closed before return; the buffer (u64 words, so the
    // UNICODE_STRING header is aligned) holds `MAX_BYTES` and outlives the call.
    let status = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut ret = 0u32;
        let status = NtQueryInformationProcess(handle.0 as isize, PROCESS_COMMAND_LINE_INFORMATION, buf.as_mut_ptr().cast(), MAX_BYTES, &mut ret);
        let _ = CloseHandle(handle);
        status
    };
    if status < 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with a UNICODE_STRING; its text is used only
    // when it lies inside the same buffer.
    unsafe {
        let header = &*(buf.as_ptr() as *const UnicodeString);
        let start = buf.as_ptr() as usize;
        let end = start + buf.len() * 8;
        let text = header.buffer as usize;
        let chars = header.length as usize / 2;
        if header.buffer.is_null() || text < start || text + chars * 2 > end {
            return None;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(header.buffer, chars)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_a_live_process_command_line() {
        let own = super::command_line(std::process::id()).expect("own command line");
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy();
        assert!(own.contains(stem.as_ref()), "{own}");
        assert!(super::command_line(0).is_none(), "the idle process refuses");
    }
}
