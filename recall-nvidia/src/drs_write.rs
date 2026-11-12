//! Per-program driver settings written to the profile database (NvAPI DRS).
//!
//! Only the in-app fixer calls this, and only after the user clicked Fix for
//! one specific fix. The NVIDIA profile database lets every account save it
//! (the NVIDIA app writes it the same way), so no elevation is needed. Every write returns a [`Prior`]
//! that [`DriverWriter::revert`] uses to put the database back the way it was.
//!
//! Function IDs are from NVIDIA's public `nvapi_interface.h`.

use std::ffi::c_void;

use libloading::Library;

use crate::drs::{profile_version, setting_version, wide, Application, Handle, Profile, Setting, Unicode};

const NVAPI_OK: i32 = 0;
const EXE_NOT_FOUND: i32 = -166;
const SETTING_NOT_FOUND: i32 = -160;
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
const ID_SET_SETTING: u32 = 0x577D_D202;
const ID_DELETE_SETTING: u32 = 0xE4A2_6362;
const ID_RESTORE_DEFAULT: u32 = 0x53F0_381E;

/// What one program's setting looks like in the database right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSetting {
    /// The profile the program belongs to, if any.
    pub profile: Option<String>,
    /// The setting's current value in that profile, if the profile sets it.
    pub value: Option<u32>,
    /// The current value is NVIDIA's own (predefined), not a user change.
    pub predefined: bool,
}

/// What a write changed, so it can be undone exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prior {
    /// The program had no profile; the fixer made `profile` (holding only it).
    CreatedProfile { profile: String },
    /// The program's existing profile did not set the setting.
    SettingAdded { profile: String },
    /// The setting had NVIDIA's predefined `old` value.
    PredefinedValue { profile: String, old: u32 },
    /// The setting had a user-chosen `old` value.
    UserValue { profile: String, old: u32 },
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
type SetSetting = unsafe extern "C" fn(Handle, Handle, *mut Setting) -> i32;
type WithProfileSetting = unsafe extern "C" fn(Handle, Handle, u32) -> i32;

/// NvAPI with the DRS write calls. Each call opens its own session, loads the
/// database and destroys the session before it returns.
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
    set_setting: SetSetting,
    delete_setting: WithProfileSetting,
    restore_default: WithProfileSetting,
    _lib: Library,
}

impl DriverWriter {
    /// Load and initialize NvAPI. Fails without an NVIDIA driver or GPU.
    pub fn load() -> Result<Self, String> {
        // SAFETY: nvapi64.dll is the NVIDIA driver's own library; loading it runs its DllMain only.
        let lib = unsafe { Library::new("nvapi64.dll") }.map_err(|e| format!("nvapi64.dll not loaded: {e}"))?;
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
        let nv = DriverWriter {
            unload: f!(ID_UNLOAD, Plain),
            create: f!(ID_CREATE_SESSION, CreateSession),
            destroy: f!(ID_DESTROY_SESSION, WithSession),
            load: f!(ID_LOAD_SETTINGS, WithSession),
            save: f!(ID_SAVE_SETTINGS, WithSession),
            find_app: f!(ID_FIND_APPLICATION, FindApplication),
            find_profile: f!(ID_FIND_PROFILE, FindProfile),
            profile_info: f!(ID_GET_PROFILE_INFO, GetProfileInfo),
            get_setting: f!(ID_GET_SETTING, GetSetting),
            create_profile: f!(ID_CREATE_PROFILE, CreateProfile),
            delete_profile: f!(ID_DELETE_PROFILE, WithProfile),
            create_app: f!(ID_CREATE_APPLICATION, CreateApplication),
            set_setting: f!(ID_SET_SETTING, SetSetting),
            delete_setting: f!(ID_DELETE_SETTING, WithProfileSetting),
            restore_default: f!(ID_RESTORE_DEFAULT, WithProfileSetting),
            _lib: lib,
        };
        let init = f!(ID_INITIALIZE, Plain);
        // SAFETY: NvAPI_Initialize takes no arguments.
        match unsafe { init() } {
            NVAPI_OK => Ok(nv),
            rc => Err(format!("NvAPI_Initialize failed ({rc}): no NVIDIA GPU?")),
        }
    }

    /// Read-only: the program's profile and the setting's value. Saves nothing.
    pub fn read(&self, exe: &str, id: u32) -> Result<AppSetting, String> {
        let s = self.session()?;
        let Some(profile) = s.find_app(exe)? else { return Ok(AppSetting { profile: None, value: None, predefined: false }) };
        let name = s.profile_name(profile)?;
        let (value, predefined) = match s.setting(profile, id)? {
            Some((v, p)) => (Some(v), p),
            None => (None, false),
        };
        Ok(AppSetting { profile: Some(name), value, predefined })
    }

    /// Set `id` = `value` for the program `exe`. A program with no profile gets
    /// a new profile named `new_profile` holding only it. Saves the database.
    pub fn apply(&self, exe: &str, id: u32, value: u32, new_profile: &str) -> Result<Prior, String> {
        let s = self.session()?;
        let (profile, prior) = match s.find_app(exe)? {
            Some(profile) => {
                let name = s.profile_name(profile)?;
                let prior = match s.setting(profile, id)? {
                    None => Prior::SettingAdded { profile: name },
                    Some((old, true)) => Prior::PredefinedValue { profile: name, old },
                    Some((old, false)) => Prior::UserValue { profile: name, old },
                };
                (profile, prior)
            }
            None => {
                if s.find_profile(new_profile)?.is_some() {
                    return Err(format!("a driver profile named \"{new_profile}\" already exists but does not hold {exe}"));
                }
                let profile = s.create_profile(new_profile)?;
                s.add_app(profile, exe)?;
                (profile, Prior::CreatedProfile { profile: new_profile.to_owned() })
            }
        };
        s.set(profile, id, value)?;
        s.save()?;
        Ok(prior)
    }

    /// Undo an [`apply`](Self::apply) using what it returned. Saves the database.
    pub fn revert(&self, id: u32, prior: &Prior) -> Result<(), String> {
        let s = self.session()?;
        let name = match prior {
            Prior::CreatedProfile { profile } | Prior::SettingAdded { profile } | Prior::PredefinedValue { profile, .. } | Prior::UserValue { profile, .. } => profile,
        };
        let Some(profile) = s.find_profile(name)? else {
            // Already gone (the user removed it by hand): nothing left to undo.
            return Ok(());
        };
        match prior {
            Prior::CreatedProfile { .. } => s.call("NvAPI_DRS_DeleteProfile", unsafe { (self.delete_profile)(s.handle, profile) })?,
            Prior::SettingAdded { .. } => match unsafe { (self.delete_setting)(s.handle, profile, id) } {
                NVAPI_OK | SETTING_NOT_FOUND => {}
                rc => return Err(format!("NvAPI_DRS_DeleteProfileSetting failed ({rc})")),
            },
            Prior::PredefinedValue { .. } => s.call("NvAPI_DRS_RestoreProfileDefaultSetting", unsafe { (self.restore_default)(s.handle, profile, id) })?,
            Prior::UserValue { old, .. } => s.set(profile, id, *old)?,
        }
        s.save()
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
        // SAFETY: pairs the Initialize in `load`.
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
            // NVAPI_PROFILE_NOT_FOUND (-163).
            -163 => Ok(None),
            rc => Err(format!("NvAPI_DRS_FindProfileByName failed ({rc})")),
        }
    }

    fn profile_name(&self, profile: Handle) -> Result<String, String> {
        // SAFETY: plain data.
        let mut info: Box<Profile> = Box::new(unsafe { std::mem::zeroed() });
        info.version = profile_version();
        // SAFETY: live session and profile handle.
        self.call("NvAPI_DRS_GetProfileInfo", unsafe { (self.nv.profile_info)(self.handle, profile, &mut *info) })?;
        Ok(wide(&info.profile_name))
    }

    /// The setting's DWORD value and whether it is NVIDIA's predefined one.
    fn setting(&self, profile: Handle, id: u32) -> Result<Option<(u32, bool)>, String> {
        // SAFETY: plain data.
        let mut s: Box<Setting> = Box::new(unsafe { std::mem::zeroed() });
        s.version = setting_version();
        // SAFETY: live session and profile handle.
        match unsafe { (self.nv.get_setting)(self.handle, profile, id, &mut *s) } {
            NVAPI_OK if s.setting_type != DWORD_TYPE => Err(format!("setting 0x{id:08X} is not a number setting (type {})", s.setting_type)),
            NVAPI_OK => Ok(Some((u32::from_le_bytes([s.current[0], s.current[1], s.current[2], s.current[3]]), s.is_current_predefined != 0))),
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
