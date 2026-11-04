//! Timed pre-encode demo on a COPY of a clip, through the app's own export services.
//! Set RECALL_FOLDERS (a sandbox folders.json whose recordings folder holds the copy),
//! RECALL_CACHE_DIR and RECALL_ENGINE first, so nothing lands in the real library.
//! Usage: pre_encode_demo <clip copy> <start s> <end s> [max MB]
#[path = "../../recall/src/edit_model.rs"]
mod edit_model;
#[path = "../src/backend.rs"]
#[allow(dead_code)]
mod backend;
#[path = "../src/services/mod.rs"]
#[allow(dead_code)]
mod services;
#[path = "../src/startup.rs"]
#[allow(dead_code)]
mod startup;

use std::time::{Duration, Instant};

use edit_model::{AudioSetting, EditPlan, ExportSettings, TimeRange};
use services::pre_encode::{self, Status};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (clip, start, end) = (&args[1], args[2].parse::<f64>().unwrap(), args[3].parse::<f64>().unwrap());
    let max_bytes = args.get(4).map(|mb| (mb.parse::<f64>().unwrap() * 1e6) as u64);
    let info = backend::prepare_clip(clip).expect("prepare clip").info;
    let plan = EditPlan {
        ranges: vec![TimeRange::new(start, end)],
        audio: info.audio.iter().map(|a| AudioSetting { stream_index: a.stream_index, gain_db: 0.0, muted: false, mix: true }).collect(),
    };
    let settings = ExportSettings { max_bytes, ..Default::default() };

    // Export as today: no pre-encode.
    backend::arm_export();
    let plain = Instant::now();
    let normal = pre_encode::export(&info, &plan, 60, &settings).expect("normal export");
    let plain_s = plain.elapsed().as_secs_f64();

    // Change a setting, let it settle, prepare in the background, then press Export.
    let key = pre_encode::key(&info, &plan, 60, &settings).expect("re-encode key");
    let changed = Instant::now();
    assert!(pre_encode::start(key, info.clone(), plan.clone(), 60, settings.clone()), "pre-encode starts");
    let mut progress = Vec::new();
    loop {
        match pre_encode::status(key) {
            Status::Running(Some(share)) => {
                let percent = (share * 100.0) as u32;
                if progress.last() != Some(&percent) {
                    progress.push(percent);
                }
            }
            Status::Running(None) => {}
            Status::Ready => break,
            other => panic!("pre-encode ended as {other:?}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let ready_s = changed.elapsed().as_secs_f64();
    backend::arm_export();
    let pressed = Instant::now();
    let instant = pre_encode::export(&info, &plan, 60, &settings).expect("prepared export");
    let export_s = pressed.elapsed().as_secs_f64();
    let left = std::fs::read_dir(pre_encode::dir()).map(|d| d.count()).unwrap_or(0);

    println!("{}", serde_json::json!({
        "normal_export_s": (plain_s * 10.0).round() / 10.0,
        "change_to_ready_s": (ready_s * 10.0).round() / 10.0,
        "export_press_to_file_s": (export_s * 100.0).round() / 100.0,
        "progress_seen": progress.len(),
        "normal": {"path": normal.path, "bytes": normal.bytes, "frames": normal.frames},
        "prepared": {"path": instant.path, "bytes": instant.bytes, "frames": instant.frames},
        "files_left_in_pre_encode_folder": left,
    }));
}
