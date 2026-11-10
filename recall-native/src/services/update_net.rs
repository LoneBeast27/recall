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

/// The real transport: WinHTTP with the system proxy, TLS 1.2+, certificate checks on.
pub(crate) struct WinHttp;

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
        let wait = left.min(Duration::from_secs(30)).as_millis().max(1) as i32;
        let net = |e: windows::core::Error| format!("Download failed: {}", e.message());
        unsafe {
            let session = Handle::new(WinHttpOpen(w!("Recall-updater"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0), "WinHttpOpen")?;
            WinHttpSetTimeouts(session.0, wait, wait, wait, wait).map_err(net)?;
            let tls = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
            if WinHttpSetOption(Some(session.0 as *const _), WINHTTP_OPTION_SECURE_PROTOCOLS, Some(&tls.to_ne_bytes())).is_err() {
                // Windows 10 before 1903 has no TLS 1.3 in WinHTTP.
                WinHttpSetOption(Some(session.0 as *const _), WINHTTP_OPTION_SECURE_PROTOCOLS, Some(&WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2.to_ne_bytes())).map_err(net)?;
            }
            let connect = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0), "WinHttpConnect")?;
            let request = Handle::new(
                WinHttpOpenRequest(connect.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE),
                "WinHttpOpenRequest",
            )?;
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
                    return Err("The download timed out.".into());
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
    }

    impl FakeNet {
        pub fn reply(mut self, url: &str, status: u16, location: Option<&str>, body: &[u8]) -> Self {
            self.replies.insert(url.into(), (status, location.map(str::to_owned), body.to_vec()));
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
    fn a_body_over_the_cap_fails() {
        let net = FakeNet::default().reply("https://api.github.com/big", 200, None, &[0; 2048]);
        assert!(get(&net, "https://api.github.com/big").is_err());
    }
}
