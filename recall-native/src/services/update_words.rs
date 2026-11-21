//! What the updater says on screen: plain steps a non-technical reader follows ("Looking for
//! updates", "Update found", "Downloading the update"), never server, file or branch names
//! (the user, 2026-11-21: "break it down to basic steps that are less dev jargon").
//! The technical reasons stay in the code paths that make them; the screen gets one of these.

pub const LOOKING: &str = "Looking for updates…";
pub const UP_TO_DATE: &str = "You're up to date.";
pub const STARTING_DOWNLOAD: &str = "Starting the download…";
pub const CHECKING_DOWNLOAD: &str = "Verifying update…";
pub const STARTING_INSTALLER: &str = "Starting the installer…";
/// No source gave a verified update: never "up to date".
pub const COULDNT_CHECK: &str = "Recall couldn't check for updates right now. Your version keeps working. Try again later.";

/// The plain sentence for an updater failure, picked from its technical reason.
pub fn failure(reason: &str) -> &'static str {
    let has = |words: &[&str]| words.iter().any(|w| reason.contains(w));
    if reason.starts_with(super::sources::NO_SOURCE) {
        COULDNT_CHECK
    } else if reason == super::TOO_OLD {
        "This update needs a fresh install of Recall. Your version keeps working."
    } else if super::net::transient(reason) {
        "Recall couldn't connect to look for updates. Check your internet connection, then try again."
    } else if has(&["Cannot save", "Could not save", "Cannot create", "Setup could not be read", "is unavailable", "LOCALAPPDATA"]) {
        "Recall couldn't save the update on this PC. Make sure the drive has free space, then try again."
    } else if has(&["open Setup", "opened Setup"]) {
        "Recall couldn't start the installer. Try again, or restart your PC first."
    } else if has(&["checksum", "official", "match", "larger than", "Refused", "invalid", "SOURCE-REVISION", "source revision", "size is"]) {
        "The update couldn't be verified, so Recall didn't install it. Try again later."
    } else {
        "The update service isn't responding properly right now. Try again later."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_reads_as_a_plain_sentence() {
        let cases = [
            (super::super::net::TIMED_OUT, "internet connection"),
            ("Could not reach the update server: The server name could not be resolved", "internet connection"),
            ("Cannot save Setup: Access is denied.", "free space"),
            ("Windows could not open Setup: The operation was canceled by the user.", "installer"),
            ("Setup did not match the release's checksum and size.", "verified"),
            ("The release could not be verified as an official release.", "verified"),
            ("The update server returned HTTP 502.", "Try again later"),
            // A garbled reply is the service's problem, never a full drive.
            ("The release information could not be read: expected value at line 1 column 1", "Try again later"),
            ("The release check could not be read: EOF while parsing", "Try again later"),
            ("Setup could not be read: The device is not ready.", "free space"),
            ("No update source gave a verified update file. https://gitlab.com/x: HTTP 404; https://github.com/y: Could not reach the update server", "couldn't check"),
            (super::super::TOO_OLD, "fresh install"),
        ];
        for (reason, expect) in cases {
            let said = failure(reason);
            assert!(said.contains(expect), "{reason} -> {said}");
            for jargon in ["GitHub", "HTTP", "Setup", "server", "checksum", "SHA", "main", "API", "tag"] {
                assert!(!said.contains(jargon), "{said} says {jargon}");
            }
        }
    }
}
