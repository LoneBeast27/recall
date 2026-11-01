//! Native client operations. Call media preparation/export on a background thread.
//!
//! Storage policy (see `services::storage`): clip reads on an HDD go through its disk lane;
//! opening a clip copies it to the SSD video tier first and prepares from the copy;
//! autosave writes the working project to the SSD and the sidecar beside the clip is only
//! written by [`flush_project`].
use crate::edit_model::{ClipInfo, EditPlan, ExportMode, ExportSettings};
use crate::services::library::{folders::{self, Folders},watch::WatchRoot};
use crate::services::storage::{self, Priority, ProjectSource};
use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
#[path = "visual_assets.rs"]
mod visual_assets;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClipEntry {
    pub name:String, pub path:String, pub bytes:u64, pub mtime:u64, pub thumb:Option<String>,
    /// Folder holding an imported clip: names the game when the file name does not.
    pub folder:Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project { pub version:u32,pub source_path:String,pub source_bytes:u64,pub source_modified_ms:u64,pub plan:EditPlan,pub export_fps:u32,#[serde(default)] pub export:ExportSettings }
#[derive(Clone,Debug,Default)]
pub struct VisualAssets { pub poster:Option<String>,pub filmstrip:Vec<String> }
#[derive(Clone,Debug)]
pub struct PreparedClip {
    pub info:ClipInfo,pub project:Option<Project>,pub warning:Option<String>,pub visuals:VisualAssets,
    /// SSD copy to play from (`None`: play `info.path` -- a clip already on an SSD, or one the video tier could not take).
    pub playback:Option<String>,
}
/// Settings > Storage: library and cache drive types plus per-tier cache usage.
#[derive(Clone,Debug)]
pub struct StorageReport { pub library_drives:Vec<storage::DriveInfo>,pub cache_drive:Option<storage::DriveInfo>,pub cache:storage::CacheUsage,pub notice:Option<String> }
#[derive(Clone,Debug,Serialize)]
pub struct ExportSummary { pub path:String,pub fps:Option<u32>,pub frames:u64,pub seconds:f64,pub bytes:u64,pub lossless:bool,pub width:u32,pub height:u32 }
static EXPORTING:AtomicBool=AtomicBool::new(false);
struct ExportGuard;
impl Drop for ExportGuard { fn drop(&mut self){EXPORTING.store(false,Ordering::Release);} }

pub fn engine_path()->Result<PathBuf,String> {
    if let Some(p)=std::env::var_os("RECALL_ENGINE"){let p=PathBuf::from(p);return if p.is_file(){Ok(p)}else{Err("RECALL_ENGINE does not name an executable".into())};}
    let exe=std::env::current_exe().map_err(|e|e.to_string())?;
    for p in exe.ancestors(){for candidate in [p.join("recall.exe"),p.join("target-dev/debug/recall.exe"),p.join("target/release/recall.exe")] {if candidate.is_file(){return Ok(candidate);}}}
    Err("Recorder executable not found. Build recall or set RECALL_ENGINE.".into())
}
fn engine_command()->Result<Command,String>{Ok(Command::new(engine_path()?))}
/// Engine media-preparation cache (`<cache>\editor`): WAV preview stems + `clip.json` per source.
pub fn cache_dir()->Result<PathBuf,String>{
    let root=storage::cache_root().join("editor");
    std::fs::create_dir_all(&root).map_err(|e|format!("create editor cache: {e}"))?;Ok(root)
}
fn tmp_dir()->Result<PathBuf,String>{let root=storage::cache_root().join("tmp");std::fs::create_dir_all(&root).map_err(|e|format!("create cache temp: {e}"))?;Ok(root)}
/// `RECALL_OUT_DIR`, else the recorder's output folder (last reported, kept in
/// `folders.json`), else `V:\ALL RECORDINGS`.
pub fn library_dir()->PathBuf{
    if let Some(dir)=std::env::var_os("RECALL_OUT_DIR"){return PathBuf::from(dir);}
    Folders::load().recordings.filter(|r|!r.trim().is_empty()).map(PathBuf::from).unwrap_or_else(||PathBuf::from(r"V:\ALL RECORDINGS"))
}
/// The folders [`list_clips`] reads, in its order.
pub fn clip_roots()->Vec<WatchRoot>{
    let mut roots=vec![WatchRoot{path:library_dir(),recursive:false}];
    roots.extend(Folders::load().imported.into_iter().map(|folder|WatchRoot{path:PathBuf::from(folder),recursive:true}));
    roots
}
/// The recordings folder (one listing) plus every imported folder (searched a few levels
/// deep), and one listing of the SSD thumbnail tier; a clip's `thumb` is set only when its
/// thumbnail is already on the SSD (misses are filled by `AssetJob::Thumb` and idle prefetch
/// through the disk lane). A clip reachable from two folders is listed once.
pub fn list_clips()->Result<Vec<ClipEntry>,String>{
    let dir=library_dir();
    let thumbs=storage::thumb_index();
    let mut clips=Vec::new();
    let mut seen=HashSet::new();
    let mut push=|path:PathBuf,bytes:u64,mtime:u64,folder:Option<String>|{
        if !seen.insert(path.to_string_lossy().to_lowercase()){return;}
        let name=path.file_name().map(|n|n.to_string_lossy().into_owned()).unwrap_or_default();
        let thumb=thumbs.lookup(&name,bytes,mtime).map(|p|p.to_string_lossy().into_owned());
        clips.push(ClipEntry{name,path:path.to_string_lossy().into_owned(),bytes,mtime,thumb,folder});
    };
    if dir.exists(){
        for e in std::fs::read_dir(&dir).map_err(|e|format!("read clips: {e}"))?.flatten(){
            let path=e.path();if !folders::is_clip(&path){continue;}
            let Ok(m)=e.metadata()else{continue};if !m.is_file(){continue;}
            let mtime=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            push(path,m.len(),mtime,None);
        }
    }
    let skip=[storage::cache_root()];
    for root in Folders::load().imported{
        for clip in folders::scan_imported_cached(Path::new(&root),&skip){push(clip.path,clip.bytes,clip.mtime,clip.folder);}
    }
    clips.sort_by(|a,b|b.mtime.cmp(&a.mtime));Ok(clips)
}
/// SSD thumbnail for a library clip: cached, imported from the recorder's `thumbs` folder, or generated.
/// Blocking; HDD work runs on the clip's disk lane (inline when already on it).
pub fn thumbnail(path:&str)->Result<String,String>{storage::ensure_thumb(path)}
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
fn modified_ms(m:&std::fs::Metadata)->u64{m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64}
/// Engine input for a clip: whichever of (SSD copy, original) already has an engine cache
/// entry (no re-decode), else the copy (fast reads), else the original.
fn engine_input(source:&Path,metadata:&std::fs::Metadata,copy:Option<&Path>)->PathBuf{
    let editor=storage::cache_root().join("editor");let (bytes,ms)=(metadata.len(),modified_ms(metadata));
    let has=|input:&Path|storage::engine_cache_dir(&editor,input,bytes,ms).join("clip.json").is_file();
    match copy.and_then(|c|std::fs::canonicalize(c).ok()){
        Some(copy) if has(&copy)=>copy,
        _ if has(source)=>source.to_path_buf(),
        Some(copy)=>copy,
        None=>source.to_path_buf(),
    }
}
/// `recall.exe --prepare-editor`: audio stems + waveforms into `<cache>\editor` (a cache hit reads
/// only `clip.json`). `lowered` starts the engine at very low I/O priority (HDD input).
/// Opening a clip the smart cache is preparing at the same moment races two engines to publish
/// one cache entry; the loser fails ("publish cache") although the entry is now complete, so it
/// asks once more and reads the winner's entry.
fn run_engine_prepare(input:&Path,lowered:bool)->Result<ClipInfo,String>{
    match run_engine_prepare_once(input,lowered){
        Err(error) if lost_publish_race(&error)=>run_engine_prepare_once(input,lowered),
        result=>result,
    }
}
fn lost_publish_race(error:&str)->bool{error.contains("publish cache:")}
fn run_engine_prepare_once(input:&Path,lowered:bool)->Result<ClipInfo,String>{
    let mut command=engine_command()?;command.arg("--prepare-editor").arg(input).arg(cache_dir()?);
    let output=storage::run_hidden(&mut command,lowered).map_err(|e|format!("start media preparation: {e}"))?;
    if !output.status.success(){return Err(format!("prepare media: {}",String::from_utf8_lossy(&output.stderr).trim()));}
    serde_json::from_slice(&output.stdout).map_err(|e|format!("read prepared media: {e}"))
}
/// Engine folders holding a prepared clip's audio stems.
fn editor_dirs(info:&ClipInfo)->Vec<PathBuf>{
    let mut dirs:Vec<PathBuf>=info.audio.iter().filter_map(|a|Path::new(&a.preview_path).parent().map(Path::to_path_buf)).collect();
    dirs.sort();dirs.dedup();dirs
}
fn folder_bytes(dir:&Path)->u64{std::fs::read_dir(dir).map(|l|l.flatten().filter_map(|e|e.metadata().ok()).filter(|m|m.is_file()).map(|m|m.len()).sum()).unwrap_or(0)}
/// HDD clip: copy it to the SSD video tier (one sequential pass on its disk lane), then prepare
/// audio stems and visuals from the copy (or reuse what the smart cache already prepared). A
/// clip that cannot be cached is prepared in place on the lane at very low I/O priority.
/// `info.path` always names the original.
pub fn prepare_clip(path:&str)->Result<PreparedClip,String>{
    let source=std::fs::canonicalize(path).map_err(|e|format!("resolve source: {e}"))?;
    let metadata=source.metadata().map_err(|e|format!("stat source: {e}"))?;
    if !metadata.is_file(){return Err("source is not a file".into());}
    let on_hdd=storage::drive_kind(&source).is_hdd();
    let mut warnings=Vec::new();
    // Too big for the video tier (or no room): play from the library drive, no warning needed.
    let copy=if on_hdd{storage::cache_video(&source.to_string_lossy()).ok().filter(|c|Path::new(c)!=source.as_path())}else{None};
    let input=engine_input(&source,&metadata,copy.as_deref().map(Path::new));
    let frames_from=copy.as_ref().map(PathBuf::from).unwrap_or_else(||source.clone());
    let in_place=on_hdd&&copy.is_none();
    let original=source.clone();
    let work=move||->Result<(ClipInfo,VisualAssets,Option<String>),String>{
        let info=run_engine_prepare(&input,in_place&&storage::drive_kind(&input).is_hdd())?;
        let (visuals,warning)=visual_assets::visuals_best_effort(&original,&frames_from,info.duration);
        Ok((info,visuals,warning))
    };
    let (mut info,visuals,visual_warning)=if in_place{storage::run_io(&source,Priority::Interactive,work)??}else{work()?};
    // The copy is byte-identical and stamped with the source's mtime; pin identity to the original.
    info.path=source.to_string_lossy().into_owned();info.source_bytes=metadata.len();info.source_modified_ms=modified_ms(&metadata);
    normalize_audio_names(&mut info);
    storage::note_editor_media(&info.path,&editor_dirs(&info),true);
    storage::enforce_budget();
    let (project,project_warning)=load_project(&info);
    warnings.extend(project_warning);warnings.extend(visual_warning);
    let warning=(!warnings.is_empty()).then(||warnings.join(" "));
    Ok(PreparedClip{info,project,warning,visuals,playback:copy})
}
/// Smart-cache fill step (runs on the clip's disk lane): prepares editor media — audio stems,
/// waveforms, poster, filmstrip — without opening the clip, from the SSD copy when there is
/// one. Returns the bytes now cached for it.
pub fn prefetch_editor_media(path:&str)->Result<u64,String>{
    let source=std::fs::canonicalize(path).map_err(|e|format!("resolve source: {e}"))?;
    let metadata=source.metadata().map_err(|e|format!("stat source: {e}"))?;
    let copy=storage::cached_video(&source.to_string_lossy()).map(PathBuf::from);
    let input=engine_input(&source,&metadata,copy.as_deref());
    let info=run_engine_prepare(&input,storage::drive_kind(&input).is_hdd())?;
    let frames_from=copy.unwrap_or_else(||source.clone());
    let (visuals,_)=visual_assets::visuals_best_effort(&source,&frames_from,info.duration);
    let dirs=editor_dirs(&info);
    storage::note_editor_media(&source.to_string_lossy(),&dirs,false);
    let frames:u64=visuals.poster.iter().chain(visuals.filmstrip.iter()).filter_map(|p|std::fs::metadata(p).ok()).map(|m|m.len()).sum();
    Ok(dirs.iter().map(|d|folder_bytes(d)).sum::<u64>()+frames)
}
fn working_path(info:&ClipInfo)->Result<PathBuf,String>{
    let name=Path::new(&info.path).file_name().ok_or("source has no filename")?.to_string_lossy().into_owned();
    Ok(storage::working_copy_path(&storage::cache_root(),&name,info.source_bytes,info.source_modified_ms/1000))
}
/// Newer of the SSD working copy and the sidecar (falling back to the other when the newer one is invalid).
fn load_project(info:&ClipInfo)->(Option<Project>,Option<String>){
    let (Ok(working),Ok(sidecar))=(working_path(info),sidecar(&info.path))else{return(None,None)};
    let lane_sidecar=sidecar.clone();
    let (sidecar_ms,sidecar_bytes)=storage::run_io(Path::new(&info.path),Priority::Interactive,move||(storage::projects_modified_ms(&lane_sidecar),std::fs::read(&lane_sidecar).ok())).unwrap_or((None,None));
    let mut warning=None;
    for source in storage::project_order(storage::projects_modified_ms(&working),sidecar_ms.filter(|_|sidecar_bytes.is_some())){
        let bytes=match source{ProjectSource::WorkingCopy=>std::fs::read(&working).ok(),ProjectSource::Sidecar=>sidecar_bytes.clone()};
        let Some(bytes)=bytes else{continue};
        match serde_json::from_slice::<Project>(&bytes).map_err(|e|e.to_string()).and_then(|p|{validate_project(&p,info)?;Ok(p)}){
            Ok(mut p)=>{normalize_project_audio(&mut p.plan,info);return(Some(p),warning)},
            Err(e)=>{warning.get_or_insert(format!("Saved edits were not loaded: {e}"));}
        }
    }
    (None,warning)
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
fn generic_audio_name(name:&str)->bool{let value=name.trim().to_ascii_lowercase();if matches!(value.as_str(),""|"und"|"unknown"|"undefined"){return true;}if value.starts_with("audio ")&&value[6..].parse::<usize>().is_ok(){return true;}language_code(name.trim())}
/// A stream's language used as a name: 1-3 letters of one case ("eng", "EN"), which a real label
/// is not ("Mic"). The labels a tool may spell like a code stay names, though "mic" is in ISO 639-2.
fn language_code(name:&str)->bool{
    let one_case=name.chars().all(|c|c.is_ascii_lowercase())||name.chars().all(|c|c.is_ascii_uppercase());
    !name.is_empty()&&name.len()<=3&&one_case&&!matches!(name.to_ascii_lowercase().as_str(),"mic"|"sfx"|"bgm")
}
fn recall_generated_path(path:&str)->bool{let stem=Path::new(path).file_stem().and_then(|value|value.to_str()).unwrap_or("");let parts:Vec<_>=stem.split("__").collect();parts.len()==4&&parts[1].len()==10&&parts[1].chars().enumerate().all(|(index,c)|matches!(index,4|7) && c=='-' || index!=4&&index!=7&&c.is_ascii_digit())&&parts[2].len()>=8&&parts[3].len()>=16&&parts[3].chars().all(|c|c.is_ascii_hexdigit())}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn recall_two_track_names_are_only_applied_to_matching_generated_sources(){
        let mut info=ClipInfo{path:"C:/clips/VALORANT__2026-10-17__22-34-31-275__3d260146faca4115835ba29741d71fe3.mp4".into(),duration:1.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![crate::edit_model::AudioTrackInfo{stream_index:1,name:"und".into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new()},crate::edit_model::AudioTrackInfo{stream_index:2,name:"und".into(),channels:1,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new()}],keyframes:Vec::new()};
        normalize_audio_names(&mut info);assert_eq!(info.audio[0].name,"Game audio");assert_eq!(info.audio[1].name,"Microphone");
        info.path="C:/clips/imported.mp4".into();info.audio[0].name="und".into();info.audio[1].name="unknown".into();normalize_audio_names(&mut info);assert_eq!(info.audio[0].name,"Audio track 1");assert_eq!(info.audio[1].name,"Audio track 2");
        assert_eq!(storage::recorder_thumb_stem("my-hyphenated-game__2026-10-17__22-34-31-275__abcdef0123456789-1783463424-thumb.jpg"),Some("my-hyphenated-game__2026-10-17__22-34-31-275__abcdef0123456789"));
        assert_eq!(storage::recorder_thumb_stem("ordinary-name-thumb.jpg"),None);
    }
    #[test] fn short_track_labels_stay_and_language_codes_do_not(){
        for label in ["Mic","mic","MIC","SFX","Bgm","Sub","Tom"]{assert!(!generic_audio_name(label),"{label}");}
        for code in ["eng","en","fre","jpn","ENG","und"," eng ","","a"]{assert!(generic_audio_name(code),"{code}");}
        let track=|name:&str,stream_index|crate::edit_model::AudioTrackInfo{stream_index,name:name.into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new()};
        let mut info=ClipInfo{path:"V:/clips/VALORANT__2025-03-01__20-10-11.mp4".into(),duration:1.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![track("eng",1),track("Game",2),track("Mic",3)],keyframes:Vec::new()};
        normalize_audio_names(&mut info);
        assert_eq!(info.audio.iter().map(|a|a.name.as_str()).collect::<Vec<_>>(),["Audio track 1","Game","Mic"]);
    }
    #[test] fn a_lost_publish_race_is_retried_and_other_failures_are_not(){
        assert!(lost_publish_race("prepare media: prepare editor: publish cache: The directory is not empty. (os error 145)"));
        assert!(!lost_publish_race("prepare media: open clip.mp4: Invalid data found when processing input"));
    }
    #[test] fn only_real_edits_count_as_non_default_plans(){
        let info=ClipInfo{path:"C:/clips/a.mp4".into(),duration:10.0,width:1,height:1,video_fps:60.0,source_bytes:1,source_modified_ms:1,audio:vec![crate::edit_model::AudioTrackInfo{stream_index:1,name:"Game audio".into(),channels:2,sample_rate:48_000,preview_path:String::new(),waveform:Vec::new()}],keyframes:Vec::new()};
        let untouched=EditPlan{ranges:vec![crate::edit_model::TimeRange::new(0.0,10.0)],audio:vec![crate::edit_model::AudioSetting{stream_index:1,gain_db:0.0,muted:false}]};
        assert!(plan_is_default(&untouched,&info));
        let mut trimmed=untouched.clone();trimmed.ranges[0].start=1.5;assert!(!plan_is_default(&trimmed,&info));
        let mut split=untouched.clone();split.ranges=vec![crate::edit_model::TimeRange::new(0.0,4.0),crate::edit_model::TimeRange::new(4.0,10.0)];assert!(!plan_is_default(&split,&info));
        let mut muted=untouched.clone();muted.audio[0].muted=true;assert!(!plan_is_default(&muted,&info));
    }
}
fn validate_fps(fps:u32)->Result<(),String>{if fps==0||fps>i32::MAX as u32{Err("Export rate must be a positive integer within the supported range.".into())}else{Ok(())}}
fn project_bytes(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<Vec<u8>,String>{
    validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;
    let project=Project{version:1,source_path:info.path.clone(),source_bytes:info.source_bytes,source_modified_ms:info.source_modified_ms,plan:plan.clone(),export_fps:fps,export:export.clone()};
    serde_json::to_vec_pretty(&project).map_err(|e|e.to_string())
}
/// Autosave: writes the working project to the SSD (`cache\projects\<id>.json`, atomic). Touches nothing
/// on the library drive. Returns the working-copy path.
pub fn save_project(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<String,String>{
    let bytes=project_bytes(info,plan,fps,export)?;let path=working_path(info)?;
    storage::write_atomic(&path,&bytes)?;
    // The first real edit also writes the sidecar beside the clip (once): the recorder's storage
    // limit keeps clips that have one, and an edit made before a crash stays protected.
    if !plan_is_default(plan,info)&&sidecar(&info.path).is_ok_and(|s|!s.exists()){let _=flush_project(info,plan,fps,export);}
    Ok(path.to_string_lossy().into_owned())
}
/// Untouched plan: one full-length range and every track at 0 dB, unmuted.
fn plan_is_default(plan:&EditPlan,info:&ClipInfo)->bool{
    let full=matches!(plan.ranges.as_slice(),[only] if only.start.abs()<1e-6&&(only.end-info.duration).abs()<1e-3&&only.speed==1.0);
    full&&plan.audio.iter().all(|a|a.gain_db==0.0&&!a.muted)
}
/// Leave / close / export: writes the `.recall-edit.json` sidecar beside the clip, through the clip's
/// disk lane, only when its bytes would change (and, with no sidecar yet, only for real edits).
/// Ok(sidecar path) when the sidecar is current; Ok("") when there was nothing to write.
pub fn flush_project(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<String,String>{
    validate_info(info)?;let bytes=project_bytes(info,plan,fps,export)?;
    let working=working_path(info)?;let working_exists=working.is_file();let default=plan_is_default(plan,info);
    let path=sidecar(&info.path)?;let lane_bytes=bytes.clone();
    let written=storage::run_io(Path::new(&info.path),Priority::Interactive,move||->Result<Option<String>,String>{
        let existing=std::fs::read(&path).ok();
        if !storage::should_flush(existing.as_deref(),&lane_bytes,working_exists,default){return Ok(existing.map(|_|path.to_string_lossy().into_owned()));}
        storage::write_atomic(&path,&lane_bytes)?;Ok(Some(path.to_string_lossy().into_owned()))
    })??;
    if written.is_some(){storage::write_atomic(&working,&bytes)?;}
    Ok(written.unwrap_or_default())
}
pub fn export_clip(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<ExportSummary,String>{
    validate_info(info)?;validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;export.validate(plan)?;
    let lossless=export.mode==ExportMode::Lossless;
    if EXPORTING.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return Err("Another export is running.".into());}
    let _guard=ExportGuard;
    let nonce=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_nanos();
    let source=Path::new(&info.path);let outdir=source.parent().ok_or("source has no directory")?.join("exports");
    std::fs::create_dir_all(&outdir).map_err(|e|format!("create export directory: {e}"))?;
    let stem=source.file_stem().and_then(|v|v.to_str()).unwrap_or("clip");
    let output=if lossless{outdir.join(format!("{stem}-trim-{nonce}.mp4"))}else{outdir.join(format!("{stem}-edited-{fps}fps-{nonce}.mp4"))};
    // Read the SSD copy when one exists; the output still lands beside the original.
    let input=storage::cached_video(&info.path).map(PathBuf::from).unwrap_or_else(||source.to_path_buf());
    let lowered=storage::drive_kind(&outdir).is_hdd()||storage::drive_kind(&input).is_hdd();
    let project=tmp_dir()?.join(format!("export-{}-{nonce}.json",std::process::id()));
    std::fs::write(&project,serde_json::to_vec(&serde_json::json!({"plan":plan,"export":export})).map_err(|e|e.to_string())?).map_err(|e|format!("write export plan: {e}"))?;
    let result=(||{
        let mut command=engine_command()?;command.arg("--export-edit").arg(&input).arg(&output).arg(fps.to_string()).arg(&project).arg("--json");
        // The export holds the output drive's lane: prefetch pauses instead of seeking against it.
        let result=storage::run_io(&outdir,Priority::Interactive,move||storage::run_hidden(&mut command,lowered))?.map_err(|e|format!("start export: {e}"))?;
        let events:Vec<serde_json::Value>=String::from_utf8_lossy(&result.stdout).lines().filter_map(|l|serde_json::from_str(l).ok()).collect();
        if !result.status.success(){return Err(events.iter().find(|v|v["type"]=="export_failed").and_then(|v|v["reason"].as_str()).map(str::to_owned).unwrap_or_else(||format!("Export failed: {}",String::from_utf8_lossy(&result.stderr).trim())));}
        let done=events.iter().find(|v|v["type"]=="exported").ok_or("Recorder returned no completed export.")?;
        let report=events.iter().find(|v|v["type"]=="export_report");
        let field=|k:&str|report.and_then(|r|r[k].as_u64());
        let bytes=field("bytes").or_else(||std::fs::metadata(&output).ok().map(|m|m.len())).unwrap_or(0);
        Ok(ExportSummary{path:output.to_string_lossy().into_owned(),fps:if lossless{None}else{Some(fps)},frames:done["frames"].as_u64().ok_or("Missing frame count.")?,seconds:done["seconds"].as_f64().ok_or("Missing duration.")?,bytes,lossless,width:field("width").unwrap_or(0) as u32,height:field("height").unwrap_or(0) as u32})
    })();
    let _=std::fs::remove_file(&project);result
}
/// Drive types and cache usage for Settings (walks the cache: call from a worker thread).
pub fn storage_report(notice:Option<String>)->StorageReport{
    let cache=storage::usage_detail();
    StorageReport{library_drives:storage::drive_info(&library_dir()).into_iter().collect(),cache_drive:storage::drive_info(Path::new(&cache.root)),cache,notice}
}
pub fn show_in_folder(path:&str){let _=Command::new("explorer.exe").arg(format!("/select,{path}")).creation_flags(0x08000000).spawn();}

