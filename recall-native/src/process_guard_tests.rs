//! Process guard tests. The live ones run the guard in a child copy of this test binary
//! (`guard_child`, started with RECALL_GUARD_CHILD set), so the list never lands on the test
//! runner itself, then read the child's list back and open it the way another program would.
//! No windows, no audio, no Recall data.
use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE};
use windows::Win32::Security::Authorization::GetSecurityInfo;
use windows::Win32::Security::{ACCESS_ALLOWED_ACE, ACE_HEADER, GetAce, PSID};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_CREATE_THREAD, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, PROCESS_VM_OPERATION, PROCESS_VM_WRITE,
};

const CHILD_ENV: &str = "RECALL_GUARD_CHILD";
const READY: &str = "guard-child ready";

#[test]
fn rights_leave_out_everything_injection_needs() {
    // PROCESS_CREATE_THREAD, VM_OPERATION, VM_WRITE, DUP_HANDLE, SET_INFORMATION, WRITE_DAC, WRITE_OWNER.
    for denied in [0x2u32, 0x8, 0x20, 0x40, 0x200, 0x4_0000, 0x8_0000] {
        assert_eq!(USER_RIGHTS & denied, 0, "user rights include 0x{denied:x}");
        assert_eq!(OTHER_RIGHTS & denied, 0, "other rights include 0x{denied:x}");
    }
    for kept in [0x1u32, 0x1000, 0x10_0000] {
        assert_ne!(USER_RIGHTS & kept, 0, "user rights miss 0x{kept:x}");
    }
}

#[test]
fn sddl_names_the_user_system_and_everyone() {
    assert_eq!(sddl("S-1-5-21-1"), "D:P(A;;0x121401;;;S-1-5-21-1)(A;;GA;;;SY)(A;;0x101000;;;WD)");
}

#[test]
fn reads_this_user() {
    assert!(user_sid().unwrap().starts_with("S-1-5-"));
}

/// Not a check of its own: the child the live tests start. It runs the start-up entry, says
/// so, and stays alive until the parent closes its stdin.
#[test]
fn guard_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let outcome = match start() {
        Outcome::Applied => "applied",
        Outcome::Off => "off",
        Outcome::Failed(_) => "failed",
    };
    println!("{READY} {outcome}");
    let _ = std::io::stdin().read_line(&mut String::new());
}

struct GuardChild {
    child: Child,
    outcome: String,
}

impl GuardChild {
    fn start(allow_injection: bool) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "process_guard::tests::guard_child", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if allow_injection {
            command.env("RECALL_ALLOW_INJECTION", "1");
        } else {
            command.env_remove("RECALL_ALLOW_INJECTION");
        }
        let mut child = command.spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let outcome = loop {
            let line = lines.next().expect("child ended before it was ready").unwrap();
            // libtest prints the test name on the same line first.
            if let Some(at) = line.find(READY) {
                break line[at + READY.len()..].trim().to_string();
            }
        };
        Self { child, outcome }
    }

    fn open(&self, rights: PROCESS_ACCESS_RIGHTS) -> windows::core::Result<HANDLE> {
        unsafe { OpenProcess(rights, false, self.child.id()) }
    }
}

impl Drop for GuardChild {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill(); // our own child only
        }
        let _ = self.child.wait();
    }
}

fn close(handle: HANDLE) {
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
}

/// The child's access list, as (SID string, mask) per allow entry.
fn read_list(child: &GuardChild) -> Vec<(String, u32)> {
    let handle = child.open(PROCESS_ACCESS_RIGHTS(0x2_0000)).expect("read access list"); // READ_CONTROL
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetSecurityInfo(handle, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(&mut dacl), None, Some(&mut descriptor))
    };
    close(handle);
    assert!(status.is_ok(), "GetSecurityInfo: {}", status.0);
    let count = unsafe { (*dacl).AceCount };
    let mut entries = Vec::new();
    for index in 0..count as u32 {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        unsafe { GetAce(dacl, index, &mut ace) }.unwrap();
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        assert_eq!(header.AceType, 0, "entry {index} is not an allow entry");
        let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
        let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
        let mut text = windows::core::PWSTR::null();
        unsafe { ConvertSidToStringSidW(sid, &mut text) }.unwrap();
        entries.push((unsafe { text.to_string() }.unwrap(), allowed.Mask));
        unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    }
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    entries
}

fn denied(child: &GuardChild, rights: PROCESS_ACCESS_RIGHTS, name: &str) {
    match child.open(rights) {
        Ok(handle) => {
            close(handle);
            panic!("{name} was allowed");
        }
        Err(error) => assert_eq!(error.code(), ERROR_ACCESS_DENIED.to_hresult(), "{name}: {error}"),
    }
}

#[test]
fn the_guard_sets_the_list_on_a_running_process() {
    let child = GuardChild::start(false);
    assert_eq!(child.outcome, "applied");
    let user = user_sid().unwrap();
    let list = read_list(&child);
    assert_eq!(list.len(), 3, "{list:?}");
    assert!(list.contains(&(user, USER_RIGHTS)), "{list:?}");
    assert!(list.contains(&("S-1-1-0".to_string(), OTHER_RIGHTS)), "{list:?}");
    assert!(list.iter().any(|(sid, _)| sid == "S-1-5-18"), "no SYSTEM entry: {list:?}");
}

#[test]
fn same_user_can_end_and_wait_but_not_inject() {
    let child = GuardChild::start(false);
    assert_eq!(child.outcome, "applied");
    let handle = child.open(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE).expect("terminate/query/wait");
    close(handle);
    if crate::process_guard::tests::elevated() {
        eprintln!("skipped the denied half: an elevated test runner holds SeDebugPrivilege");
        return;
    }
    denied(&child, PROCESS_VM_WRITE, "VM_WRITE");
    denied(&child, PROCESS_VM_OPERATION, "VM_OPERATION");
    denied(&child, PROCESS_CREATE_THREAD, "CREATE_THREAD");
    denied(&child, PROCESS_DUP_HANDLE, "DUP_HANDLE");
}

#[test]
fn allow_injection_leaves_the_list_alone() {
    let child = GuardChild::start(true);
    assert_eq!(child.outcome, "off");
    let handle = child.open(PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_CREATE_THREAD | PROCESS_DUP_HANDLE).expect("default list allows injection rights");
    close(handle);
    let user = user_sid().unwrap();
    assert!(!read_list(&child).contains(&(user, USER_RIGHTS)), "guard list applied anyway");
}

/// Whether this test runner runs elevated (then SeDebugPrivilege may open anything).
fn elevated() -> bool {
    use windows::Win32::Security::{TOKEN_ELEVATION, TokenElevation};
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    let ok = unsafe {
        GetTokenInformation(token, TokenElevation, Some((&raw mut elevation).cast()), size_of::<TOKEN_ELEVATION>() as u32, &mut size)
    }
    .is_ok();
    close(token);
    ok && elevation.TokenIsElevated != 0
}
