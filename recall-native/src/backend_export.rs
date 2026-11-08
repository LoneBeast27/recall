//! Running an export: the engine's `--export-edit` into `Recall\Edited` (and, for a pre-encode,
//! into the cache). Shares the export guards and helpers of `backend.rs`.
use super::*;

pub fn export_clip(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<ExportSummary,String>{
    let _queued=ExportQueueGuard;
    validate_info(info)?;validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;export.validate(plan)?;
    if EXPORTING.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return Err("Another export is running.".into());}
    let _guard=ExportGuard;
    let output=export_target(info,fps,export.mode==ExportMode::Lossless)?;
    run_export(info,&output,plan,fps,export,storage::recorder_busy(),engine_export)
}
/// The engine run of [`export_clip`] (`engine` is [`engine_export`]; a test passes a fake).
pub(crate) type Engine=fn(&ClipInfo,&Path,&EditPlan,u32,&ExportSettings,&'static AtomicBool,Option<storage::OnLine>,bool)->Result<ExportSummary,String>;
pub(crate) fn run_export(info:&ClipInfo,output:&Path,plan:&EditPlan,fps:u32,export:&ExportSettings,recording:bool,engine:Engine)->Result<ExportSummary,String>{
    EXPORT_PROGRESS.store(NO_PROGRESS,Ordering::Relaxed);
    let on_line:storage::OnLine=Box::new(|line|{if let Some(share)=crate::services::pre_encode::progress_share(line){EXPORT_PROGRESS.store((share*1000.0) as u32,Ordering::Relaxed);}});
    // The export holds the output drive's lane: prefetch pauses instead of seeking against it.
    let result=engine(info,output,plan,fps,export,&EXPORT_CANCEL,Some(on_line),yields_to_recording(recording));
    EXPORT_PROGRESS.store(NO_PROGRESS,Ordering::Relaxed);
    result
}
/// Per-mille done of the running export; `NO_PROGRESS` until the engine reports.
static EXPORT_PROGRESS:std::sync::atomic::AtomicU32=std::sync::atomic::AtomicU32::new(NO_PROGRESS);
const NO_PROGRESS:u32=u32::MAX;
/// The share of the running export done, once the engine reports it (the button shows it).
#[allow(dead_code)] // the examples build backend without the editor
pub fn export_progress()->Option<f32>{let value=EXPORT_PROGRESS.load(Ordering::Relaxed);(value!=NO_PROGRESS).then(||value as f32/1000.0)}
/// An export started while a game records runs as a background job (EcoQoS, slow cores), like
/// the pre-encode: it shares NVENC and the CPU with the live recording, which must not drop frames.
pub(crate) fn yields_to_recording(recording:bool)->bool{recording}
/// Held by an export that ends without [`export_clip`] (a prepared copy): closing waits for it.
pub(crate) fn export_queued()->impl Drop{ExportQueueGuard}
/// Cancel was pressed for the queued export.
pub(crate) fn export_cancelled()->bool{EXPORT_CANCEL.load(Ordering::Acquire)}
/// Where an export of `info` lands now (`Recall\Edited`, created if missing).
pub(crate) fn export_target(info:&ClipInfo,fps:u32,lossless:bool)->Result<PathBuf,String>{
    let source=Path::new(&info.path);let outdir=storage::export_dir(source,&library_dir()).ok_or("source has no directory")?;
    std::fs::create_dir_all(&outdir).map_err(|e|format!("create export directory: {e}"))?;
    let stem=source.file_stem().and_then(|v|v.to_str()).unwrap_or("clip");
    free_name(&outdir,&if lossless{format!("{stem}-trim")}else{format!("{stem}-edited-{fps}fps")})
}
/// `<outdir>\<base>-<n>.mp4` with `n` one past the highest taken (short names people drag into
/// Discord, sorting by age even after one is deleted; the engine and the prepared copy still
/// refuse to replace a file that appears meanwhile).
pub(crate) fn free_name(outdir:&Path,base:&str)->Result<PathBuf,String>{
    let prefix=format!("{base}-");
    let number=|name:&str|name.strip_prefix(&prefix)?.strip_suffix(".mp4").filter(|n|!n.is_empty()&&n.bytes().all(|b|b.is_ascii_digit()))?.parse::<u64>().ok();
    let highest=std::fs::read_dir(outdir).into_iter().flatten().flatten().filter_map(|item|number(&item.file_name().to_string_lossy())).max().unwrap_or(0);
    (highest+1..highest+10_000).map(|n|outdir.join(format!("{base}-{n}.mp4"))).find(|path|!path.exists()).ok_or_else(||format!("no free export name for {base} in {}",outdir.display()))
}
/// The next free export name after `taken` (`<base>-<n>.mp4`), for a name that was taken
/// while the export ran.
pub(crate) fn name_after(taken:&Path)->Option<PathBuf>{
    let (outdir,stem)=(taken.parent()?,taken.file_stem()?.to_str()?);
    let (base,n)=stem.rsplit_once('-')?;n.parse::<u64>().ok()?;
    free_name(outdir,base).ok()
}
/// Runs the engine's edited export of `info` into `output` on the output drive's lane, until it
/// ends or `cancel` turns true. `on_line` sees the engine's stdout as it arrives (progress).
pub(crate) fn engine_export(info:&ClipInfo,output:&Path,plan:&EditPlan,fps:u32,export:&ExportSettings,cancel:&'static AtomicBool,on_line:Option<storage::OnLine>,background:bool)->Result<ExportSummary,String>{
    // The recorder's storage cap keeps the source while it is read (`open_clips.rs`).
    let _open=crate::services::library::open_clips::exporting(&info.path);
    let lossless=export.mode==ExportMode::Lossless;
    let nonce=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos();
    let outdir=output.parent().ok_or("export path has no directory")?.to_path_buf();
    // Read the SSD copy when one exists; the output still lands beside the original.
    let input=storage::cached_video(&info.path).map(PathBuf::from).unwrap_or_else(||PathBuf::from(&info.path));
    let lowered=storage::drive_kind(&outdir).is_hdd()||storage::drive_kind(&input).is_hdd();
    let project=tmp_dir()?.join(format!("export-{}-{nonce}.json",std::process::id()));
    std::fs::write(&project,serde_json::to_vec(&serde_json::json!({"plan":plan,"export":export})).map_err(|e|e.to_string())?).map_err(|e|format!("write export plan: {e}"))?;
    let result=(||{
        let mut command=engine_command("export-edit")?;command.arg(&input).arg(output).arg(fps.to_string()).arg(&project).arg("--json");
        // A background job (pre-encode) asks the engine for EcoQoS: slow cores, low clocks.
        if background{command.env("RECALL_ECOQOS","1");}
        let limit=export_limit(plan,fps);
        let run=storage::run_io(&outdir,Priority::Interactive,move||storage::run_cancellable_lines(&mut command,lowered,cancel,limit,on_line))?.map_err(|e|format!("start export: {e}"))?;
        let result=match run{storage::Run::Finished(result)=>result,storage::Run::Cancelled{pid}=>{remove_export_temps(output,pid);return Err(EXPORT_CANCELLED.to_owned());},storage::Run::TimedOut{pid}=>{remove_export_temps(output,pid);return Err("Export stopped: the recorder did not finish in time.".to_owned());}};
        let events:Vec<serde_json::Value>=String::from_utf8_lossy(&result.stdout).lines().filter_map(|l|serde_json::from_str(l).ok()).collect();
        // A crashed engine leaves its hidden partial output: the name is this export's alone.
        if !result.status.success(){remove_failed_temps(output,crate::services::process_tree::alive);return Err(events.iter().find(|v|v["type"]=="export_failed").and_then(|v|v["reason"].as_str()).map(str::to_owned).unwrap_or_else(||format!("Export failed: {}",String::from_utf8_lossy(&result.stderr).trim())));}
        let done=events.iter().find(|v|v["type"]=="exported").ok_or("Recorder returned no completed export.")?;
        let report=events.iter().find(|v|v["type"]=="export_report");
        let field=|k:&str|report.and_then(|r|r[k].as_u64());
        let bytes=field("bytes").or_else(||std::fs::metadata(output).ok().map(|m|m.len())).unwrap_or(0);
        Ok(ExportSummary{path:output.to_string_lossy().into_owned(),fps:if lossless{None}else{Some(fps)},frames:done["frames"].as_u64().ok_or("Missing frame count.")?,seconds:done["seconds"].as_f64().ok_or("Missing duration.")?,bytes,lossless,width:field("width").unwrap_or(0) as u32,height:field("height").unwrap_or(0) as u32})
    })();
    let _=std::fs::remove_file(&project);result
}
/// The engine's hidden temporaries of `output` (`.<stem>.recall-export-<pid>-<nonce>-<n>.mp4`)
/// whose engine no longer runs (`alive`, review RI-6): the failed run's own, and a crashed
/// one's; a temp of an engine still running is left to it.
fn remove_failed_temps(output:&Path,alive:fn(u32)->bool){
    let (Some(outdir),Some(stem))=(output.parent(),output.file_stem().and_then(|v|v.to_str())) else{return};
    let prefix=format!(".{stem}.recall-export-");
    for item in std::fs::read_dir(outdir).into_iter().flatten().flatten(){let name=item.file_name().to_string_lossy().into_owned();if name.starts_with(&prefix)&&crate::services::pre_encode::dead_export_temp(&name,alive){let _=std::fs::remove_file(item.path());}}
}
#[cfg(test)]
#[path="backend_export_tests.rs"]
mod tests;
