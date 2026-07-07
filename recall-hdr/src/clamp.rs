//! Gamut-clamp drivers: the `GamutClamp` trait plus a no-op and the real NvAPI
//! color-space-conversion (CSC) implementation.
//!
//! # What the NvAPI clamp actually is
//! NVIDIA's driver has an undocumented per-display "color space conversion" stage
//! (the same one MS "Advanced color" / the driver's own sRGB clamp use). ledoge's
//! novideo_srgb reverse-engineered its private struct + two `nvapi_QueryInterface`
//! function IDs. We port that here: to clamp a wide-gamut panel to sRGB we push a
//! 3x3 `RGBToRGB(sRGB -> panel_native_primaries)` matrix into `matrix1`; to reset
//! we write the driver-default CSC (contentColorSpace=2, monitorColorSpace=0),
//! which disables the stage and restores the panel's native gamut.
//!
//! # Hard constraints (from the field, do not relearn the hard way)
//! - The clamp is REJECTED while Windows HDR is ON (driver constraint since
//!   531.79 -> error -104 NVAPI_NOT_SUPPORTED). Callers must ensure SDR first.
//! - Confirmed working on RTX 5090 / driver 596.36 via novideo_srgb v4.2-fix, so
//!   any error from *our* SET is an FFI-layout bug, not a driver limitation.
//! - The `version` field is the classic footgun: it is a packed
//!   MAKE_NVAPI_VERSION value, and for the V1 `Csc` struct ledoge determined it is
//!   the literal `0x1007C`. We use that constant verbatim (do NOT recompute it).
//!
//! # FFI shape
//! `nvapi64.dll` exports exactly one symbol, `nvapi_QueryInterface(u32 id) -> *fn`.
//! Every real function is resolved by its magic hex ID through that. All resolved
//! functions use the C (cdecl) calling convention and return an `i32` status.

use std::ffi::c_void;

use crate::edid::{self, EdidPrimaries};
use crate::log;
use crate::matrix::{rgb_to_rgb, ColorSpace, SRGB};

/// Whether a driver can currently service clamp requests.
#[derive(Debug, Clone)]
pub enum Capability {
    Available,
    Unavailable(String),
}

/// A gamut-clamp backend. Wave 1's reconciler holds a `Box<dyn GamutClamp>` and
/// never cares which concrete driver it is.
pub trait GamutClamp {
    /// Cheap capability check (may re-probe). Never mutates display state.
    fn capability(&self) -> Capability;
    /// Apply the given clamp intensity. `Off`/`Punchy` reset to native gamut;
    /// `Accurate` engages the sRGB clamp.
    fn apply(&mut self, mode: crate::types::ClampMode) -> Result<(), String>;
    /// Force the display back to its native (unclamped) gamut.
    fn reset(&mut self) -> Result<(), String>;
}

/// A driver that does nothing but log -- used when NvAPI is unavailable or the
/// user disabled clamp management. Every call succeeds.
pub struct NoopClamp;

impl GamutClamp for NoopClamp {
    fn capability(&self) -> Capability {
        Capability::Available
    }
    fn apply(&mut self, mode: crate::types::ClampMode) -> Result<(), String> {
        log!("NoopClamp.apply({mode:?}) -- no-op");
        Ok(())
    }
    fn reset(&mut self) -> Result<(), String> {
        log!("NoopClamp.reset() -- no-op");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NvAPI FFI
// ---------------------------------------------------------------------------

/// Magic `nvapi_QueryInterface` function IDs. The three general ones come from
/// NVIDIA's public `nvapi_interface.h`; the two CSC ones are ledoge's
/// reverse-engineered private IDs (not in any NVIDIA header).
mod ids {
    pub const INITIALIZE: u32 = 0x0150_E828;
    pub const UNLOAD: u32 = 0xD22B_DD7E;
    pub const GET_ERROR_MESSAGE: u32 = 0x6C2D_048C;
    pub const DISP_GET_GDI_PRIMARY_DISPLAY_ID: u32 = 0x1E9D_8A31;
    /// Private (ledoge). Get current CSC for a displayId.
    pub const GPU_GET_COLOR_SPACE_CONVERSION: u32 = 0x8159_E87A;
    /// Private (ledoge). Set CSC for a displayId.
    pub const GPU_SET_COLOR_SPACE_CONVERSION: u32 = 0xFCAB_D23A;
}

/// V1 CSC version tag. This is a packed NvAPI version value; ledoge determined
/// the constant is `0x1007C`. We treat it as opaque and never recompute it.
const CSC_VERSION_V1: u32 = 0x0001_007C;

/// The reverse-engineered NvAPI CSC struct, V1 layout (matrices only, no LUT).
///
/// `#[repr(C)]` + field order MUST match ledoge's C# `Csc` exactly:
/// ```text
/// uint version; uint contentColorSpace; uint monitorColorSpace;
/// uint unknown1; uint unknown2; uint useMatrix1;
/// float matrix1[12]; uint useMatrix2; float matrix2[12];
/// // V2-only trailer (pointers + size). For V1 we still declare them so the
/// // struct we hand the driver is exactly the size it wrote back on GET; they
/// // are left null/zero and ignored when version == V1.
/// float* degamma; float* regamma; float* buffer; int bufferSize;
/// ```
///
/// The matrices are `[f32; 12]` = a 3x4 row-major matrix (`m[i*4 + j]`): the
/// first three columns are the 3x3 gain, the fourth column is a per-row offset.
#[repr(C)]
#[derive(Clone, Copy)]
struct Csc {
    version: u32,
    content_color_space: u32,
    monitor_color_space: u32,
    unknown1: u32,
    unknown2: u32,
    use_matrix1: u32,
    matrix1: [f32; 12],
    use_matrix2: u32,
    matrix2: [f32; 12],
    // V2 trailer -- present so the struct footprint is stable; null for V1.
    degamma: *mut f32,
    regamma: *mut f32,
    buffer: *mut f32,
    buffer_size: i32,
}

impl Csc {
    /// A zeroed V1 struct ready for GET (driver fills the rest).
    fn v1() -> Self {
        Csc {
            version: CSC_VERSION_V1,
            content_color_space: 0,
            monitor_color_space: 0,
            unknown1: 0,
            unknown2: 0,
            use_matrix1: 0,
            matrix1: [0.0; 12],
            use_matrix2: 0,
            matrix2: [0.0; 12],
            degamma: std::ptr::null_mut(),
            regamma: std::ptr::null_mut(),
            buffer: std::ptr::null_mut(),
            buffer_size: 0,
        }
    }

    /// Is the CSC stage effectively active? Mirrors ledoge's
    /// `IsColorSpaceConversionActive`: monitorColorSpace 0 = disabled; the
    /// unity-HDR case (12/12 with no matrices) also counts as inactive.
    fn is_active(&self) -> bool {
        match self.monitor_color_space {
            0 => false,
            12 if self.content_color_space == 12
                && self.use_matrix1 == 0
                && self.use_matrix2 == 0 =>
            {
                false
            }
            _ => true,
        }
    }

    /// Short human summary for logs.
    fn summary(&self) -> String {
        format!(
            "content={} monitor={} useM1={} useM2={} active={} m1[0..3]=[{:.4},{:.4},{:.4}]",
            self.content_color_space,
            self.monitor_color_space,
            self.use_matrix1,
            self.use_matrix2,
            self.is_active(),
            self.matrix1[0],
            self.matrix1[1],
            self.matrix1[2],
        )
    }
}

// cdecl signatures resolved from nvapi_QueryInterface.
type FnInitialize = unsafe extern "C" fn() -> i32;
type FnUnload = unsafe extern "C" fn() -> i32;
type FnGetErrorMessage = unsafe extern "C" fn(i32, *mut u8) -> i32;
type FnGetGdiPrimaryDisplayId = unsafe extern "C" fn(*mut u32) -> i32;
type FnGetCsc = unsafe extern "C" fn(u32, *mut Csc) -> i32;
type FnSetCsc = unsafe extern "C" fn(u32, *mut Csc) -> i32;
/// The one true export.
type FnQueryInterface = unsafe extern "C" fn(u32) -> *const c_void;

/// Loaded + initialized NvAPI entry points for the primary display.
pub struct NvApiClamp {
    /// Kept alive for the lifetime of the resolved fn pointers.
    _lib: libloading::Library,
    query: FnQueryInterface,
    get_csc: FnGetCsc,
    set_csc: FnSetCsc,
    get_error: Option<FnGetErrorMessage>,
    display_id: u32,
    /// Native panel primaries (from EDID) used to build the sRGB clamp matrix.
    /// `None` if EDID couldn't be read -> `Accurate` clamp is unavailable.
    primaries: Option<EdidPrimaries>,
}

impl NvApiClamp {
    /// Load nvapi64.dll, initialize, resolve the CSC fns, find the primary
    /// displayId, and read panel primaries. Does NOT mutate display state.
    pub fn init() -> Result<Self, String> {
        // SAFETY: loading the system NvAPI DLL and resolving its single export.
        let lib = unsafe { libloading::Library::new("nvapi64.dll") }
            .map_err(|e| format!("load nvapi64.dll failed: {e}"))?;

        let query: FnQueryInterface = unsafe {
            let sym: libloading::Symbol<FnQueryInterface> = lib
                .get(b"nvapi_QueryInterface\0")
                .map_err(|e| format!("resolve nvapi_QueryInterface failed: {e}"))?;
            *sym
        };

        // Resolve a function ID into a typed fn pointer, or a descriptive error.
        // SAFETY: transmute of a resolved, non-null nvapi entry point to its
        // known cdecl signature.
        let resolve = |id: u32, name: &str| -> Result<*const c_void, String> {
            let p = unsafe { query(id) };
            if p.is_null() {
                Err(format!("nvapi_QueryInterface({name}=0x{id:08X}) returned null"))
            } else {
                Ok(p)
            }
        };

        let init_ptr = resolve(ids::INITIALIZE, "NvAPI_Initialize")?;
        let init_fn: FnInitialize = unsafe { std::mem::transmute(init_ptr) };
        let st = unsafe { init_fn() };
        if st != 0 {
            return Err(format!("NvAPI_Initialize failed: status {st}"));
        }

        let get_error = resolve(ids::GET_ERROR_MESSAGE, "NvAPI_GetErrorMessage")
            .ok()
            .map(|p| unsafe { std::mem::transmute::<_, FnGetErrorMessage>(p) });

        let gdi_ptr = resolve(
            ids::DISP_GET_GDI_PRIMARY_DISPLAY_ID,
            "NvAPI_DISP_GetGDIPrimaryDisplayId",
        )?;
        let gdi_fn: FnGetGdiPrimaryDisplayId = unsafe { std::mem::transmute(gdi_ptr) };
        let mut display_id: u32 = 0;
        let st = unsafe { gdi_fn(&mut display_id) };
        if st != 0 {
            return Err(format!("NvAPI_DISP_GetGDIPrimaryDisplayId failed: status {st}"));
        }
        log!("NvAPI primary displayId = 0x{display_id:08X}");

        let get_ptr = resolve(
            ids::GPU_GET_COLOR_SPACE_CONVERSION,
            "NvAPI_GPU_GetColorSpaceConversion",
        )?;
        let get_csc: FnGetCsc = unsafe { std::mem::transmute(get_ptr) };

        let set_ptr = resolve(
            ids::GPU_SET_COLOR_SPACE_CONVERSION,
            "NvAPI_GPU_SetColorSpaceConversion",
        )?;
        let set_csc: FnSetCsc = unsafe { std::mem::transmute(set_ptr) };

        // Best-effort EDID read for the primary display's native primaries.
        let primaries = match edid::primary_display_primaries() {
            Ok(p) => {
                log!(
                    "EDID primaries: R({:.3},{:.3}) G({:.3},{:.3}) B({:.3},{:.3}) W({:.3},{:.3})",
                    p.red.0, p.red.1, p.green.0, p.green.1, p.blue.0, p.blue.1, p.white.0, p.white.1
                );
                Some(p)
            }
            Err(e) => {
                log!("EDID read failed ({e}); Accurate clamp will be unavailable");
                None
            }
        };

        Ok(NvApiClamp {
            _lib: lib,
            query,
            get_csc,
            set_csc,
            get_error,
            display_id,
            primaries,
        })
    }

    /// Translate an NvAPI status into "code + name" using the driver's own
    /// message table when available.
    fn err_str(&self, status: i32) -> String {
        let mut name = String::new();
        if let Some(f) = self.get_error {
            let mut buf = [0u8; 64];
            let st = unsafe { f(status, buf.as_mut_ptr()) };
            if st == 0 {
                let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                name = String::from_utf8_lossy(&buf[..end]).into_owned();
            }
        }
        if name.is_empty() {
            format!("status {status}")
        } else {
            format!("status {status} ({name})")
        }
    }

    /// GET the current CSC state. Never mutates. Crate-private because `Csc` is
    /// an internal FFI type; callers outside this module use [`Self::get_summary`].
    fn get(&self) -> Result<Csc, String> {
        let mut csc = Csc::v1();
        let st = unsafe { (self.get_csc)(self.display_id, &mut csc) };
        if st != 0 {
            return Err(format!(
                "NvAPI_GPU_GetColorSpaceConversion failed: {}",
                self.err_str(st)
            ));
        }
        Ok(csc)
    }

    /// Public wrapper returning a printable GET summary (for --probe / tests).
    pub fn get_summary(&self) -> Result<String, String> {
        Ok(self.get()?.summary())
    }

    /// Build the sRGB-clamp CSC (matrix1 = RGBToRGB(sRGB -> panel primaries)).
    fn build_clamp(&self) -> Result<Csc, String> {
        let prim = self
            .primaries
            .as_ref()
            .ok_or_else(|| "no EDID primaries -> cannot build sRGB clamp".to_string())?;
        let panel = ColorSpace::from(prim);
        // Panel-native RGB in, sRGB out clamp: incoming sRGB-encoded content is
        // remapped so full-saturation sRGB maps to sRGB gamut on the wide panel.
        let m = rgb_to_rgb(&SRGB, &panel);

        let mut csc = Csc::v1();
        csc.content_color_space = 2; // sRGB, matches novideo's MatrixToColorSpaceConversion
        csc.monitor_color_space = 2;
        csc.use_matrix1 = 1;
        for i in 0..3 {
            for j in 0..3 {
                csc.matrix1[i * 4 + j] = m[i][j] as f32;
            }
            // Column 4 (offset) stays 0.
        }
        Ok(csc)
    }

    /// The driver-default (disabled) CSC. Mirrors ledoge's
    /// `DisableColorSpaceConversion`: contentColorSpace=2, everything else 0.
    fn build_reset(&self) -> Csc {
        let mut csc = Csc::v1();
        csc.content_color_space = 2;
        csc.monitor_color_space = 0;
        csc.use_matrix1 = 0;
        csc.use_matrix2 = 0;
        csc
    }

    /// Low-level SET with a pre-built struct.
    fn set(&self, csc: &mut Csc) -> Result<(), String> {
        let st = unsafe { (self.set_csc)(self.display_id, csc) };
        if st != 0 {
            return Err(format!(
                "NvAPI_GPU_SetColorSpaceConversion failed: {}",
                self.err_str(st)
            ));
        }
        Ok(())
    }
}

impl GamutClamp for NvApiClamp {
    fn capability(&self) -> Capability {
        // We can always reset; "Accurate" needs EDID primaries. Report the
        // richer state so the UI can explain a degraded mode.
        match self.get() {
            Ok(_) if self.primaries.is_some() => Capability::Available,
            Ok(_) => Capability::Unavailable(
                "GET ok but no EDID primaries -> sRGB clamp unavailable".into(),
            ),
            Err(e) => Capability::Unavailable(e),
        }
    }

    fn apply(&mut self, mode: crate::types::ClampMode) -> Result<(), String> {
        use crate::types::ClampMode::*;
        match mode {
            Accurate => {
                let mut csc = self.build_clamp()?;
                log!("NvApiClamp.apply(Accurate) -> engaging sRGB clamp");
                self.set(&mut csc)
            }
            // Punchy = panel-native = no clamp; Off = don't manage -> both reset.
            Punchy | Off => self.reset(),
        }
    }

    fn reset(&mut self) -> Result<(), String> {
        let mut csc = self.build_reset();
        log!("NvApiClamp.reset() -> disabling CSC (native gamut)");
        self.set(&mut csc)
    }
}

impl Drop for NvApiClamp {
    fn drop(&mut self) {
        // Best-effort NvAPI_Unload so we don't leak the driver handle.
        if let Ok(p) = {
            let p = unsafe { (self.query)(ids::UNLOAD) };
            if p.is_null() {
                Err(())
            } else {
                Ok(p)
            }
        } {
            let unload: FnUnload = unsafe { std::mem::transmute(p) };
            unsafe {
                let _ = unload();
            }
        }
    }
}

/// Read-only reachability probe: init NvAPI, GET current CSC, report Capability.
/// Never mutates display state. Returns `(capability, get_summary_or_error)`.
pub fn probe() -> (Capability, String) {
    match NvApiClamp::init() {
        Ok(clamp) => {
            let cap = clamp.capability();
            let summary = clamp
                .get_summary()
                .unwrap_or_else(|e| format!("GET failed: {e}"));
            (cap, summary)
        }
        Err(e) => (Capability::Unavailable(e.clone()), e),
    }
}
