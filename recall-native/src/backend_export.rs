//! Running an export: the engine's `--export-edit` into `Recall\Edited` (and, for a pre-encode,
//! into the cache). Shares the export guards and helpers of `backend.rs`.
use super::*;

pub fn export_clip(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<ExportSummary,String>{
    let _queued=ExportQueueGuard;
    validate_info(info)?;validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;export.validate(plan)?;
    if EXPORTING.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return Err("Another export is running.".into());}
    let _guard=ExportGuard;
    let output=export_target(info,fps,export.mode==ExportMode::Lossless)?;
    // The export holds the output drive's lane: prefetch pauses instead of seeking against it.
    engine_export(info,&output,plan,fps,export,&EXPORT_CANCEL,None,false)
}
/// Held by an export that ends without [`export_clip`] (a prepared copy): closing waits for it.
pub(crate) fn export_queued()->impl Drop{ExportQueueGuard}
/// Cancel was pressed for the queued export.
pub(crate) fn export_cancelled()->bool{EXPORT_CANCEL.load(Ordering::Acquire)}
/// Where an export of `info` lands now (`Recall\Edited`, created if missing).
pub(crate) fn export_target(info:&ClipInfo,fps:u32,lossless:bool)->Result<PathBuf,String>{
    let nonce=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos();
    let source=Path::new(&info.path);let outdir=storage::export_dir(source,&library_dir()).ok_or("source has no directory")?;
    std::fs::create_dir_all(&outdir).map_err(|e|format!("create export directory: {e}"))?;
    let stem=source.file_stem().and_then(|v|v.to_str()).unwrap_or("clip");
    Ok(if lossless{outdir.join(format!("{stem}-trim-{nonce}.mp4"))}else{outdir.join(format!("{stem}-edited-{fps}fps-{nonce}.mp4"))})
}
/// Runs the engine's edited export of `info` into `output` on the output drive's lane, until it
/// ends or `cancel` turns true. `on_line` sees the engine's stdout as it arrives (progress).
pub(crate) fn engine_export(info:&ClipInfo,output:&Path,plan:&EditPlan,fps:u32,export:&ExportSettings,cancel:&'static AtomicBool,on_line:Option<storage::OnLine>,background:bool)->Result<ExportSummary,String>{
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
        if !result.status.success(){return Err(events.iter().find(|v|v["type"]=="export_failed").and_then(|v|v["reason"].as_str()).map(str::to_owned).unwrap_or_else(||format!("Export failed: {}",String::from_utf8_lossy(&result.stderr).trim())));}
        let done=events.iter().find(|v|v["type"]=="exported").ok_or("Recorder returned no completed export.")?;
        let report=events.iter().find(|v|v["type"]=="export_report");
        let field=|k:&str|report.and_then(|r|r[k].as_u64());
        let bytes=field("bytes").or_else(||std::fs::metadata(output).ok().map(|m|m.len())).unwrap_or(0);
        Ok(ExportSummary{path:output.to_string_lossy().into_owned(),fps:if lossless{None}else{Some(fps)},frames:done["frames"].as_u64().ok_or("Missing frame count.")?,seconds:done["seconds"].as_f64().ok_or("Missing duration.")?,bytes,lossless,width:field("width").unwrap_or(0) as u32,height:field("height").unwrap_or(0) as u32})
    })();
    let _=std::fs::remove_file(&project);result
}
