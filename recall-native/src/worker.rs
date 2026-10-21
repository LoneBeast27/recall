//! Background threads. The UI thread never blocks on disk, ffmpeg or the engine.
//! `io`: list / prepare / save / export / delete (serial, in order).
//! `assets`: thumbnails and durations (bulk). `scrub`: hover-scrub frames (latency-sensitive).
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use crate::{
    backend::{self, ClipEntry, ExportSummary, PreparedClip},
    edit_model::{ClipInfo, EditPlan},
    services::library,
};

pub enum Job {
    List,
    Prepare(u64, String),
    SaveProject(ClipInfo, EditPlan, u32),
    Export(ClipInfo, EditPlan, u32),
    Delete(Vec<PathBuf>),
}

pub enum AssetJob {
    Thumb(String),
    Duration(String),
}

pub enum Done {
    Clips(Result<Vec<ClipEntry>, String>),
    Prepared(u64, Result<PreparedClip, String>),
    Saved(Result<String, String>),
    Exported(Result<ExportSummary, String>),
    Deleted(Result<usize, String>),
    Thumb(String, Result<String, String>),
    Duration(String, Option<f64>),
    Scrub(String, Result<Vec<String>, String>),
}

pub struct Workers {
    jobs: Sender<Job>,
    assets: Sender<AssetJob>,
    scrub: Sender<String>,
    pub done: Receiver<Done>,
}

impl Workers {
    pub fn spawn() -> Self {
        let (done_tx, done) = mpsc::channel::<Done>();
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (assets, asset_rx) = mpsc::channel::<AssetJob>();
        let (scrub, scrub_rx) = mpsc::channel::<String>();

        let tx = done_tx.clone();
        spawn_named("recall-io", move || {
            while let Ok(job) = job_rx.recv() {
                let done = match job {
                    Job::List => Done::Clips(backend::list_clips()),
                    Job::Prepare(generation, path) => Done::Prepared(generation, backend::prepare_clip(&path)),
                    Job::SaveProject(info, plan, fps) => Done::Saved(backend::save_project(&info, &plan, fps)),
                    Job::Export(info, plan, fps) => Done::Exported(backend::export_clip(&info, &plan, fps)),
                    Job::Delete(paths) => Done::Deleted(library::delete_to_recycle_bin(&paths)),
                };
                if tx.send(done).is_err() {
                    break;
                }
            }
        });

        let tx = done_tx.clone();
        spawn_named("recall-assets", move || {
            while let Ok(job) = asset_rx.recv() {
                let done = match job {
                    AssetJob::Thumb(path) => {
                        let result = backend::thumbnail(&path);
                        Done::Thumb(path, result)
                    }
                    AssetJob::Duration(path) => {
                        let secs = library::mp4_duration(std::path::Path::new(&path));
                        Done::Duration(path, secs)
                    }
                };
                if tx.send(done).is_err() {
                    break;
                }
            }
        });

        spawn_named("recall-scrub", move || {
            while let Ok(mut path) = scrub_rx.recv() {
                // Only the latest hovered clip matters.
                while let Ok(newer) = scrub_rx.try_recv() {
                    path = newer;
                }
                let result = library::scrub_frames(&path);
                if done_tx.send(Done::Scrub(path, result)).is_err() {
                    break;
                }
            }
        });

        Self { jobs, assets, scrub, done }
    }

    pub fn job(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn asset(&self, job: AssetJob) {
        let _ = self.assets.send(job);
    }

    pub fn scrub(&self, path: String) {
        let _ = self.scrub.send(path);
    }
}

fn spawn_named(name: &str, body: impl FnOnce() + Send + 'static) {
    if let Err(error) = thread::Builder::new().name(name.into()).spawn(body) {
        eprintln!("start {name}: {error}");
    }
}
