//! Own and configure only the status-overlay process launched by this Hub.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::os::windows::process::CommandExt;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config { pub enabled: bool, pub corner: String }
impl Default for Config { fn default() -> Self { Self { enabled:false,corner:"top-right".into() } } }
#[derive(Serialize)]
pub struct Status { enabled: bool, corner: String, running: bool, error: Option<String> }
struct Runtime { config: Config, child: Option<Child>, error: Option<String> }
pub struct OverlayState(Mutex<Runtime>);
impl Default for OverlayState {
    fn default()->Self { Self(Mutex::new(Runtime {config:Config::default(),child:None,error:None})) }
}
impl Drop for OverlayState {
    fn drop(&mut self) { if let Ok(runtime)=self.0.get_mut() { stop_child(&mut runtime.child); } }
}
fn stop_child(child: &mut Option<Child>) {
    if let Some(mut own_child)=child.take() {
        if matches!(own_child.try_wait(),Ok(None)) { let _=own_child.kill(); }
        let _=own_child.wait();
    }
}
fn valid_corner(corner:&str)->bool { matches!(corner,"dynamic"|"top-left"|"top-right"|"bottom-left"|"bottom-right") }
fn locate_overlay()->Result<PathBuf,String> {
    if let Some(value)=std::env::var_os("RECALL_OVERLAY") {
        let path=PathBuf::from(value);
        return if path.is_file(){Ok(path)}else{Err("RECALL_OVERLAY does not name a file".into())};
    }
    let exe=std::env::current_exe().map_err(|e|e.to_string())?;
    for parent in exe.ancestors() {
        for candidate in [parent.join("recall-overlay.exe"),parent.join("target-dev/debug/recall-overlay.exe"),parent.join("target/release/recall-overlay.exe")] {
            if candidate.is_file() {return Ok(candidate);}
        }
    }
    Err("Overlay executable is missing. Build recall-overlay or set RECALL_OVERLAY.".into())
}
fn settings_path(app:&AppHandle)->Result<PathBuf,String> {
    let dir=app.path().app_config_dir().map_err(|e|e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e|format!("create settings directory: {e}"))?;
    Ok(dir.join("overlay.json"))
}
fn snapshot(runtime:&mut Runtime)->Status {
    let running=runtime.child.as_mut().is_some_and(|child|matches!(child.try_wait(),Ok(None)));
    if runtime.config.enabled && !running && runtime.error.is_none() {runtime.error=Some("Overlay stopped. Switch it off and on to retry.".into());}
    Status {enabled:runtime.config.enabled,corner:runtime.config.corner.clone(),running,error:runtime.error.clone()}
}
fn apply(app:&AppHandle,state:&OverlayState,config:Config,persist:bool)->Result<Status,String> {
    if !valid_corner(&config.corner) {return Err("unsupported overlay corner".into());}
    let mut runtime=state.0.lock().map_err(|_|"overlay state is unavailable")?;
    // Validate the executable before changing an existing running instance.
    let executable=if config.enabled {Some(locate_overlay()?)} else {None};
    if persist {
        let path=settings_path(app)?;
        let temp=path.with_extension(format!("{}.tmp",std::process::id()));
        std::fs::write(&temp,serde_json::to_vec_pretty(&config).map_err(|e|e.to_string())?).map_err(|e|format!("save overlay settings: {e}"))?;
        std::fs::rename(&temp,&path).map_err(|e|format!("publish overlay settings: {e}"))?;
    }
    stop_child(&mut runtime.child);runtime.config=config.clone();runtime.error=None;
    if let Some(exe)=executable {
        match Command::new(exe).arg("--corner").arg(&config.corner).arg("--parent").arg(std::process::id().to_string())
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(0x08000000).spawn() {
                Ok(child)=>runtime.child=Some(child),
                Err(error)=>runtime.error=Some(format!("Overlay could not start: {error}")),
            }
    }
    Ok(snapshot(&mut runtime))
}
pub fn initialize(app:&AppHandle) {
    let loaded=settings_path(app).ok().and_then(|p|std::fs::read(p).ok()).and_then(|v|serde_json::from_slice::<Config>(&v).ok()).unwrap_or_default();
    let state=app.state::<OverlayState>();
    if let Err(error)=apply(app,&state,loaded,false) {
        if let Ok(mut runtime)=state.0.lock(){runtime.error=Some(error);}
    }
}
#[tauri::command]
pub fn overlay_status(state:tauri::State<OverlayState>)->Result<Status,String> {
    let mut runtime=state.0.lock().map_err(|_|"overlay state is unavailable")?;
    Ok(snapshot(&mut runtime))
}
#[tauri::command]
pub fn overlay_configure(app:AppHandle,state:tauri::State<OverlayState>,enabled:bool,corner:String)->Result<Status,String> {
    apply(&app,&state,Config{enabled,corner},true)
}

