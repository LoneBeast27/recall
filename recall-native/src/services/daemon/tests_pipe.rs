//! Daemon client tests that leave the parser: a real pipe against a fake controller, and
//! finding the controller (workspace manifest, repo build, running process).

use super::controller::{process_running, repo_controller, workspace_lists_controller};
use super::*;

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Real pipe, real threads: a fake controller on a private pipe name sees the two
/// connect requests and a UI command; its lines reach the snapshot; closing it marks
/// the client disconnected and sends are dropped.
#[test]
fn pipe_client_round_trip_against_a_fake_controller() {
    use interprocess::local_socket::traits::{Listener as _, Stream as _};
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
    use std::io::{BufRead as _, BufReader, Write as _};

    let pipe = format!("recall-native-test-{}", std::process::id());
    let listener = ListenerOptions::new().name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap()).create_sync().unwrap();
    let client = DaemonClient::spawn_on(&pipe);
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stream = listener.accept().unwrap();
        let _ = accepted_tx.send(stream);
        std::thread::sleep(std::time::Duration::from_secs(5)); // keep the listener alive briefly
    });
    let stream = accepted_rx.recv_timeout(std::time::Duration::from_secs(10)).expect("client connected");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"status"}"#);
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"get_config"}"#);
    wait_until("connected", || client.snapshot().connected);
    writer.write_all(b"{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"save_seconds\":120}}\n").unwrap();
    writer.write_all(b"{\"mod\":\"engine\",\"event\":{\"type\":\"saved\",\"path\":\"E:\\\\c.mp4\",\"seconds\":12.5,\"bytes\":9,\"elapsed_ms\":3}}\n").unwrap();
    wait_until("session status", || client.snapshot().session.is_some_and(|s| s.state == "recording"));
    wait_until("saved event", || client.revision() >= 3);
    assert_eq!(client.drain_events(), vec![DaemonEvent::Saved { path: "E:\\c.mp4".into(), seconds: 12.5, bytes: 9, elapsed_ms: 3, partial: None }]);
    assert!(client.drain_events().is_empty(), "drained once");
    assert!(client.send(DaemonCommand::SaveClip { seconds: Some(30) }));
    // A status ping (every 2 s) may come first on a slow run.
    let sent = lines.by_ref().map(Result::unwrap).find(|l| l != r#"{"cmd":"status"}"#).unwrap();
    assert_eq!(sent, r#"{"cmd":"save_clip","seconds":30}"#);
    // Idle, the client asks for status by itself: only a live pump answers.
    let t = std::time::Instant::now();
    assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"status"}"#);
    assert!(t.elapsed() < std::time::Duration::from_secs(4), "ping after {:?}", t.elapsed());
    drop(writer);
    drop(lines);
    wait_until("disconnect", || !client.snapshot().connected);
    assert_eq!(client.snapshot().session, None, "stale status cleared");
}

/// The recorder quits (an install) and a new one starts on the same pipe: the app keeps
/// running, sees the disconnect, then reconnects by itself and asks for status again.
#[test]
fn pipe_client_reconnects_to_a_restarted_controller() {
    use interprocess::local_socket::traits::{Listener as _, Stream as _};
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
    use std::io::{BufRead as _, BufReader, Write as _};

    let pipe = format!("recall-native-restart-test-{}", std::process::id());
    let listen = || ListenerOptions::new().name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap()).create_sync().unwrap();
    let client = DaemonClient::spawn_on(&pipe);
    for run in 1..=2 {
        let stream = listen().accept().unwrap(); // the listener goes with this scope, like a quit recorder
        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"status"}"#, "run {run} asks for status");
        assert_eq!(lines.next().unwrap().unwrap(), r#"{"cmd":"get_config"}"#);
        wait_until("connected", || client.snapshot().connected);
        writer.write_all(b"{\"mod\":\"session\",\"event\":{\"type\":\"status\",\"state\":\"recording\",\"save_seconds\":120}}\n").unwrap();
        wait_until("session status", || client.snapshot().session.is_some_and(|s| s.state == "recording"));
        assert!(client.send(DaemonCommand::SaveClip { seconds: None }), "run {run} takes commands");
        drop(writer);
        drop(lines);
        wait_until("disconnect", || !client.snapshot().connected);
        assert!(!client.send(DaemonCommand::Status), "no recorder: commands are dropped, the app stays up");
    }
}

#[test]
fn workspace_manifest_detection() {
    assert!(workspace_lists_controller("[workspace]\nresolver = \"2\"\nmembers = [\"recall\", \"recall-hdr\", \"recall-overlay\"]\n"));
    assert!(workspace_lists_controller("[workspace]\nmembers = [\n  \"recall\",\n  \"recall-hdr\",\n]\n"));
    assert!(!workspace_lists_controller("[workspace]\n"));
    assert!(!workspace_lists_controller("[package]\nname = \"recall-hdr\"\n"));
    assert!(!workspace_lists_controller("[workspace]\nmembers = [\"recall-hdr-extra\"]\n"));
}

#[test]
fn repo_controller_walks_up_to_the_workspace() {
    let root = std::env::temp_dir().join(format!("recall-native-controller-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let exe_dir = root.join("target-gpui").join("debug");
    std::fs::create_dir_all(&exe_dir).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"recall\", \"recall-hdr\"]\n").unwrap();
    let exe = exe_dir.join("recall-native.exe");
    assert_eq!(repo_controller(&exe), None, "no build yet");
    let debug = root.join("target-dev").join("debug");
    std::fs::create_dir_all(&debug).unwrap();
    std::fs::write(debug.join("recall-hdr.exe"), b"stub").unwrap();
    assert_eq!(repo_controller(&exe), Some(debug.join("recall-hdr.exe")));
    let release = root.join("target-dev").join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(release.join("recall-hdr.exe"), b"stub").unwrap();
    assert_eq!(repo_controller(&exe), Some(release.join("recall-hdr.exe")), "release preferred");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn process_lookup_finds_this_test_binary_only() {
    let me = std::env::current_exe().unwrap();
    let name = me.file_name().unwrap().to_string_lossy().to_uppercase();
    assert!(process_running(&name).unwrap());
    assert!(!process_running("recall-native-no-such-process-7f3a.exe").unwrap());
}

/// The pipe's server runs as another account (the SID lookup is faked): the client writes
/// nothing to it, stays disconnected and names the refusal; the real check accepts this
/// process's own fake controller (the round-trip tests above go through it).
#[test]
fn a_pipe_whose_server_runs_as_another_account_is_refused() {
    use interprocess::local_socket::traits::Listener as _;
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
    use std::io::Read as _;

    fn other_account(stream: &Stream) -> Result<(), String> {
        pipe_owner::verdict(pipe_owner::server_pid(stream), Ok("S-1-5-21-1-1-1-1001".into()), |_| Ok("S-1-5-21-2-2-2-1002".into()))
    }
    let pipe = format!("recall-native-foreign-test-{}", std::process::id());
    let listener = ListenerOptions::new().name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap()).create_sync().unwrap();
    let client = DaemonClient::spawn_checked(&pipe, other_account);
    let mut stream = listener.accept().unwrap();
    wait_until("refusal", || client.snapshot().refused.is_some());
    let refused = client.snapshot().refused.unwrap();
    assert!(refused.starts_with(pipe_owner::FOREIGN), "{refused}");
    // Nothing was written: the client dropped the connection, so the read ends at EOF.
    let mut got = Vec::new();
    let _ = stream.read_to_end(&mut got);
    assert!(got.is_empty(), "sent {:?}", String::from_utf8_lossy(&got));
    assert!(!client.snapshot().connected);
    assert!(!client.send(DaemonCommand::Status), "no writer for a refused server");
    drop(client);
    // The live check accepts a server of this account.
    let own = format!("recall-native-own-test-{}", std::process::id());
    let listener = ListenerOptions::new().name(own.as_str().to_ns_name::<GenericNamespaced>().unwrap()).create_sync().unwrap();
    let real = DaemonClient::spawn_checked(&own, pipe_owner::check);
    let _accepted = listener.accept().unwrap();
    wait_until("connected", || real.snapshot().connected);
    assert_eq!(real.snapshot().refused, None);
}
