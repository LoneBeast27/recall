//! NvAPI DRS through `nvapi_QueryInterface` (the DLL's only export). Struct
//! layouts and function IDs are from NVIDIA's public `nvapi.h` /
//! `nvapi_interface.h` (R5xx).

use std::ffi::c_void;

use libloading::Library;

use crate::{Lookup, GAME_MARKER_ID};

const NVAPI_OK: i32 = 0;
/// NVAPI_EXECUTABLE_NOT_FOUND: no profile has this application.
const EXE_NOT_FOUND: i32 = -166;
/// NVAPI_SETTING_NOT_FOUND: the profile does not set this setting.
const SETTING_NOT_FOUND: i32 = -160;

const ID_INITIALIZE: u32 = 0x0150_E828;
const ID_UNLOAD: u32 = 0xD22B_DD7E;
const ID_CREATE_SESSION: u32 = 0x0694_D52E;
const ID_DESTROY_SESSION: u32 = 0xDAD9_CFF8;
const ID_LOAD_SETTINGS: u32 = 0x375D_BD6B;
const ID_FIND_APPLICATION: u32 = 0xEEE5_66B2;
const ID_GET_PROFILE_INFO: u32 = 0x61CD_6FD6;
const ID_GET_SETTING: u32 = 0x73BF_8338;

/// NvAPI_UnicodeString: 2048 UTF-16 units.
pub(crate) type Unicode = [u16; 2048];
pub(crate) type Handle = *mut c_void;

#[repr(C)]
#[allow(dead_code)] // C layout: every field must exist, few are read
pub(crate) struct Application {
    pub(crate) version: u32,
    pub(crate) is_predefined: u32,
    pub(crate) app_name: Unicode,
    pub(crate) user_friendly_name: Unicode,
    pub(crate) launcher: Unicode,
    pub(crate) file_in_folder: Unicode,
    /// isMetro:1, isCommandLine:1, reserved:30.
    pub(crate) flags: u32,
    pub(crate) command_line: Unicode,
}

#[repr(C)]
#[allow(dead_code)] // C layout: every field must exist, few are read
pub(crate) struct Profile {
    pub(crate) version: u32,
    pub(crate) profile_name: Unicode,
    pub(crate) gpu_support: u32,
    pub(crate) is_predefined: u32,
    pub(crate) num_of_apps: u32,
    pub(crate) num_of_settings: u32,
}

#[repr(C)]
#[allow(dead_code)] // C layout: every field must exist, few are read
pub(crate) struct Setting {
    pub(crate) version: u32,
    pub(crate) setting_name: Unicode,
    pub(crate) setting_id: u32,
    pub(crate) setting_type: u32,
    pub(crate) setting_location: u32,
    pub(crate) is_current_predefined: u32,
    pub(crate) is_predefined_valid: u32,
    /// Union of a u32, a binary blob (u32 length + 4096 bytes) and a string.
    pub(crate) predefined: [u8; 4100],
    pub(crate) current: [u8; 4100],
}

const fn version<T>(v: u32) -> u32 {
    std::mem::size_of::<T>() as u32 | (v << 16)
}

pub(crate) const fn app_version() -> u32 {
    version::<Application>(4)
}
pub(crate) const fn profile_version() -> u32 {
    version::<Profile>(1)
}
pub(crate) const fn setting_version() -> u32 {
    version::<Setting>(1)
}

type QueryInterface = unsafe extern "C" fn(u32) -> *mut c_void;
type Plain = unsafe extern "C" fn() -> i32;
type CreateSession = unsafe extern "C" fn(*mut Handle) -> i32;
type WithSession = unsafe extern "C" fn(Handle) -> i32;
type FindApplication = unsafe extern "C" fn(Handle, *const u16, *mut Handle, *mut Application) -> i32;
type GetProfileInfo = unsafe extern "C" fn(Handle, Handle, *mut Profile) -> i32;
type GetSetting = unsafe extern "C" fn(Handle, Handle, u32, *mut Setting) -> i32;

/// A loaded, initialized `nvapi64.dll`. Each [`Nvapi::lookup`] opens its own
/// DRS session (load ~110 ms, ~18 MiB while open) and destroys it before
/// returning, so nothing is held between lookups.
pub struct Nvapi {
    unload: Plain,
    create: CreateSession,
    destroy: WithSession,
    load: WithSession,
    find: FindApplication,
    profile_info: GetProfileInfo,
    get_setting: GetSetting,
    _lib: Library,
}

impl Nvapi {
    /// Load the driver's NvAPI and initialize it. Fails on a PC without an
    /// NVIDIA driver (no `nvapi64.dll`) or without an NVIDIA GPU.
    pub fn load() -> Result<Self, String> {
        let lib = load_system_nvapi()?;
        // SAFETY: the export's signature is fixed by nvapi.h.
        let qi: QueryInterface = unsafe { *lib.get::<QueryInterface>(b"nvapi_QueryInterface\0").map_err(|e| format!("nvapi_QueryInterface: {e}"))? };
        let get = |id: u32| -> Result<*mut c_void, String> {
            // SAFETY: QueryInterface takes any id and returns null when unknown.
            let p = unsafe { qi(id) };
            if p.is_null() { Err(format!("nvapi_QueryInterface(0x{id:08X}) returned null")) } else { Ok(p) }
        };
        // SAFETY: as below.
        let init = unsafe { std::mem::transmute::<*mut c_void, Plain>(get(ID_INITIALIZE)?) };
        // SAFETY (all transmutes): each id maps to the nvapi.h signature named by the target type.
        let nv = unsafe {
            Nvapi {
                unload: std::mem::transmute::<*mut c_void, Plain>(get(ID_UNLOAD)?),
                create: std::mem::transmute::<*mut c_void, CreateSession>(get(ID_CREATE_SESSION)?),
                destroy: std::mem::transmute::<*mut c_void, WithSession>(get(ID_DESTROY_SESSION)?),
                load: std::mem::transmute::<*mut c_void, WithSession>(get(ID_LOAD_SETTINGS)?),
                find: std::mem::transmute::<*mut c_void, FindApplication>(get(ID_FIND_APPLICATION)?),
                profile_info: std::mem::transmute::<*mut c_void, GetProfileInfo>(get(ID_GET_PROFILE_INFO)?),
                get_setting: std::mem::transmute::<*mut c_void, GetSetting>(get(ID_GET_SETTING)?),
                _lib: lib,
            }
        };
        // SAFETY: NvAPI_Initialize takes no arguments.
        match unsafe { init() } {
            NVAPI_OK => Ok(nv),
            rc => {
                // No Unload for a failed Initialize: NvAPI counts only successful ones.
                std::mem::forget(nv);
                Err(format!("NvAPI_Initialize failed ({rc}): no NVIDIA GPU?"))
            }
        }
    }

    /// Look up each application (a full exe path matches the driver's folder
    /// rules best; a bare exe name also works) in one read-only session.
    pub fn lookup(&self, apps: &[&str]) -> Result<Vec<Lookup>, String> {
        let mut session: Handle = std::ptr::null_mut();
        // SAFETY: out-pointer to a local.
        let rc = unsafe { (self.create)(&mut session) };
        if rc != NVAPI_OK {
            return Err(format!("NvAPI_DRS_CreateSession failed ({rc})"));
        }
        let out = Session { nv: self, handle: session };
        // SAFETY: a live session.
        let rc = unsafe { (self.load)(session) };
        if rc != NVAPI_OK {
            return Err(format!("NvAPI_DRS_LoadSettings failed ({rc})"));
        }
        apps.iter().map(|a| out.find(a)).collect()
    }
}

impl Drop for Nvapi {
    fn drop(&mut self) {
        // SAFETY: pairs the Initialize in `load` (NvAPI counts them).
        unsafe { (self.unload)() };
    }
}

/// A DRS session, destroyed on drop.
struct Session<'a> {
    nv: &'a Nvapi,
    handle: Handle,
}

impl Session<'_> {
    fn find(&self, app: &str) -> Result<Lookup, String> {
        let mut name: Box<Unicode> = Box::new([0; 2048]);
        for (dst, src) in name.iter_mut().zip(app.encode_utf16().take(2047)) {
            *dst = src;
        }
        // SAFETY: Application is plain data; all-zero is a valid value.
        let mut entry: Box<Application> = Box::new(unsafe { std::mem::zeroed() });
        entry.version = app_version();
        let mut profile: Handle = std::ptr::null_mut();
        // SAFETY: live session; name is NUL-terminated; out-pointers to owned memory.
        match unsafe { (self.nv.find)(self.handle, name.as_ptr(), &mut profile, &mut *entry) } {
            NVAPI_OK => {}
            EXE_NOT_FOUND => return Ok(Lookup::NoProfile),
            rc => return Err(format!("NvAPI_DRS_FindApplicationByName failed ({rc})")),
        }
        // SAFETY: as above.
        let mut info: Box<Profile> = Box::new(unsafe { std::mem::zeroed() });
        info.version = profile_version();
        // SAFETY: live session and profile handle.
        let rc = unsafe { (self.nv.profile_info)(self.handle, profile, &mut *info) };
        let name = if rc == NVAPI_OK { wide(&info.profile_name) } else { String::new() };
        // SAFETY: as above.
        let mut setting: Box<Setting> = Box::new(unsafe { std::mem::zeroed() });
        setting.version = setting_version();
        // SAFETY: live session and profile handle; out-pointer to owned memory.
        let marker = match unsafe { (self.nv.get_setting)(self.handle, profile, GAME_MARKER_ID, &mut *setting) } {
            NVAPI_OK => Some(u32::from_le_bytes([setting.current[0], setting.current[1], setting.current[2], setting.current[3]])),
            SETTING_NOT_FOUND => None,
            rc => return Err(format!("NvAPI_DRS_GetSetting failed ({rc})")),
        };
        Ok(Lookup::Profile { name, marker })
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        // SAFETY: the session from CreateSession, destroyed once.
        unsafe { (self.nv.destroy)(self.handle) };
    }
}

/// `nvapi64.dll` from System32 only, never from the program's folder or PATH.
pub(crate) fn load_system_nvapi() -> Result<Library, String> {
    use libloading::os::windows::{Library as WinLibrary, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // SAFETY: nvapi64.dll is the NVIDIA driver's own library; loading it runs its DllMain only.
    unsafe { WinLibrary::load_with_flags("nvapi64.dll", LOAD_LIBRARY_SEARCH_SYSTEM32) }.map(Library::from).map_err(|e| format!("nvapi64.dll not loaded from System32: {e}"))
}

pub(crate) fn wide(s: &Unicode) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}
