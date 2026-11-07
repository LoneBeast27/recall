use super::*;

#[test]
fn export_names_are_short_and_take_the_next_free_number() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-export-names");
    let first = free_name(&sandbox, "clip-edited-60fps").unwrap();
    assert_eq!(first, sandbox.join("clip-edited-60fps-1.mp4"));
    std::fs::write(&first, b"x").unwrap();
    std::fs::write(sandbox.join("clip-edited-60fps-2.mp4"), b"x").unwrap();
    assert_eq!(free_name(&sandbox, "clip-edited-60fps").unwrap(), sandbox.join("clip-edited-60fps-3.mp4"));
    assert_eq!(free_name(&sandbox, "clip-trim").unwrap(), sandbox.join("clip-trim-1.mp4"));
    // `storage::has_export` recognises exports by their `<stem>-edited-` / `<stem>-trim-` prefix.
    assert!(first.file_name().unwrap().to_string_lossy().starts_with("clip-edited-"));
}

#[test]
fn a_failed_engine_run_leaves_none_of_its_temps() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-export-failed-temps");
    let output = sandbox.join("clip-edited-60fps-1.mp4");
    let ours = [".clip-edited-60fps-1.recall-export-77-123-0.mp4", ".clip-edited-60fps-1.recall-export-91-5-1.mp4"];
    let others = [".clip-edited-60fps-10.recall-export-77-123-0.mp4", "clip-edited-60fps-2.mp4", ".clip-edited-60fps-1.recall-export-77-1-0.txt"];
    for name in ours.iter().chain(&others) {
        std::fs::write(sandbox.join(name), b"x").unwrap();
    }
    remove_failed_temps(&output);
    assert!(ours.iter().all(|name| !sandbox.join(name).exists()), "this export's partial outputs go");
    assert!(others.iter().all(|name| sandbox.join(name).exists()), "other exports and files stay");
}

#[test]
fn an_export_during_a_recording_runs_as_a_background_job() {
    assert!(yields_to_recording(true), "EcoQoS while a game records");
    assert!(!yields_to_recording(false), "full speed otherwise");
}
