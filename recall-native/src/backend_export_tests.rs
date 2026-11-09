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
    remove_failed_temps(&output, |_| false);
    assert!(ours.iter().all(|name| !sandbox.join(name).exists()), "this export's partial outputs go");
    assert!(others.iter().all(|name| sandbox.join(name).exists()), "other exports and files stay");
}

/// RI-6: the failed-run sweep takes only temps of engines that no longer run.
#[test]
fn a_failed_run_leaves_a_running_engines_temp_alone() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-export-live-temp");
    let output = sandbox.join("clip-trim-1.mp4");
    let (dead, live) = (".clip-trim-1.recall-export-77-123-0.mp4", ".clip-trim-1.recall-export-91-5-1.mp4");
    for name in [dead, live] {
        std::fs::write(sandbox.join(name), b"x").unwrap();
    }
    remove_failed_temps(&output, |pid| pid == 91);
    assert!(!sandbox.join(dead).exists(), "the dead engine's temp goes");
    assert!(sandbox.join(live).exists(), "a running engine's temp stays");
}

/// R2-6: the next export takes the highest number + 1, so names sort by age even after an
/// older export was deleted.
#[test]
fn a_deleted_export_number_is_not_reused() {
    let sandbox = crate::test_temp::TempDir::sandbox("recall-export-names-max");
    std::fs::write(sandbox.join("clip-edited-60fps-2.mp4"), b"x").unwrap(); // -1 was deleted
    std::fs::write(sandbox.join("clip-edited-60fps-10.mp4"), b"x").unwrap();
    std::fs::write(sandbox.join("clip-edited-60fps-x.mp4"), b"x").unwrap();
    std::fs::write(sandbox.join("clip-edited-60fps-99-2.mp4"), b"x").unwrap();
    assert_eq!(free_name(&sandbox, "clip-edited-60fps").unwrap(), sandbox.join("clip-edited-60fps-11.mp4"));
    assert_eq!(free_name(&sandbox, "clip-edited").unwrap(), sandbox.join("clip-edited-1.mp4"), "another base's names do not count");
}

/// X11 through the export's own call path: `export_clip` hands the engine `background = true`
/// while a game records (EcoQoS), and full speed otherwise.
#[test]
fn an_export_during_a_recording_asks_the_engine_for_a_background_run() {
    static BACKGROUND: AtomicBool = AtomicBool::new(false);
    #[allow(clippy::too_many_arguments)] // the shape of `engine_export`
    fn fake_engine(_: &ClipInfo, output: &Path, _: &EditPlan, fps: u32, _: &ExportSettings, _: &'static AtomicBool, _: Option<storage::OnLine>, background: bool) -> Result<ExportSummary, String> {
        BACKGROUND.store(background, Ordering::SeqCst);
        Ok(ExportSummary { path: output.to_string_lossy().into_owned(), fps: Some(fps), frames: 1, seconds: 1.0, bytes: 1, lossless: false, width: 2, height: 2 })
    }
    let info = ClipInfo { path: "E:/Recall/Raw/a.mp4".into(), duration: 10.0, width: 1920, height: 1080, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() };
    let plan = EditPlan { mix_down: None, ranges: vec![crate::edit_model::TimeRange::new(0.0, 5.0)], audio: Vec::new() };
    let output = Path::new("E:/Recall/Edited/a-edited-60fps-1.mp4");
    run_export(&info, output, &plan, 60, &ExportSettings::default(), true, fake_engine).unwrap();
    assert!(BACKGROUND.load(Ordering::SeqCst), "EcoQoS while a game records");
    run_export(&info, output, &plan, 60, &ExportSettings::default(), false, fake_engine).unwrap();
    assert!(!BACKGROUND.load(Ordering::SeqCst), "full speed otherwise");
}
