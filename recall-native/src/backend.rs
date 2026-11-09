//! Native client operations. Call media preparation/export on a background thread.
//!
//! Storage policy (see `services::storage`): clip reads on an HDD go through its disk lane;
//! opening a clip copies it to the SSD video tier first and prepares from the copy;
//! autosave writes the working project to the SSD and the sidecar beside the clip is only
//! written by [`flush_project`].
use crate::edit_model::{ClipInfo, EditPlan, ExportMode, ExportSettings};
use crate::services::library::{folders::{self, Folders},import_check,watch::WatchRoot};
use crate::services::storage::{self, Priority, ProjectSource};
use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::os::windows::process::CommandExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[path = "visual_assets.rs"]
mod visual_assets;
#[path = "scene_strip.rs"]
pub mod scene_strip;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClipEntry {
    pub name:String, pub path:String, pub bytes:u64, pub mtime:u64, pub thumb:Option<String>,
    /// Folder holding an imported clip: names the game when the file name does not.
    pub folder:Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project { pub version:u32,pub source_path:String,pub source_bytes:u64,pub source_modified_ms:u64,pub plan:EditPlan,pub export_fps:u32,#[serde(default)] pub export:ExportSettings }
#[derive(Clone,Debug,Default)]
/// `aspect`: the clip picture's width / height (0 when unknown).
pub struct VisualAssets { pub poster:Option<String>,pub filmstrip:Vec<scene_strip::StripFrame>,pub aspect:f32 }
#[derive(Clone,Debug)]
pub struct PreparedClip {
    pub info:ClipInfo,pub project:Option<Project>,pub warning:Option<String>,pub visuals:VisualAssets,
    /// This session may rewrite or remove the sidecar: there was none, or it loaded.
    pub sidecar_ours:bool,
    /// A sidecar was there at open; one that appears later is not this session's to remove.
    pub sidecar_at_open:bool,
    /// SSD copy to play from (`None`: play `info.path` -- a clip already on an SSD, or one the video tier could not take).
    pub playback:Option<String>,
    /// Where the file's picture ends (`services::library::video_limits`).
    pub limits:(Option<f64>,Option<f64>),
    /// Bookmarks from beside the clip.
    pub marks:Vec<f64>,
}
/// Settings > Storage: library and cache drive types plus per-tier cache usage.
#[derive(Clone,Debug)]
pub struct StorageReport { pub library_drives:Vec<storage::DriveInfo>,pub cache_drive:Option<storage::DriveInfo>,pub cache:storage::CacheUsage,pub notice:Option<String> }
#[derive(Clone,Debug,Serialize)]
pub struct ExportSummary { pub path:String,pub fps:Option<u32>,pub frames:u64,pub seconds:f64,pub bytes:u64,pub lossless:bool,pub width:u32,pub height:u32 }
static EXPORTING:AtomicBool=AtomicBool::new(false);
static EXPORT_CANCEL:AtomicBool=AtomicBool::new(false);
static EXPORT_QUEUED:AtomicBool=AtomicBool::new(false);
/// The error of an export the person stopped.
pub const EXPORT_CANCELLED:&str="Export cancelled.";
/// Before an export is queued: an earlier cancel must not end this one.
pub fn arm_export(){EXPORT_CANCEL.store(false,Ordering::Release);EXPORT_QUEUED.store(true,Ordering::Release);}
pub fn cancel_export(){EXPORT_CANCEL.store(true,Ordering::Release);}
/// For a closing app, whose exit would end the engine: returns when the queued export has ended.
/// Its own time limit bounds a running export; 60 s bounds one the worker never starts.
pub fn wait_for_export(){
    let start=std::time::Instant::now();
    while EXPORT_QUEUED.load(Ordering::Acquire)&&(EXPORTING.load(Ordering::Acquire)||crate::services::pre_encode::awaited()||start.elapsed().as_secs()<60){std::thread::sleep(Duration::from_millis(50));}
}
/// The engine's hidden temporary output, left behind when it is stopped mid-write.
fn remove_export_temps(output:&Path,pid:u32){
    let (Some(outdir),Some(stem))=(output.parent(),output.file_stem().and_then(|v|v.to_str())) else{return};
    let prefix=format!(".{stem}.recall-export-{pid}-");
    for item in std::fs::read_dir(outdir).into_iter().flatten().flatten(){if item.file_name().to_string_lossy().starts_with(&prefix){let _=std::fs::remove_file(item.path());}}
}
struct ExportGuard;
impl Drop for ExportGuard { fn drop(&mut self){EXPORTING.store(false,Ordering::Release);} }
struct ExportQueueGuard;
impl Drop for ExportQueueGuard { fn drop(&mut self){EXPORT_QUEUED.store(false,Ordering::Release);} }

pub fn engine_path()->Result<PathBuf,String> {
    if let Some(p)=std::env::var_os("RECALL_ENGINE"){let p=PathBuf::from(p);return if p.is_file(){Ok(p)}else{Err("RECALL_ENGINE does not name an executable".into())};}
    let exe=std::env::current_exe().map_err(|e|e.to_string())?;
    for p in exe.ancestors(){for candidate in [p.join("recall.exe"),p.join("target-dev/debug/recall.exe")] {if candidate.is_file(){return Ok(candidate);}}}
    Err("Recorder executable not found. Build recall or set RECALL_ENGINE.".into())
}
/// What an engine build stamps in its program file: `recall-engine-caps:<cap>,<cap>;`
/// (`ENGINE_CAPS` in `recall/src/main.rs`). Flag names are not searched for: a release build
/// holds them split up, and a build without encode holds them while they do nothing.
const CAPS_MARK:&[u8]=b"recall-engine-caps:";
/// An engine that can do `cap`, set up to run it. Unknown arguments start an engine as a recorder
/// that never returns, so a build without it must not be run to find out: the check reads the program file.
fn engine_command(cap:&str)->Result<Command,String>{
    let path=engine_path()?;
    if !engine_knows(&path,cap)?{return Err(format!("The recorder at {} is too old for this version of Recall (no {cap}).",path.display()));}
    let mut command=Command::new(path);command.arg(format!("--{cap}"));Ok(command)
}
fn engine_knows(path:&Path,cap:&str)->Result<bool,String>{
    static KNOWN:Mutex<Option<HashMap<(PathBuf,u64,u64,String),bool>>>=Mutex::new(None);
    let m=path.metadata().map_err(|e|format!("stat recorder: {e}"))?;
    let key=(path.to_path_buf(),m.len(),modified_ms(&m),cap.to_owned());
    let mut known=KNOWN.lock().unwrap_or_else(|e|e.into_inner());
    if let Some(answer)=known.get_or_insert_with(HashMap::new).get(&key){return Ok(*answer);}
    let bytes=std::fs::read(path).map_err(|e|format!("read recorder: {e}"))?;
    let answer=stamped_caps(&bytes).iter().any(|stamped|*stamped==cap.as_bytes());
    known.get_or_insert_with(HashMap::new).insert(key,answer);
    Ok(answer)
}
/// The capabilities stamped in a program file.
fn stamped_caps(bytes:&[u8])->Vec<&[u8]>{
    let mut caps=Vec::new();
    for at in bytes.windows(CAPS_MARK.len()).enumerate().filter(|(_,w)|w[0]==CAPS_MARK[0]&&*w==CAPS_MARK).map(|(at,_)|at){
        let list=&bytes[at+CAPS_MARK.len()..];
        let Some(end)=list.iter().take(256).position(|b|*b==b';')else{continue};
        caps.extend(list[..end].split(|b|*b==b','));
    }
    caps
}
/// The engine has already prepared this clip in place.
pub(crate) fn has_prepared(source:&Path,metadata:&std::fs::Metadata)->bool{
    storage::engine_cache_dir(&storage::cache_root().join("editor"),source,metadata.len(),modified_ms(metadata)).join("clip.json").is_file()
}
/// Preparing reads the whole clip once per track; past this it is not coming back.
fn prepare_limit(bytes:u64)->Duration{Duration::from_secs(120+bytes/(2_000_000))}
/// Engine media-preparation cache (`<cache>\editor`): WAV preview stems + `clip.json` per source.
pub fn cache_dir()->Result<PathBuf,String>{
    let root=storage::cache_root().join("editor");
    std::fs::create_dir_all(&root).map_err(|e|format!("create editor cache: {e}"))?;Ok(root)
}
fn tmp_dir()->Result<PathBuf,String>{let root=storage::cache_root().join("tmp");std::fs::create_dir_all(&root).map_err(|e|format!("create cache temp: {e}"))?;Ok(root)}
/// `RECALL_OUT_DIR`, else the recorder's output folder (last reported, kept in
/// `folders.json`), else `<Videos>\Recall` (the recorder's default).
pub fn library_dir()->PathBuf{library_dir_from(&Folders::load())}
/// [`library_dir`] from folders already in memory (the UI's copy), so a render never reads the file.
pub fn library_dir_from(folders:&Folders)->PathBuf{
    if let Some(dir)=std::env::var_os("RECALL_OUT_DIR"){return PathBuf::from(dir);}
    folders.recordings.clone().filter(|r|!r.trim().is_empty()).map(PathBuf::from).unwrap_or_else(crate::services::library::folders::default_recordings_dir)
}
/// The folders [`list_clips`] reads, in its order.
/// From folders already read (the listing's copy).
pub fn clip_roots_from(f:&Folders)->Vec<WatchRoot>{
    let recordings=library_dir_from(f);
    // Game folders ("Sort clips into game folders"), each watched like the recordings folder; a
    // new one is picked up by the listing after its first save (every save ends in a listing).
    let games=folders::game_folders(&recordings);
    let mut roots=vec![WatchRoot{path:recordings,recursive:false}];
    roots.extend(games.into_iter().map(|path|WatchRoot{path,recursive:false}));
    roots.extend(f.imported.iter().map(|folder|WatchRoot{path:PathBuf::from(folder),recursive:true}));
    roots
}
/// The recordings folder (one listing, plus one of each game folder in it) plus every imported folder (searched a few levels
/// deep), and one listing of the SSD thumbnail tier; a clip's `thumb` is set only when its
/// thumbnail is already on the SSD (misses are filled by `AssetJob::Thumb` and idle prefetch
/// through the disk lane). A clip reachable from two folders is listed once.
pub fn list_clips()->Result<Vec<ClipEntry>,String>{list_clips_checked_from(&Folders::load()).map(|(clips,_)|clips)}
/// [`list_clips`], and whether the listing saw every configured folder whole (each existed, was
/// read, and no entry failed). Only a complete listing may prove a clip is gone.
/// From folders already read (one read per listing).
pub fn list_clips_checked_from(f:&Folders)->Result<(Vec<ClipEntry>,bool),String>{list_from(&library_dir_from(f),&f.imported,&f.former_recordings)}
pub(crate) fn list_from(dir:&Path,imported:&[String],former:&[String])->Result<(Vec<ClipEntry>,bool),String>{
    let thumbs=storage::thumb_index();
    let mut complete=true;
    let mut clips=Vec::new();
    let mut seen=HashSet::new();
    let mut push=|path:PathBuf,bytes:u64,mtime:u64,folder:Option<String>|{
        if !seen.insert(path.to_string_lossy().to_lowercase()){return;}
        let name=path.file_name().map(|n|n.to_string_lossy().into_owned()).unwrap_or_default();
        let thumb=thumbs.lookup(&name,bytes,mtime).map(|p|p.to_string_lossy().into_owned());
        clips.push(ClipEntry{name,path:path.to_string_lossy().into_owned(),bytes,mtime,thumb,folder});
    };
    if dir.exists(){
        for e in std::fs::read_dir(dir).map_err(|e|format!("read clips: {e}"))?{
            let Ok(e)=e else{complete=false;continue};
            let path=e.path();if !folders::is_clip(&path){continue;}
            let Ok(m)=e.metadata()else{complete=false;continue};if !m.is_file(){continue;}
            let mtime=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            push(path,m.len(),mtime,None);
        }
        // Game folders one level down: recorder clips only (`folders::in_game_folder`).
        for game in folders::game_folders(dir){
            let Ok(listing)=std::fs::read_dir(&game)else{complete=false;continue};
            for e in listing{
                let Ok(e)=e else{complete=false;continue};
                let path=e.path();if !folders::is_recall_named(&e.file_name().to_string_lossy()){continue;}
                let Ok(m)=e.metadata()else{complete=false;continue};if !m.is_file(){continue;}
                let mtime=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                push(path,m.len(),mtime,None);
            }
        }
    }else{complete=false;}
    let skip=[storage::cache_root()];
    for root in imported{
        let (found,whole)=folders::scan_imported_cached_checked(Path::new(root),&skip);
        complete&=whole;
        for clip in import_check::listable(found,dir,imported,former){push(clip.path,clip.bytes,clip.mtime,clip.folder);}
    }
    clips.sort_by(|a,b|b.mtime.cmp(&a.mtime));Ok((clips,complete))
}
/// SSD thumbnail for a library clip: cached, imported from the recorder's `thumbs` folder, or generated.
/// Blocking; HDD work runs on the clip's disk lane (inline when already on it).
pub fn thumbnail(path:&str)->Result<String,String>{storage::ensure_thumb(path)}
// The editor save: beside the clip, or on C: for a clip in an imported folder (`backend_edit_place.rs`).
#[path="backend_edit_place.rs"] mod edit_place;
pub(crate) use edit_place::{checked_export_dir, export_dir_for, folders_now, link_blocks_write};
use edit_place::sidecar;
#[allow(unused_imports)] // the examples build backend.rs without Root, its only caller
pub use edit_place::share_folders;
pub(crate) fn validate_info(info:&ClipInfo)->Result<(),String>{
    let p=std::fs::canonicalize(&info.path).map_err(|e|format!("resolve source: {e}"))?;
    let m=p.metadata().map_err(|e|e.to_string())?;
    let modified=m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64;
    if !m.is_file()||m.len()!=info.source_bytes||modified!=info.source_modified_ms{return Err("Source changed since it was opened; reopen the clip.".into());}
    Ok(())
}
/// Saved edits belong to the clip by name, size and write time, not by its absolute path: a USB
/// drive back under another letter, or a moved clips folder, keeps them. The next flush writes
/// the new path.
/// A byte-identical copy (Explorer keeps the write time) shares the SSD working copy, so the
/// working copy counts for another path only when that path is gone (a move, not a copy). The
/// sidecar beside the clip always counts: its place names the clip.
fn validate_project(project:&Project,info:&ClipInfo,source:ProjectSource)->Result<(),String>{
    let elsewhere=!project.source_path.eq_ignore_ascii_case(&info.path)&&Path::new(&project.source_path).exists();
    if source==ProjectSource::WorkingCopy&&elsewhere{return Err("Saved edits belong to a copy of this clip.".into());}
    let name=|p:&str|Path::new(p).file_name().map(|n|n.to_string_lossy().to_lowercase());
    if project.version!=1||name(&project.source_path)!=name(&info.path)||project.source_bytes!=info.source_bytes||project.source_modified_ms!=info.source_modified_ms{return Err("Saved edits belong to another source revision.".into());}
    validate_fps(project.export_fps.min(MAX_EXPORT_FPS))?;
    project.plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())
}
pub(crate) fn modified_ms(m:&std::fs::Metadata)->u64{m.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64}
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
fn run_engine_prepare(input:&Path,lowered:bool,stop:&dyn Fn()->bool)->Result<ClipInfo,String>{
    match run_engine_prepare_once(input,lowered,stop){
        Err(error) if lost_publish_race(&error)=>run_engine_prepare_once(input,lowered,stop),
        result=>result,
    }
}
fn lost_publish_race(error:&str)->bool{error.contains("publish cache:")}
fn run_engine_prepare_once(input:&Path,lowered:bool,stop:&dyn Fn()->bool)->Result<ClipInfo,String>{
    let editor=cache_dir()?;let mut command=engine_command("prepare-editor")?;command.arg(input).arg(&editor);
    let limit=prepare_limit(input.metadata().map_or(0,|m|m.len()));
    // A cut-off engine (another clip opened, the limit) leaves no partial stems behind.
    let output=storage::run_prepare(&mut command,&editor,lowered,limit,stop).map_err(|e|format!("prepare media: {e}"))?;
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
pub fn prepare_clip(path:&str)->Result<PreparedClip,String>{prepare_clip_steps(path,None,false,&|_|{},Arc::new(||false))}
/// [`prepare_clip`] saying what it is doing, for as long as `stop` says no. `known_copy`: an SSD copy
/// that already exists. `beside_playback`: the clip plays from where it lies, so it gets no copy, and
/// the engine runs off the disk's lane (very low I/O priority): for the length of a run the lane would
/// hold thumbnails, hover frames and the sidecar flush back.
pub fn prepare_clip_steps(path:&str,known_copy:Option<String>,beside_playback:bool,step:&dyn Fn(&'static str),stop:Arc<dyn Fn()->bool+Send+Sync>)->Result<PreparedClip,String>{
    let source=std::fs::canonicalize(path).map_err(|e|format!("resolve source: {e}"))?;
    let metadata=source.metadata().map_err(|e|format!("stat source: {e}"))?;
    if !metadata.is_file(){return Err("source is not a file".into());}
    let on_hdd=storage::drive_kind(&source).is_hdd();
    let mut warnings=Vec::new();
    // Too big for the video tier (or no room): play from the library drive, no warning needed.
    let copy=match known_copy{Some(copy)=>Some(copy),None if on_hdd&&!beside_playback=>{step("Loading clip from the hard drive…");storage::cache_video(&source.to_string_lossy()).ok().filter(|c|Path::new(c)!=source.as_path())},None=>None};
    step("Preparing sound…");
    let input=engine_input(&source,&metadata,copy.as_deref().map(Path::new));
    let frames_from=copy.as_ref().map(PathBuf::from).unwrap_or_else(||source.clone());
    let in_place=on_hdd&&copy.is_none();
    let original=source.clone();
    let work=move||->Result<(ClipInfo,VisualAssets,Option<String>),String>{
        let info=run_engine_prepare(&input,in_place&&storage::drive_kind(&input).is_hdd(),&*stop)?;
        // A copy gone mid-run (Settings cleared the cache) leaves frames to the original.
        let frames_from=if frames_from.is_file(){frames_from}else{original.clone()};
        let (visuals,warning)=visual_assets::visuals_best_effort(&original,&frames_from,info.duration);
        Ok((info,visuals,warning))
    };
    let (mut info,visuals,visual_warning)=if in_place&&!beside_playback{storage::run_io(&source,Priority::Interactive,work)??}else{work()?};
    // The copy is byte-identical and stamped with the source's mtime; pin identity to the original.
    info.path=source.to_string_lossy().into_owned();info.source_bytes=metadata.len();info.source_modified_ms=modified_ms(&metadata);
    normalize_audio_names(&mut info);
    storage::note_editor_media(&info.path,&editor_dirs(&info),true);
    storage::enforce_budget();
    let (project,project_warning,sidecar_ours,sidecar_at_open)=load_project(&info);
    warnings.extend(project_warning);warnings.extend(visual_warning);
    let warning=(!warnings.is_empty()).then(||warnings.join(" "));
    // Never hand the player a copy that is gone: it would show 0x80070002 and no picture.
    let copy=copy.filter(|c|Path::new(c).is_file());
    let limits=crate::services::library::video_limits(copy.as_deref().unwrap_or(&info.path),info.duration);
    let marks=read_marks(&info.path);
    Ok(PreparedClip{info,project,warning,visuals,sidecar_ours,sidecar_at_open,playback:copy,limits,marks})
}
/// Smart-cache fill step (runs on the clip's disk lane): prepares editor media — audio stems,
/// waveforms, poster, filmstrip — without opening the clip, from the SSD copy when there is
/// one. Returns the bytes now cached for it.
pub fn prefetch_editor_media(path:&str)->Result<u64,String>{
    let source=std::fs::canonicalize(path).map_err(|e|format!("resolve source: {e}"))?;
    let metadata=source.metadata().map_err(|e|format!("stat source: {e}"))?;
    let copy=storage::cached_video(&source.to_string_lossy()).map(PathBuf::from);
    let input=engine_input(&source,&metadata,copy.as_deref());
    let info=run_engine_prepare(&input,storage::drive_kind(&input).is_hdd(),&||false)?;
    let frames_from=copy.unwrap_or_else(||source.clone());
    let (visuals,_)=visual_assets::visuals_best_effort(&source,&frames_from,info.duration);
    let dirs=editor_dirs(&info);
    storage::note_editor_media(&source.to_string_lossy(),&dirs,false);
    let frames:u64=visuals.poster.iter().chain(visuals.filmstrip.iter().map(|f|&f.path)).filter_map(|p|std::fs::metadata(p).ok()).map(|m|m.len()).sum();
    Ok(dirs.iter().map(|d|folder_bytes(d)).sum::<u64>()+frames)
}
/// The poster and filmstrip already in the frame tier for this clip (no extraction).
pub(crate) fn cached_visuals(source:&Path)->VisualAssets{visual_assets::cached(source)}
/// Reads bookmark times written by the recorder beside the clip:
/// `<stem>.recall-marks.json` = `{"version":1,"marks":[seconds,...]}`. Missing → none.
pub fn read_marks(clip:&str)->Vec<f64>{
    let path=Path::new(clip);
    let Some(stem)=path.file_stem().and_then(|s|s.to_str()) else{return Vec::new()};
    let sidecar=path.with_file_name(format!("{stem}.recall-marks.json"));
    let Ok(text)=std::fs::read_to_string(sidecar) else{return Vec::new()};
    let Ok(value)=serde_json::from_str::<serde_json::Value>(&text) else{return Vec::new()};
    let mut marks:Vec<f64>=value.get("marks").and_then(|m|m.as_array()).map(|a|a.iter().filter_map(|v|v.as_f64()).filter(|t|t.is_finite()&&*t>=0.0).collect()).unwrap_or_default();
    marks.sort_by(f64::total_cmp);marks
}
/// Newer of the SSD working copy and the sidecar (falling back to the other when the newer one is invalid).
/// The project to open, a warning, and whether this session owns the sidecar: a sidecar that
/// exists but cannot be read or loaded may hold edits, so a later flush must leave it alone.
pub(crate) fn load_project(info:&ClipInfo)->(Option<Project>,Option<String>,bool,bool){
    let (Ok(working),Ok(sidecar))=(working::load_path(info),sidecar(&info.path))else{return(None,None,false,false)};
    // An imported clip with no edit file on C: yet may have one beside it from when its folder was
    // Recall's: read it (never written); the first flush writes the C: one.
    let fallback=edit_place::beside_fallback(&info.path,&sidecar);
    let lane_sidecar=fallback.clone().unwrap_or_else(||sidecar.clone());
    let (sidecar_ms,sidecar_bytes)=storage::run_io(Path::new(&info.path),Priority::Interactive,move||(storage::projects_modified_ms(&lane_sidecar),std::fs::read(&lane_sidecar).ok())).unwrap_or((None,None));
    let parse=|bytes:&[u8],from:ProjectSource|serde_json::from_slice::<Project>(bytes).map_err(|e|e.to_string()).and_then(|p|{validate_project(&p,info,from)?;Ok(p)});
    // `present`/`ours` are about the file a flush writes (the C: one when reading the fallback).
    let present=(fallback.is_none()&&sidecar_ms.is_some())||sidecar.exists();
    let ours=!present||sidecar_bytes.as_deref().is_some_and(|bytes|parse(bytes,ProjectSource::Sidecar).is_ok());
    let warning=(present&&sidecar_bytes.is_none()).then(||"Saved edits could not be read. They stay beside the clip.".to_owned());
    // A source that fails is reported only when no other one loads.
    let mut failed=None;
    for source in storage::project_order(storage::projects_modified_ms(&working),sidecar_ms.filter(|_|sidecar_bytes.is_some())){
        let bytes=match source{ProjectSource::WorkingCopy=>std::fs::read(&working).ok(),ProjectSource::Sidecar=>sidecar_bytes.clone()};
        let Some(bytes)=bytes else{continue};
        match parse(&bytes,source){
            Ok(mut p)=>{normalize_project_audio(&mut p.plan,info);return(Some(p),warning,ours,present)},
            Err(e)=>{failed.get_or_insert(format!("Saved edits were not loaded: {e}"));}
        }
    }
    (None,warning.or(failed),ours,present)
}
fn normalize_project_audio(plan:&mut EditPlan,info:&ClipInfo){
    let mut normalized=Vec::with_capacity(info.audio.len());
    for track in &info.audio { normalized.push(plan.audio.iter().find(|setting|setting.stream_index==track.stream_index).cloned().unwrap_or_else(||track.default_setting())); }
    plan.audio=normalized;
}
pub(crate) fn normalize_audio_names(info:&mut ClipInfo){
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
#[path="backend_project_tests.rs"] mod project_tests;
#[cfg(test)]
#[path="backend_tests.rs"] mod tests;
/// The highest export rate: the largest preset (`editor_state::FPS_PRESETS`). Here, not in
/// editor_state, because the examples include backend.rs without the editor (11-10 gate fix).
pub(crate) const MAX_EXPORT_FPS: u32 = 960;
fn validate_fps(fps:u32)->Result<(),String>{let max=MAX_EXPORT_FPS;if fps==0||fps>max{Err(format!("Export rate must be between 1 and {max} fps."))}else{Ok(())}}
fn project_bytes(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<Vec<u8>,String>{
    validate_fps(fps)?;plan.validate(info.duration,&info.audio.iter().map(|a|a.stream_index).collect::<Vec<_>>())?;
    let project=Project{version:1,source_path:info.path.clone(),source_bytes:info.source_bytes,source_modified_ms:info.source_modified_ms,plan:plan.clone(),export_fps:fps,export:export.clone()};
    serde_json::to_vec_pretty(&project).map_err(|e|e.to_string())
}
/// The edits now, on the SSD only: a clip reopened while the sidecar waits behind an export
/// must load these, not what was saved before.
pub fn save_working(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<(),String>{
    storage::write_atomic(&working_path(info)?,&project_bytes(info,plan,fps,export)?)
}
/// Autosave: writes the working project to the SSD (`cache\projects\<id>.json`, atomic). Touches nothing
/// on the library drive. Returns the working-copy path.
pub fn save_project(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings)->Result<String,String>{
    let bytes=project_bytes(info,plan,fps,export)?;let path=working_path(info)?;
    storage::write_atomic(&path,&bytes)?;
    // The first real edit also writes the sidecar beside the clip (once): the recorder's storage
    // limit keeps clips that have one, and an edit made before a crash stays protected.
    if !plan_is_default(plan,info)&&sidecar(&info.path).is_ok_and(|s|!s.exists()){let _=flush_project(info,plan,fps,export,true);}
    Ok(path.to_string_lossy().into_owned())
}
/// Untouched plan: one full-length range and every track at its default (0 dB, unmuted, in the
/// mix unless it is an app track).
fn plan_is_default(plan:&EditPlan,info:&ClipInfo)->bool{
    let full=matches!(plan.ranges.as_slice(),[only] if only.start.abs()<1e-6&&(only.end-info.duration).abs()<1e-3&&only.speed==1.0);
    full&&plan.mix_down.is_none_or(|on|on==crate::services::editor_prefs::EditorPrefs::load().mix_down)&&plan.audio.iter().all(|a|a.gain_db==0.0&&!a.muted&&info.audio.iter().find(|t|t.stream_index==a.stream_index).map_or(a.mix,|t|t.default_setting().mix==a.mix))
}
/// Leave / close / export: writes the `.recall-edit.json` sidecar beside the clip, through the clip's
/// disk lane, only when its bytes would change (and, with no sidecar yet, only for real edits).
/// A plan back at the untouched default removes the sidecar: no edits, no Edited. An exported
/// clip keeps its sidecar, and with it the storage limit's protection. `ours` false leaves an
/// existing sidecar untouched (see [`load_project`]).
pub fn flush_project(info:&ClipInfo,plan:&EditPlan,fps:u32,export:&ExportSettings,ours:bool)->Result<Flushed,String>{
    validate_info(info)?;let bytes=project_bytes(info,plan,fps,export)?;
    let working=working_path(info)?;let default=plan_is_default(plan,info);
    let path=sidecar(&info.path)?;let lane_bytes=bytes.clone();let clip=PathBuf::from(&info.path);
    // A few KB: it does not wait for an export that holds the disk lane for its whole run.
    let flushed=storage::run_io_brief(Path::new(&info.path),Priority::Interactive,move||sidecar_flush::sidecar_task(&path,&clip,&lane_bytes,default,ours))??;
    if !matches!(flushed,Flushed::Nothing){storage::write_atomic(&working,&bytes)?;}
    Ok(flushed)
}
/// What a flush left beside the clip; the path is the sidecar's.
#[derive(Debug,Clone,PartialEq)]
pub enum Flushed{Nothing,Current(String),Removed(String)}
/// A hung engine must not hold the io queue and the output drive for good. NVENC runs far past real
/// time and the CPU encoder near it, at 60 fps; this is well beyond the slowest honest export.
fn export_limit(plan:&EditPlan,fps:u32)->Duration{
    let seconds:f64=plan.ranges.iter().map(|r|(r.end-r.start)/if r.speed>0.0{r.speed}else{1.0}).sum();
    Duration::from_secs_f64(600.0+seconds.max(0.0)*20.0*(f64::from(fps)/60.0).max(1.0))
}
#[path="backend_export.rs"] mod run_export;
#[path="backend_sidecar.rs"] mod sidecar_flush;
#[path="backend_working.rs"] mod working;
use working::working_path;
pub use run_export::export_clip;
#[allow(unused_imports)] // the editor reads it; the examples build backend without one
pub use run_export::export_progress;
pub(crate) use run_export::{engine_export,export_cancelled,export_queued,export_target,name_after};
/// Drive types and cache usage for Settings (walks the cache: call from a worker thread).
pub fn storage_report(notice:Option<String>)->StorageReport{
    let cache=storage::usage_detail();
    StorageReport{library_drives:storage::drive_info(&library_dir()).into_iter().collect(),cache_drive:storage::drive_info(Path::new(&cache.root)),cache,notice}
}
pub fn show_in_folder(path:&str){let _=Command::new("explorer.exe").arg(format!("/select,{path}")).creation_flags(0x08000000).spawn();}

