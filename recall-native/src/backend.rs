//! Native client operations. Call media preparation/export on a background thread.
use crate::edit_model::{ClipInfo, EditPlan};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
#[path = "visual_assets.rs"]
mod visual_assets;

#[derive(Clone, Debug, Serialize)]
pub struct ClipEntry { pub name:String, pub path:String, pub bytes:u64, pub mtime:u64, pub thumb:Option<String> }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project { pub version:u32,pub source_path:String,pub source_bytes:u64,pub source_modified_ms:u64,pub plan:EditPlan,pub export_fps:u32 }
#[derive(Clone,Debug,Default)]
pub struct VisualAssets { pub poster:Option<String>,pub filmstrip:Vec<String> }
#[derive(Clone,Debug)]
pub struct PreparedClip { pub info:ClipInfo,pub project:Option<Project>,pub warning:Option<String>,pub visuals:VisualAssets }
#[derive(Clone,Debug,Serialize)]
pub struct ExportSummary { pub path:String,pub fps:u32,pub frames:u64,pub seconds:f64 }
static EXPORTING:AtomicBool=AtomicBool::new(false);
struct ExportGuard;
impl Drop for ExportGuard { fn drop(&mut self){EXPORTING.store(false,Ordering::Release);} }

pub fn engine_path()->Result<PathBuf,String> {
    if let Some(p)=std::env::var_os("RECALL_ENGINE"){let p=PathBuf::from(p);return if p.is_file(){Ok(p)}else{Err("RECALL_ENGINE does not name an executable".into())};}
    let exe=std::env::current_exe().map_err(|e|e.to_string())?;
    for p in exe.ancestors(){for candidate in [p.join("recall.exe"),p.join("target-dev/debug/recall.exe"),p.join("target/release/recall.exe")] {if candidate.is_file(){return Ok(candidate);}}}
    Err("Recorder executable not found. Build recall or set RECALL_ENGINE.".into())
}
fn hidden_engine()->Result<Command,String>{let mut c=Command::new(engine_path()?);c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(0x08004000);Ok(c)}
pub fn cache_dir()->Result<PathBuf,String>{
    let root=std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Recall").join("editor");
    std::fs::create_dir_all(&root).map_err(|e|format!("create editor cache: {e}"))?;Ok(root)
}
pub fn list_clips()->Result<Vec<ClipEntry>,String>{
    let dir=std::env::var_os("RECALL_OUT_DIR").map(PathBuf::from).unwrap_or_else(||PathBuf::from(r"V:\ALL RECORDINGS"));
    if !dir.exists(){return Ok(Vec::new());}
    let thumbs=thumb_index(&dir.join("thumbs"));
    let mut clips=Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e|format!("read clips: {e}"))?.flatten(){
        let path=e.path();if !path.extension().is_some_and(|e|e.eq_ignore_ascii_case("mp4")){continue;}
        let Ok(m)=e.metadata()else{continue};if !m.is_file(){continue;}
        let stem=path.file_stem().and_then(|s|s.to_str()).unwrap_or("");
        let modified=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default();
        let mtime=modified.as_secs();let modified_ms=modified.as_millis().min(u64::MAX as u128)as u64;
        let thumb=thumbs.get(stem).cloned().or_else(||visual_assets::cached_thumbnail(&path,m.len(),modified_ms));
        clips.push(ClipEntry{name:e.file_name().to_string_lossy().into_owned(),path:path.to_string_lossy().into_owned(),bytes:m.len(),mtime,thumb:thumb.map(|p|p.to_string_lossy().into_owned())});
    }
    clips.sort_by(|a,b|b.mtime.cmp(&a.mtime));Ok(clips)
}
fn thumb_index(directory:&Path)->HashMap<String,PathBuf>{
    let mut thumbs=HashMap::new();let Ok(entries)=std::fs::read_dir(directory)else{return thumbs};
    for entry in entries.flatten(){let path=entry.path();if !path.is_file(){continue;}let Some(name)=path.file_name().and_then(|name|name.to_str())else{continue};let Some(stem)=recorder_thumb_stem(name)else{continue};thumbs.entry(stem.to_owned()).or_insert(path);}
    thumbs
}
fn recorder_thumb_stem(name:&str)->Option<&str>{let name=name.strip_suffix(".jpg")?;let name=name.strip_suffix("-thumb")?;let(stem,epoch)=name.rsplit_once('-')?;(!stem.is_empty()&&epoch.len()>=9&&epoch.chars().all(|character|character.is_ascii_digit())).then_some(stem)}
/// Lazy thumbnail generation for library entries that predate recorder thumbnails.
pub fn thumbnail(path:&str)->Result<String,String>{visual_assets::thumbnail(path)}
fn sidecar(source:&str)->Result<PathBuf,String>{let p=Path::new(source);let name=p.file_name().ok_or("source has no filename")?.to_string_lossy();Ok(p.with_file_name(format!("{name}.recall-edit.json")))}
fn validate_info(info:&ClipInfo)->Result<(),String>{
    let p=std::fs::canonicalize(&info.path).map_err(|e|format!("resolve source: {e}"))?;
    let m=p.metadata().map_err(|e|e.to_string())?;
    let modified=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64;
    if !m.is_file()||m.len()!=info.source_bytes||modified!=info.source_modified_ms{return Err("Source changed since it was opened; reopen the clip.".into());}
    Ok(())
}
fn validate_project(project:&Project,info:&ClipInfo)->Result<(),String>{
    if project.version!=1||project.source_path!=info.path||project.source_bytes!=info.source_bytes||project.source_modified_ms!=info.source_modified_ms{return Err("Saved edits belong to another source revision.".into());}
    validate_fps(project.export_fps)?;
    project.plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())
}
pub fn prepare_clip(path:&str)->Result<PreparedClip,String>{
    let cache=cache_dir()?;
    let output=hidden_engine()?.arg("--prepare-editor").arg(path).arg(&cache).output().map_err(|e|format!("start media preparation: {e}"))?;
    if !output.status.success(){return Err(format!("prepare media: {}",String::from_utf8_lossy(&output.stderr).trim()));}
    let mut info:ClipInfo=serde_json::from_slice(&output.stdout).map_err(|e|format!("read prepared media: {e}"))?;
    normalize_audio_names(&mut info);
    let mut warning=None;
    let project=if let Ok(bytes)=std::fs::read(sidecar(&info.path)?){
        match serde_json::from_slice::<Project>(&bytes).map_err(|e|e.to_string()).and_then(|p|{validate_project(&p,&info)?;Ok(p)}){
            Ok(mut p)=>{normalize_project_audio(&mut p.plan,&info);Some(p)},Err(e)=>{warning=Some(format!("Saved edits were not loaded: {e}"));None}
        }
    }else{None};
    let (visuals,visual_warning)=visual_assets::visuals_best_effort(&info.path,info.duration);
    if let Some(visual_warning)=visual_warning { warning=Some(match warning { Some(existing)=>format!("{existing} {visual_warning}"),None=>visual_warning }); }
    Ok(PreparedClip{info,project,warning,visuals})
}
fn normalize_project_audio(plan:&mut EditPlan,info:&ClipInfo){
    let mut normalized=Vec::with_capacity(info.audio.len());
    for track in &info.audio { normalized.push(plan.audio.iter().find(|setting|setting.stream_index==track.stream_index).cloned().unwrap_or(crate::edit_model::AudioSetting{stream_index:track.stream_index,gain_db:0.0,muted:false})); }
    plan.audio=normalized;
}
fn normalize_audio_names(info:&mut ClipInfo){
    let generated=recall_generated_path(&info.path);
    let unnamed:Vec<_>=info.audio.iter().enumerate().filter_map(|(index,track)|generic_audio_name(&track.name).then_some(index)).collect();
    if generated&&info.audio.len()==2&&unnamed.len()==2 { info.audio[0].name="Game audio".into();info.audio[1].name="Microphone".into();return; }
    for (ordinal,track) in info.audio.iter_mut().enumerate(){if generic_audio_name(&track.name){track.name=format!("Audio track {}",ordinal+1);}}
}
fn generic_audio_name(name:&str)->bool{let value=name.trim().to_ascii_lowercase();if matches!(value.as_str(),""|"und"|"unknown"|"undefined"){return true;}if value.starts_with("audio ")&&value[6..].parse::<usize>().is_ok(){return true;}value.len()<=3&&value.chars().all(|c|c.is_ascii_alphabetic())}
fn recall_generated_path(path:&str)->bool{let stem=Path::new(path).file_stem().and_then(|value|value.to_str()).unwrap_or("");let parts:Vec<_>=stem.split("__").collect();parts.len()==4&&parts[1].len()==10&&parts[1].chars().enumerate().all(|(index,c)|matches!(index,4|7) && c=='-' || index!=4&&index!=7&&c.is_ascii_digit())&&parts[2].len()>=8&&parts[3].len()>=16&&parts[3].chars().all(|c|c.is_ascii_hexdigit())}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn recall_two_track_names_are_only_applied_to_matching_generated_sources(){
        let mut info=ClipInfo{path:"C:/clips/VALORANT__2026-10-17__22-34-31-275__3d260146faca4115835ba29741d71fe3.mp4".into(),duration:1.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![crate::edit_model::AudioTrackInfo{stream_index:1,name:"und".into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new()},crate::edit_model::AudioTrackInfo{stream_index:2,name:"und".into(),channels:1,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new()}],keyframes:Vec::new()};
        normalize_audio_names(&mut info);assert_eq!(info.audio[0].name,"Game audio");assert_eq!(info.audio[1].name,"Microphone");
        info.path="C:/clips/imported.mp4".into();info.audio[0].name="und".into();info.audio[1].name="unknown".into();normalize_audio_names(&mut info);assert_eq!(info.audio[0].name,"Audio track 1");assert_eq!(info.audio[1].name,"Audio track 2");
        assert_eq!(recorder_thumb_stem("my-hyphenated-game__2026-10-17__22-34-31-275__abcdef0123456789-1783463424-thumb.jpg"),Some("my-hyphenated-game__2026-10-17__22-34-31-275__abcdef0123456789"));
        assert_eq!(recorder_thumb_stem("ordinary-name-thumb.jpg"),None);
    }
}
fn validate_fps(fps:u32)->Result<(),String>{if fps==0||fps>i32::MAX as u32{Err("Export rate must be a positive integer within the supported range.".into())}else{Ok(())}}
pub fn save_project(info:&ClipInfo,plan:&EditPlan,fps:u32)->Result<String,String>{
    validate_info(info)?;validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;
    let project=Project{version:1,source_path:info.path.clone(),source_bytes:info.source_bytes,source_modified_ms:info.source_modified_ms,plan:plan.clone(),export_fps:fps};
    let path=sidecar(&info.path)?;let temp=path.with_extension(format!("{}.tmp",std::process::id()));
    std::fs::write(&temp,serde_json::to_vec_pretty(&project).map_err(|e|e.to_string())?).map_err(|e|format!("write edits: {e}"))?;
    std::fs::rename(&temp,&path).map_err(|e|format!("publish edits: {e}"))?;Ok(path.to_string_lossy().into_owned())
}
pub fn export_clip(info:&ClipInfo,plan:&EditPlan,fps:u32)->Result<ExportSummary,String>{
    validate_info(info)?;validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;
    if EXPORTING.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return Err("Another export is running.".into());}
    let _guard=ExportGuard;
    let nonce=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos();
    let source=Path::new(&info.path);let outdir=source.parent().ok_or("source has no directory")?.join("exports");
    std::fs::create_dir_all(&outdir).map_err(|e|format!("create export directory: {e}"))?;
    let output=outdir.join(format!("{}-edited-{fps}fps-{nonce}.mp4",source.file_stem().and_then(|v|v.to_str()).unwrap_or("clip")));
    let project=cache_dir()?.join(format!("export-{}-{nonce}.json",std::process::id()));
    std::fs::write(&project,serde_json::to_vec(plan).map_err(|e|e.to_string())?).map_err(|e|format!("write export plan: {e}"))?;
    let result=(||{
        let result=hidden_engine()?.arg("--export-edit").arg(source).arg(&output).arg(fps.to_string()).arg(&project).arg("--json").output().map_err(|e|format!("start export: {e}"))?;
        let events:Vec<serde_json::Value>=String::from_utf8_lossy(&result.stdout).lines().filter_map(|l|serde_json::from_str(l).ok()).collect();
        if !result.status.success(){return Err(events.iter().find(|v|v["type"]=="export_failed").and_then(|v|v["reason"].as_str()).map(str::to_owned).unwrap_or_else(||format!("Export failed: {}",String::from_utf8_lossy(&result.stderr).trim())));}
        let done=events.iter().find(|v|v["type"]=="exported").ok_or("Recorder returned no completed export.")?;
        Ok(ExportSummary{path:output.to_string_lossy().into_owned(),fps,frames:done["frames"].as_u64().ok_or("Missing frame count.")?,seconds:done["seconds"].as_f64().ok_or("Missing duration.")?})
    })();
    let _=std::fs::remove_file(&project);result
}
pub fn show_in_folder(path:&str){let _=Command::new("explorer.exe").arg(format!("/select,{path}")).creation_flags(0x08000000).spawn();}

