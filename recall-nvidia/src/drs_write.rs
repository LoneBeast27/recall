//! Per-program driver settings written to the profile database (NvAPI DRS).
//!
//! Only the in-app fixer calls this, and only after the user clicked Fix for
//! one specific fix. The NVIDIA profile database lets every account save it
//! (the NVIDIA app writes it the same way), so no elevation is needed.
//!
//! Rules, so a fix never reaches past the one program:
//! - An existing profile is never edited. The fixer only makes its own
//!   profile, holding one full-path entry, and only when the driver matches
//!   the program to no profile at all.
//! - Before saving, a made-up sibling path is looked up in the same session.
//!   If it also lands in the new profile, the driver keys the entry by file
//!   name (every javaw.exe), so nothing is saved.
//! - Undo removes only what is still the fixer's: the setting only while it
//!   holds the fixer's value, the whole profile only while it holds nothing
//!   else.
//!
//! Function IDs are from NVIDIA's public `nvapi_interface.h`.

use std::ffi::c_void;

use libloading::Library;

use crate::drs::{load_system_nvapi, profile_version, setting_version, wide, Application, Handle, Profile, Setting, Unicode};

const NVAPI_OK: i32 = 0;
const EXE_NOT_FOUND: i32 = -166;
const SETTING_NOT_FOUND: i32 = -160;
const PROFILE_NOT_FOUND: i32 = -163;
/// NVDRS_DWORD_TYPE.
const DWORD_TYPE: u32 = 0;

const ID_INITIALIZE: u32 = 0x0150_E828;
const ID_UNLOAD: u32 = 0xD22B_DD7E;
const ID_CREATE_SESSION: u32 = 0x0694_D52E;
const ID_DESTROY_SESSION: u32 = 0xDAD9_CFF8;
const ID_LOAD_SETTINGS: u32 = 0x375D_BD6B;
const ID_SAVE_SETTINGS: u32 = 0xFCBC_7E14;
const ID_FIND_APPLICATION: u32 = 0xEEE5_66B2;
const ID_FIND_PROFILE: u32 = 0x7E4A_9A0B;
const ID_GET_PROFILE_INFO: u32 = 0x61CD_6FD6;
const ID_GET_SETTING: u32 = 0x73BF_8338;
const ID_CREATE_PROFILE: u32 = 0xCC17_6068;
const ID_DELETE_PROFILE: u32 = 0x1709_3206;
const ID_CREATE_APPLICATION: u32 = 0x4347_A9DE;
const ID_DELETE_APPLICATION: u32 = 0x2C69_4BC6;
const ID_SET_SETTING: u32 = 0x577D_D202;
const ID_DELETE_SETTING: u32 = 0xE4A2_6362;

/// Where the driver puts a program today. Read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// No profile matches the program.
    None,
    /// A profile matches (by path or by file name); `value` = the setting in it.
    Profile { name: String, value: Option<u32> },
}

/// What [`DriverWriter::revert_own`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reverted {
    /// The profile was the fixer's alone and is gone.
    ProfileRemoved,
    /// The profile was already gone.
    AlreadyGone,
    /// The user added to the profile since; only the fixer's parts went.
    /// `setting_kept` = the user changed the setting, so it was left as is.
    Partly { setting_kept: bool, entry_kept: bool },
}

type QueryInterface = unsafe extern "C" fn(u32) -> *mut c_void;
type Plain = unsafe extern "C" fn() -> i32;
type CreateSession = unsafe extern "C" fn(*mut Handle) -> i32;
type WithSession = unsafe extern "C" fn(Handle) -> i32;
type FindApplication = unsafe extern "C" fn(Handle, *const u16, *mut Handle, *mut Application) -> i32;
type FindProfile = unsafe extern "C" fn(Handle, *const u16, *mut Handle) -> i32;
type GetProfileInfo = unsafe extern "C" fn(Handle, Handle, *mut Profile) -> i32;
type GetSetting = unsafe extern "C" fn(Handle, Handle, u32, *mut Setting) -> i32;
type CreateProfile = unsafe extern "C" fn(Handle, *mut Profile, *mut Handle) -> i32;
type WithProfile = unsafe extern "C" fn(Handle, Handle) -> i32;
type CreateApplication = unsafe extern "C" fn(Handle, Handle, *mut Application) -> i32;
type DeleteApplication = unsafe extern "C" fn(Handle, Handle, *const u16) -> i32;
type SetSetting = unsafe extern "C" fn(Handle, Handle, *mut Setting) -> i32;
type WithProfileSetting = unsafe extern "C" fn(Handle, Handle, u32) -> i32;

/// NvAPI with the DRS write calls. Each call opens its own session, loads the
/// database and destroys the session before it returns; a session that is
/// not saved changes nothing.
pub struct DriverWriter {
    unload: Plain,
    create: CreateSession,
    destroy: WithSession,
    load: WithSession,
    save: WithSession,
    find_app: FindApplication,
    find_profile: FindProfile,
    profile_info: GetProfileInfo,
    get_setting: GetSetting,
    create_profile: CreateProfile,
    delete_profile: WithProfile,
    create_app: CreateApplication,
    delete_app: DeleteApplication,
    set_setting: SetSetting,
    delete_setting: WithProfileSetting,
    _lib: Library,
}

impl DriverWriter {
    /// Load NvAPI from System32 and initialize it. Fails without an NVIDIA driver or GPU.
    pub fn load() -> Result<Self, String> {
        let lib = load_system_nvapi()?;
        // SAFETY: the export's signature is fixed by nvapi.h.
        let qi: QueryInterface = unsafe { *lib.get::<QueryInterface>(b"nvapi_QueryInterface\0").map_err(|e| format!("nvapi_QueryInterface: {e}"))? };
        let get = |id: u32| -> Result<*mut c_void, String> {
            // SAFETY: QueryInterface takes any id and returns null when unknown.
            let p = unsafe { qi(id) };
            if p.is_null() { Err(format!("nvapi_QueryInterface(0x{id:08X}) returned null")) } else { Ok(p) }
        };
        macro_rules! f {
            ($id:expr, $t:ty) => {
                // SAFETY: each id maps to the nvapi.h signature named by the target type.
                unsafe { std::mem::transmute::<*mut c_void, $t>(get($id)?) }
            };
        }
        // Resolve everything first, then initialize: the struct (whose drop
        // calls Unload) exists only after a successful Initialize.
        let (unload, create, destroy, load, save) = (f!(ID_UNLOAD, Plain), f!(ID_CREATE_SESSION, CreateSession), f!(ID_DESTROY_SESSION, WithSession), f!(ID_LOAD_SETTINGS, WithSession), f!(ID_SAVE_SETTINGS, WithSession));
        let (find_app, find_profile, profile_info, get_setting) = (f!(ID_FIND_APPLICATION, FindApplication), f!(ID_FIND_PROFILE, FindProfile), f!(ID_GET_PROFILE_INFO, GetProfileInfo), f!(ID_GET_SETTING, GetSetting));
        let (create_profile, delete_profile, create_app, delete_app) = (f!(ID_CREATE_PROFILE, CreateProfile), f!(ID_DELETE_PROFILE, WithProfile), f!(ID_CREATE_APPLICATION, CreateApplication), f!(ID_DELETE_APPLICATION, DeleteApplication));
        let (set_setting, delete_setting) = (f!(ID_SET_SETTING, SetSetting), f!(ID_DELETE_SETTING, WithProfileSetting));
        let init = f!(ID_INITIALIZE, Plain);
        // SAFETY: NvAPI_Initialize takes no arguments.
        let rc = unsafe { init() };
        if rc != NVAPI_OK {
            return Err(format!("NvAPI_Initialize failed ({rc}): no NVIDIA GPU?"));
        }
        Ok(DriverWriter { unload, create, destroy, load, save, find_app, find_profile, profile_info, get_setting, create_profile, delete_profile, create_app, delete_app, set_setting, delete_setting, _lib: lib })
    }

    /// Read-only: which profile the driver matches `exe` to, and the setting there.
    pub fn placement(&self, exe: &str, id: u32) -> Result<Placement, String> {
        let s = self.session()?;
        let Some(profile) = s.find_app(exe)? else { return Ok(Placement::None) };
        Ok(Placement::Profile { name: s.info(profile)?.0, value: s.setting(profile, id)? })
    }

    /// Make `new_profile` holding only `exe` (full path) with `id` = `value`,
    /// then save. Refused when the driver already matches `exe` to any
    /// profile, or when `probe` (a made-up path beside it that no program
    /// has) also lands in the new profile: then the entry would not be
    /// path-scoped, and nothing is saved.
    pub fn create_own(&self, exe: &str, probe: &str, id: u32, value: u32, new_profile: &str) -> Result<(), String> {
        let s = self.session()?;
        if let Some(p) = s.find_app(exe)? {
            return Err(format!("the driver already matches this program to the profile \"{}\"; Recall does not edit existing profiles", s.info(p)?.0));
        }
        if s.find_profile(new_profile)?.is_some() {
            return Err(format!("a driver profile named \"{new_profile}\" already exists"));
        }
        let profile = s.create_profile(new_profile)?;
        s.add_app(profile, exe)?;
        s.set(profile, id, value)?;
        let probe_hit = match s.find_app(probe)? {
            Some(p) => s.info(p)?.0 == new_profile,
            None => false,
        };
        if probe_hit {
            return Err("the driver would apply this to every program with this file name, not just this one; nothing was saved".into());
        }
        s.save()
    }

    /// Undo [`create_own`](Self::create_own): only what is still the fixer's.
    pub fn revert_own(&self, exe: &str, id: u32, value: u32, own_profile: &str) -> Result<Reverted, String> {
        let s = self.session()?;
        let Some(profile) = s.find_profile(own_profile)? else { return Ok(Reverted::AlreadyGone) };
        let (_, apps, settings) = s.info(profile)?;
        let ours = s.setting(profile, id)? == Some(value);
        if apps <= 1 && settings <= 1 && (ours || settings == 0) {
            // SAFETY: live session and profile handle.
            s.call("NvAPI_DRS_DeleteProfile", unsafe { (self.delete_profile)(s.handle, profile) })?;
            s.save()?;
            return Ok(Reverted::ProfileRemoved);
        }
        if ours {
            // SAFETY: live session and profile handle.
            match unsafe { (self.delete_setting)(s.handle, profile, id) } {
                NVAPI_OK | SETTING_NOT_FOUND => {}
                rc => return Err(format!("NvAPI_DRS_DeleteProfileSetting failed ({rc})")),
            }
        }
        // The user's own settings in the profile still need a program to apply to.
        let entry_kept = apps <= 1;
        if !entry_kept {
            let name = unicode(exe);
            // SAFETY: live session and profile handle; NUL-terminated name.
            match unsafe { (self.delete_app)(s.handle, profile, name.as_ptr()) } {
                NVAPI_OK | EXE_NOT_FOUND => {}
                rc => return Err(format!("NvAPI_DRS_DeleteApplication failed ({rc})")),
            }
        }
        s.save()?;
        Ok(Reverted::Partly { setting_kept: !ours, entry_kept })
    }

    fn session(&self) -> Result<WriteSession<'_>, String> {
        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY: out-pointer to a local.
        let rc = unsafe { (self.create)(&mut handle) };
        if rc != NVAPI_OK {
            return Err(format!("NvAPI_DRS_CreateSession failed ({rc})"));
        }
        let s = WriteSession { nv: self, handle };
        // SAFETY: a live session.
        s.call("NvAPI_DRS_LoadSettings", unsafe { (self.load)(handle) })?;
        Ok(s)
    }
}

impl Drop for DriverWriter {
    fn drop(&mut self) {
        // SAFETY: pairs the successful Initialize in `load` (NvAPI counts them).
        unsafe { (self.unload)() };
    }
}

struct WriteSession<'a> {
    nv: &'a DriverWriter,
    handle: Handle,
}

impl WriteSession<'_> {
    fn call(&self, what: &str, rc: i32) -> Result<(), String> {
        if rc == NVAPI_OK { Ok(()) } else { Err(format!("{what} failed ({rc})")) }
    }

    fn find_app(&self, exe: &str) -> Result<Option<Handle>, String> {
        let name = unicode(exe);
        // SAFETY: Application is plain data; all-zero is valid.
        let mut entry: Box<Application> = Box::new(unsafe { std::mem::zeroed() });
        entry.version = crate::drs::app_version();
        let mut profile: Handle = std::ptr::null_mut();
        // SAFETY: live session; NUL-terminated name; out-pointers to owned memory.
        match unsafe { (self.nv.find_app)(self.handle, name.as_ptr(), &mut profile, &mut *entry) } {
            NVAPI_OK => Ok(Some(profile)),
            EXE_NOT_FOUND => Ok(None),
            rc => Err(format!("NvAPI_DRS_FindApplicationByName failed ({rc})")),
        }
    }

    fn find_profile(&self, name: &str) -> Result<Option<Handle>, String> {
        let wide_name = unicode(name);
        let mut profile: Handle = std::ptr::null_mut();
        // SAFETY: live session; NUL-terminated name.
        match unsafe { (self.nv.find_profile)(self.handle, wide_name.as_ptr(), &mut profile) } {
            NVAPI_OK => Ok(Some(profile)),
            PROFILE_NOT_FOUND => Ok(None),
            rc => Err(format!("NvAPI_DRS_FindProfileByName failed ({rc})")),
        }
    }

    /// (name, number of programs, number of settings).
    fn info(&self, profile: Handle) -> Result<(String, u32, u32), String> {
        // SAFETY: plain data.
        let mut info: Box<Profile> = Box::new(unsafe { std::mem::zeroed() });
        info.version = profile_version();
        // SAFETY: live session and profile handle.
        self.call("NvAPI_DRS_GetProfileInfo", unsafe { (self.nv.profile_info)(self.handle, profile, &mut *info) })?;
        Ok((wide(&info.profile_name), info.num_of_apps, info.num_of_settings))
    }

    /// The setting's DWORD value, if the profile sets it.
    fn setting(&self, profile: Handle, id: u32) -> Result<Option<u32>, String> {
        // SAFETY: plain data.
        let mut s: Box<Setting> = Box::new(unsafe { std::mem::zeroed() });
        s.version = setting_version();
        // SAFETY: live session and profile handle.
        match unsafe { (self.nv.get_setting)(self.handle, profile, id, &mut *s) } {
            NVAPI_OK if s.setting_type != DWORD_TYPE => Err(format!("setting 0x{id:08X} is not a number setting (type {})", s.setting_type)),
            NVAPI_OK => Ok(Some(u32::from_le_bytes([s.current[0], s.current[1], s.current[2], s.current[3]]))),
            SETTING_NOT_FOUND => Ok(None),
            rc => Err(format!("NvAPI_DRS_GetSetting failed ({rc})")),
        }
    }

    fn create_profile(&self, name: &str) -> Result<Handle, String> {
        // SAFETY: plain data.
        let mut info: Box<Profile> = Box::new(unsafe { std::mem::zeroed() });
        info.version = profile_version();
        info.profile_name = *unicode(name);
        let mut profile: Handle = std::ptr::null_mut();
        // SAFETY: live session; out-pointer to a local.
        self.call("NvAPI_DRS_CreateProfile", unsafe { (self.nv.create_profile)(self.handle, &mut *info, &mut profile) })?;
        Ok(profile)
    }

    fn add_app(&self, profile: Handle, exe: &str) -> Result<(), String> {
        // SAFETY: plain data.
        let mut app: Box<Application> = Box::new(unsafe { std::mem::zeroed() });
        app.version = crate::drs::app_version();
        app.app_name = *unicode(exe);
        // SAFETY: live session and profile handle.
        self.call("NvAPI_DRS_CreateApplication", unsafe { (self.nv.create_app)(self.handle, profile, &mut *app) })
    }

    fn set(&self, profile: Handle, id: u32, value: u32) -> Result<(), String> {
        // SAFETY: plain data.
        let mut s: Box<Setting> = Box::new(unsafe { std::mem::zeroed() });
        s.version = setting_version();
        s.setting_id = id;
        s.setting_type = DWORD_TYPE;
        s.current[..4].copy_from_slice(&value.to_le_bytes());
        // SAFETY: live session and profile handle.
        self.call("NvAPI_DRS_SetSetting", unsafe { (self.nv.set_setting)(self.handle, profile, &mut *s) })
    }

    fn save(&self) -> Result<(), String> {
        // SAFETY: a live session.
        match unsafe { (self.nv.save)(self.handle) } {
            NVAPI_OK => Ok(()),
            // NVAPI_INVALID_USER_PRIVILEGE.
            -137 => Err("NvAPI_DRS_SaveSettings refused: this account may not save driver settings".into()),
            rc => Err(format!("NvAPI_DRS_SaveSettings failed ({rc})")),
        }
    }
}

impl Drop for WriteSession<'_> {
    fn drop(&mut self) {
        // SAFETY: the session from CreateSession, destroyed once. Unsaved changes are dropped with it.
        unsafe { (self.nv.destroy)(self.handle) };
    }
}

fn unicode(s: &str) -> Box<Unicode> {
    let mut out: Box<Unicode> = Box::new([0; 2048]);
    for (dst, src) in out.iter_mut().zip(s.encode_utf16().take(2047)) {
        *dst = src;
    }
    out
}
