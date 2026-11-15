//! The updater's HTTPS layer: WinHTTP in-process (no curl.exe), redirects followed by hand so
//! every hop is HTTPS to a pinned GitHub host, and a byte cap on every body.
use std::{
    io::Write,
    time::{Duration, Instant},
};

/// The only hosts the updater talks to: the API, release pages, and the asset CDNs that
/// GitHub redirects release downloads to.
pub(crate) const HOSTS: [&str; 4] = [
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const MAX_REDIRECTS: usize = 5;
/// The words for a request that ran out of time (a check and a download alike).
pub(crate) const TIMED_OUT: &str = "GitHub did not answer in time. Check your connection, then try again.";
/// The start of every connection-level failure (no answer, DNS, proxy, TLS).
pub(crate) const UNREACHABLE: &str = "Could not reach GitHub";
/// Per-step WinHTTP limits. The total limit is the caller's; a body read also stops after
/// `STALL` without a byte, so a slow but moving download is never cut off early.
const RESOLVE: Duration = Duration::from_secs(10);
/// WinHTTP's connect limit covers all of a host's addresses, while Windows gives up on one
/// address only after about 21 s (TCP retries). One of the release CDN's 4 addresses never
/// answered from a user's PC (11-15): a limit under 21 s failed every time it came first; this
/// one leaves room for Windows to move on to the next address.
const CONNECT: Duration = Duration::from_secs(30);
const STALL: Duration = Duration::from_secs(30);
/// The whole limit for one small request (API JSON, checksums): a dead first address
/// (~21 s) plus a normal answer, with room to spare.
pub(crate) const SMALL_TIMEOUT: Duration = Duration::from_secs(45);
/// Small requests (API JSON, checksums) try again once after a timeout, a connection failure
/// or a GitHub 5xx: the first try on a cold network also pays proxy discovery and DNS.
const ATTEMPTS: usize = 2;
#[cfg(not(test))]
const RETRY_PAUSE: Duration = Duration::from_millis(1500);
#[cfg(test)]
const RETRY_PAUSE: Duration = Duration::from_millis(1);

/// One response, headers only: the body already went to the sink (never for a redirect).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reply {
    pub status: u16,
    pub location: Option<String>,
}

pub(crate) fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// One HTTPS GET without following redirects. A redirect's body is not read; any other body
/// is written to `sink`, failing once it passes `limit` bytes or `deadline` passes.
pub(crate) trait Transport: Send + Sync {
    fn get(&self, host: &str, path: &str, limit: u64, deadline: Instant, sink: &mut dyn Write) -> Result<Reply, String>;
}

/// `https://<pinned host>/<path>` -> (host, path). No other scheme, user info or port.
pub(crate) fn pinned(url: &str) -> Result<(String, String), String> {
    let rest = url.strip_prefix("https://").ok_or("Updates use HTTPS only.")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority.strip_suffix(":443").unwrap_or(authority).to_ascii_lowercase();
    if !HOSTS.contains(&host.as_str()) {
        return Err(format!("Refused a download from {authority}: not a GitHub release host."));
    }
    let path = if path.is_empty() { "/" } else { path };
    if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Refused an invalid download address.".into());
    }
    Ok((host, path.into()))
}

/// GET `url`, following up to five redirects, each one re-checked against the pinned hosts.
pub(crate) fn fetch(net: &dyn Transport, url: &str, limit: u64, timeout: Duration, sink: &mut dyn Write) -> Result<u16, String> {
    let deadline = Instant::now() + timeout;
    let (mut host, mut path) = pinned(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let reply = net.get(&host, &path, limit, deadline, sink)?;
        if !is_redirect(reply.status) {
            return Ok(reply.status);
        }
        let next = reply.location.ok_or("A redirect named no address.")?;
        (host, path) = if next.starts_with('/') && !next.starts_with("//") { pinned(&format!("https://{host}{next}"))? } else { pinned(&next)? };
    }
    Err("Too many redirects.".into())
}

/// A failure worth one more try: no answer in time, or no connection.
pub(crate) fn transient(error: &str) -> bool {
    error == TIMED_OUT || error.starts_with(UNREACHABLE)
}

/// A small GET into memory ([`fetch`]), tried again once on a [`transient`] failure or a 5xx.
/// Each try gets the whole `timeout`; the body of a failed try is dropped.
pub(crate) fn fetch_small(net: &dyn Transport, url: &str, limit: u64, timeout: Duration) -> Result<(u16, Vec<u8>), String> {
    let mut last = Err(TIMED_OUT.to_owned());
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(RETRY_PAUSE);
        }
        let mut body = Vec::new();
        last = fetch(net, url, limit, timeout, &mut body).map(|status| (status, body));
        match &last {
            Ok((status, _)) if *status < 500 => break,
            Ok(_) => {}
            Err(e) if transient(e) => {}
            Err(_) => break,
        }
    }
    last
}

/// The real transport: WinHTTP with the system proxy, TLS 1.2+, certificate checks on.
/// One WinHTTP session for the whole process (`session`): proxy discovery, DNS and TLS
/// connections are reused across the check's requests instead of paid again for each one.
pub(crate) struct WinHttp;

/// The process-wide session handle (0: not opened yet; a failed open is tried again later).
static SESSION: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

fn session() -> Result<*mut core::ffi::c_void, String> {
    use windows::{
        Win32::Networking::WinHttp::*,
        core::{PCWSTR, w},
    };
    let mut held = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if *held != 0 {
        return Ok(*held as *mut _);
    }
    unsafe {
        let raw = WinHttpOpen(w!("Recall-updater"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if raw.is_null() {
            return Err(format!("{UNREACHABLE}: {}", windows::core::Error::from_thread().message()));
        }
        let tls = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        if WinHttpSetOption(Some(raw as *const _), WINHTTP_OPTION_SECURE_PROTOCOLS, Some(&tls.to_ne_bytes())).is_err() {
            // Windows 10 before 1903 has no TLS 1.3 in WinHTTP.
            if let Err(e) = WinHttpSetOption(Some(raw as *const _), WINHTTP_OPTION_SECURE_PROTOCOLS, Some(&WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2.to_ne_bytes())) {
                let _ = WinHttpCloseHandle(raw);
                return Err(format!("{UNREACHABLE}: {}", e.message()));
            }
        }
        // Kept for the life of the process (Windows closes it at exit).
        *held = raw as usize;
        Ok(raw)
    }
}

/// WinHTTP's timeout code (`ERROR_WINHTTP_TIMEOUT`, 12002) as an HRESULT.
const WINHTTP_TIMEOUT: i32 = 0x8007_2EE2_u32 as i32;

fn net_error(e: windows::core::Error) -> String {
    if e.code().0 == WINHTTP_TIMEOUT { TIMED_OUT.to_owned() } else { format!("{UNREACHABLE}: {}", e.message().trim_end()) }
}

fn ms(d: Duration) -> u32 {
    d.as_millis().clamp(1, i32::MAX as u128) as u32
}

struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(raw: *mut core::ffi::c_void, what: &str) -> Result<Self, String> {
        if raw.is_null() {
            Err(format!("{what} failed: {}", windows::core::Error::from_thread()))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0);
        }
    }
}

impl Transport for WinHttp {
    fn get(&self, host: &str, path: &str, limit: u64, deadline: Instant, sink: &mut dyn Write) -> Result<Reply, String> {
        use windows::{
            Win32::Networking::WinHttp::*,
            core::{HSTRING, PCWSTR, w},
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(TIMED_OUT.into());
        }
        let net = net_error;
        let lost = |e: String| format!("{UNREACHABLE}: {e}");
        unsafe {
            let session = session()?;
            let connect = Handle::new(WinHttpConnect(session, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0), "WinHttpConnect").map_err(lost)?;
            let request = Handle::new(
                WinHttpOpenRequest(connect.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE),
                "WinHttpOpenRequest",
            )
            .map_err(lost)?;
            // Each step is bounded on its own, and never past the caller's total limit.
            let option = |value: Duration| ms(value.min(left)).to_ne_bytes();
            for (which, value) in [(WINHTTP_OPTION_RESOLVE_TIMEOUT, RESOLVE), (WINHTTP_OPTION_CONNECT_TIMEOUT, CONNECT), (WINHTTP_OPTION_SEND_TIMEOUT, STALL), (WINHTTP_OPTION_RECEIVE_TIMEOUT, STALL), (WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT, STALL)] {
                WinHttpSetOption(Some(request.0 as *const _), which, Some(&option(value))).map_err(net)?;
            }
            WinHttpSetOption(Some(request.0 as *const _), WINHTTP_OPTION_REDIRECT_POLICY, Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes())).map_err(net)?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(net)?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(net)?;
            let mut status = 0u32;
            let mut size = 4u32;
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut _),
                &mut size,
                std::ptr::null_mut(),
            )
            .map_err(net)?;
            let status = u16::try_from(status).map_err(|_| "Invalid HTTP status.")?;
            if is_redirect(status) {
                let mut buf = vec![0u16; 4096];
                let mut bytes = (buf.len() * 2) as u32;
                let location = WinHttpQueryHeaders(request.0, WINHTTP_QUERY_LOCATION, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut _), &mut bytes, std::ptr::null_mut())
                    .ok()
                    .map(|()| String::from_utf16_lossy(&buf[..bytes as usize / 2]));
                return Ok(Reply { status, location });
            }
            let mut buf = vec![0u8; 64 * 1024];
            let mut total = 0u64;
            loop {
                if Instant::now() > deadline {
                    return Err(TIMED_OUT.into());
                }
                let mut read = 0u32;
                WinHttpReadData(request.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).map_err(net)?;
                if read == 0 {
                    break;
                }
                total += read as u64;
                if total > limit {
                    return Err("The download is larger than GitHub reported.".into());
                }
                sink.write_all(&buf[..read as usize]).map_err(|e| format!("Could not save the download: {e}"))?;
            }
            Ok(Reply { status, location: None })
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    /// Canned replies by `host + path`; every request is recorded. Unknown addresses get 404.
    #[derive(Default)]
    pub(crate) struct FakeNet {
        pub replies: HashMap<String, (u16, Option<String>, Vec<u8>)>,
        pub calls: Mutex<Vec<String>>,
        /// Errors returned (first in, first out) before the canned reply for that address.
        pub errors: Mutex<HashMap<String, Vec<String>>>,
    }

    impl FakeNet {
        pub fn reply(mut self, url: &str, status: u16, location: Option<&str>, body: &[u8]) -> Self {
            self.replies.insert(url.into(), (status, location.map(str::to_owned), body.to_vec()));
            self
        }
        /// The next request to `url` fails with `error` (once per call of this).
        pub fn fail_once(self, url: &str, error: &str) -> Self {
            self.errors.lock().unwrap().entry(url.into()).or_default().push(error.into());
            self
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Transport for FakeNet {
        fn get(&self, host: &str, path: &str, limit: u64, _: Instant, sink: &mut dyn Write) -> Result<Reply, String> {
            let key = format!("https://{host}{path}");
            self.calls.lock().unwrap().push(key.clone());
            if let Some(queue) = self.errors.lock().unwrap().get_mut(&key).filter(|q| !q.is_empty()) {
                return Err(queue.remove(0));
            }
            let (status, location, body) = self.replies.get(&key).cloned().unwrap_or((404, None, Vec::new()));
            if !is_redirect(status) {
                if body.len() as u64 > limit {
                    return Err("The download is larger than GitHub reported.".into());
                }
                sink.write_all(&body).map_err(|e| e.to_string())?;
            }
            Ok(Reply { status, location })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fake::FakeNet, *};

    fn get(net: &FakeNet, url: &str) -> Result<(u16, Vec<u8>), String> {
        let mut body = Vec::new();
        fetch(net, url, 1024, Duration::from_secs(5), &mut body).map(|s| (s, body))
    }

    #[test]
    fn only_https_to_the_pinned_github_hosts_is_requested() {
        let net = FakeNet::default().reply("https://api.github.com/repos/x", 200, None, b"ok");
        assert_eq!(get(&net, "https://api.github.com/repos/x").unwrap(), (200, b"ok".to_vec()));
        assert_eq!(get(&net, "https://API.GITHUB.COM:443/repos/x").unwrap().0, 200);
        for bad in [
            "http://api.github.com/repos/x",
            "https://evil.example/repos/x",
            "https://api.github.com.evil.example/x",
            "https://github.com@evil.example/x",
            "https://api.github.com:8443/repos/x",
            "ftp://github.com/x",
        ] {
            assert!(get(&net, bad).is_err(), "{bad} must be refused");
        }
        assert_eq!(net.calls().len(), 2, "a refused address is never requested: {:?}", net.calls());
    }

    #[test]
    fn every_redirect_hop_is_pinned_too() {
        let asset = "https://release-assets.githubusercontent.com/a/b?sig=1";
        let net = FakeNet::default()
            .reply("https://github.com/r/download/v1/RecallSetup.exe", 302, Some(asset), b"")
            .reply(asset, 200, None, b"setup")
            .reply("https://github.com/evil", 302, Some("https://evil.example/setup.exe"), b"")
            .reply("https://github.com/plain", 301, Some("http://github.com/r"), b"")
            .reply("https://github.com/rel", 302, Some("/r/download/v1/RecallSetup.exe"), b"")
            .reply("https://github.com/loop", 302, Some("/loop"), b"");
        assert_eq!(get(&net, "https://github.com/r/download/v1/RecallSetup.exe").unwrap(), (200, b"setup".to_vec()));
        assert_eq!(get(&net, "https://github.com/rel").unwrap(), (200, b"setup".to_vec()));
        assert!(get(&net, "https://github.com/evil").is_err());
        assert!(get(&net, "https://github.com/plain").is_err());
        assert!(get(&net, "https://github.com/loop").unwrap_err().contains("Too many"));
        assert!(!net.calls().iter().any(|c| c.contains("evil.example")), "{:?}", net.calls());
    }

    #[test]
    fn a_small_request_tries_once_more_after_a_timeout_or_lost_connection() {
        let url = "https://api.github.com/repos/x";
        let net = FakeNet::default().reply(url, 200, None, b"ok").fail_once(url, TIMED_OUT);
        assert_eq!(fetch_small(&net, url, 1024, Duration::from_secs(5)).unwrap(), (200, b"ok".to_vec()));
        assert_eq!(net.calls().len(), 2);

        let lost = format!("{UNREACHABLE}: The connection with the server was reset");
        let net = FakeNet::default().reply(url, 200, None, b"ok").fail_once(url, &lost).fail_once(url, TIMED_OUT);
        assert_eq!(fetch_small(&net, url, 1024, Duration::from_secs(5)).unwrap_err(), TIMED_OUT, "two tries at most");
        assert_eq!(net.calls().len(), 2);
    }

    #[test]
    fn a_github_5xx_is_tried_again_but_a_refusal_or_4xx_is_not() {
        let net = FakeNet::default().reply("https://api.github.com/busy", 503, None, b"");
        assert_eq!(fetch_small(&net, "https://api.github.com/busy", 1024, Duration::from_secs(5)).unwrap().0, 503);
        assert_eq!(net.calls().len(), 2);

        let net = FakeNet::default().reply("https://api.github.com/gone", 404, None, b"");
        assert_eq!(fetch_small(&net, "https://api.github.com/gone", 1024, Duration::from_secs(5)).unwrap().0, 404);
        assert!(fetch_small(&net, "https://evil.example/x", 1024, Duration::from_secs(5)).is_err());
        let big = FakeNet::default().reply("https://api.github.com/big", 200, None, &[0; 2048]);
        assert!(fetch_small(&big, "https://api.github.com/big", 1024, Duration::from_secs(5)).is_err());
        assert_eq!(net.calls().len() + big.calls().len(), 2, "no second try for a 404, a refused host or an oversized body");
    }

    #[test]
    fn a_timeout_never_reads_as_a_download_on_a_check() {
        assert!(!TIMED_OUT.contains("download"), "Check now showed 'The download timed out.' for a release lookup");
        assert!(transient(TIMED_OUT) && transient(&format!("{UNREACHABLE}: x")));
        assert!(!transient("The download is larger than GitHub reported."));
    }

    #[test]
    fn a_body_over_the_cap_fails() {
        let net = FakeNet::default().reply("https://api.github.com/big", 200, None, &[0; 2048]);
        assert!(get(&net, "https://api.github.com/big").is_err());
    }

    /// By hand only (real GitHub): `<test exe> live_check_requests --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_check_requests() {
        let url = "https://github.com/LoneBeast27/recall/releases/download/v0.1.6/SHA256SUMS.txt";
        for i in 0..4 {
            let t = Instant::now();
            let mut body = Vec::new();
            let r = fetch(&WinHttp, url, 4096, SMALL_TIMEOUT, &mut body);
            println!("LIVE try {i}: {:.2} s -> {:?} ({} bytes)", t.elapsed().as_secs_f64(), r, body.len());
        }
    }
}
