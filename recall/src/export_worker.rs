//! CFR / edited exports (feature `encode`): the single bounded `recall-export`
//! worker behind the `export_clip` command, and the shared run + report path
//! also used by `--export-cfr` / `--export-edit`.

use std::time::Instant;

use crate::{edit_model, export, protocol, ExportHook};

/// A single bounded export worker keeps re-encoding separate from live recording.
#[cfg(feature = "encode")]
pub(super) fn spawn_export_worker(json: bool) -> ExportHook {
    struct Job { request: export::ExportRequest, edit: Option<edit_model::EditPlan> }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(1);
    std::thread::Builder::new()
        .name("recall-export".into())
        .spawn(move || {
            let settings = edit_model::ExportSettings::default();
            while let Ok(job) = rx.recv() {
                let _ = run_export(&job.request, job.edit.as_ref().map(|plan| (plan, &settings)), json);
            }
        })
        .expect("spawn export worker");
    Box::new(move |input, output, fps, edit| {
        let fps = fps.unwrap_or(60);
        let output = output.unwrap_or_else(|| {
            let source = std::path::Path::new(&input);
            let parent = source.parent().unwrap_or_else(|| std::path::Path::new("."));
            let stem = source
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Recall");
            parent
                .join("exports")
                .join(format!("{stem}-cfr-{fps}fps-{}.mp4", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .into_owned()
        });
        if let Some(parent) = std::path::Path::new(&output).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                export_failure(json, &input, &format!("create export directory: {e}"));
                return;
            }
        }
        let request = export::ExportRequest {
            input: input.clone(),
            output,
            fps,
        };
        if let Err(e) = tx.try_send(Job { request, edit }) {
            export_failure(json, &input, &format!("export queue unavailable: {e}"));
        }
    })
}

#[cfg(feature = "encode")]
fn export_failure(json: bool, source: &str, reason: &str) {
    if json {
        protocol::emit(&protocol::Outbound::ExportFailed {
            source: source.into(),
            reason: reason.into(),
        });
    } else {
        eprintln!("[recall] export failed: {reason}");
    }
}

#[cfg(feature = "encode")]
pub(super) fn run_export(request: &export::ExportRequest, edit: Option<(&edit_model::EditPlan, &edit_model::ExportSettings)>, json: bool) -> Result<(), String> {
    let start = Instant::now();
    if json {
        protocol::emit(&protocol::Outbound::ExportStarted {
            source: request.input.clone(),
            path: request.output.clone(),
            fps: request.fps,
        });
    }
    let result = match edit {
        Some((plan, settings)) => export::export_edited(request, plan, settings).map(|report| {
            if json { println!("{}", report.report_line(&request.input, &request.output)); }
            else { println!("[recall] {:?} export: {} bytes, {}x{}, ranges {:?}", report.mode, report.bytes, report.width, report.height, report.ranges.iter().map(|r| (r.start, r.end, r.speed)).collect::<Vec<_>>()); }
            report.summary()
        }),
        None => export::export_cfr(request),
    };
    match result {
        Ok(result) => {
            if json {
                protocol::emit(&protocol::Outbound::Exported {
                    source: request.input.clone(),
                    path: request.output.clone(),
                    fps: request.fps,
                    frames: result.frames,
                    seconds: result.seconds,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            } else {
                println!(
                    "[recall] exported {} at {} CFR fps",
                    request.output, request.fps
                );
            }
            Ok(())
        }
        Err(e) => {
            export_failure(json, &request.input, &e);
            Err(e)
        }
    }
}
