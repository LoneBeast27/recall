//! Which ETW events are presents, and which of them count as the target's frames.
//!
//! Provider GUIDs, event ids, keywords and payload layouts are taken from
//! Intel PresentMon (MIT License, github.com/GameTechDev/PresentMon, `main`,
//! read 2026-10-26): `PresentData/ETW/Microsoft_Windows_DXGI.h`,
//! `Microsoft_Windows_D3D9.h`, `Microsoft_Windows_DxgKrnl.h`, the enable
//! lists and keyword patches in `PresentData/PresentMonTraceSession.cpp`, and
//! the event handling in `PresentData/PresentMonTraceConsumer.cpp`
//! (`HandleDXGIEvent`, `HandleD3D9Event`, `RuntimePresentStart`,
//! `FindOrCreatePresent`, the `Present_Info` case).
//!
//! - DXGI `Present_Start` (42) / `PresentMultiplaneOverlay_Start` (55): one per
//!   `IDXGISwapChain::Present`, i.e. every D3D10/11/12 game (VALORANT is
//!   D3D11). Payload: `pIDXGISwapChain` (pointer), `Flags` (u32),
//!   `SyncInterval` (i32). `DXGI_PRESENT_TEST` presents show nothing and are
//!   skipped, as PresentMon's `RuntimePresentStart` does.
//! - D3D9 `Present_Start` (1): payload `pSwapchain` (pointer), `Flags` (u32).
//! - DxgKrnl `Present_Info` (184): emitted at the end of every kernel present
//!   call, on the presenting thread. PresentMon counts kernel-only presents as
//!   `Runtime::Other` (Vulkan, OpenGL); here it is the fallback for a process
//!   that shows no DXGI/D3D9 presents, so a D3D game's presents (which also
//!   reach the kernel) are never counted twice.

use windows::core::GUID;

pub const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
pub const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
pub const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);

pub const DXGI_PRESENT_START: u16 = 0x002a;
pub const DXGI_PRESENT_MPO_START: u16 = 0x0037;
pub const D3D9_PRESENT_START: u16 = 0x0001;
pub const DXGKRNL_PRESENT_INFO: u16 = 0x00b8;

/// `DXGI_PRESENT_TEST`: an occlusion check, not a frame.
const DXGI_PRESENT_TEST: u32 = 0x1;
/// The runtimes' present events: `Microsoft_Windows_*_Analytic | Events`.
const RUNTIME_KEYWORD: u64 = 0x8000_0000_0000_0002;
/// `Present_Info`'s keyword `0x4000000008000001` minus the Performance
/// keyword (PresentMon's `PatchKeyword`: filtering DxgKrnl on Performance has
/// costly side effects on some Windows versions) = `Present | Base`.
const DXGKRNL_PRESENT_KEYWORD: u64 = 0x0000_0000_0800_0001;

/// How one provider is enabled on the session.
pub struct ProviderSpec {
    pub guid: GUID,
    pub keyword: u64,
    pub event_ids: &'static [u16],
    /// `EVENT_FILTER_TYPE_PID` applies: user-mode providers only (a PID filter
    /// would leave a kernel-mode provider such as DxgKrnl disabled).
    pub pid_filter: bool,
}

pub const PROVIDERS: [ProviderSpec; 3] = [
    ProviderSpec {
        guid: DXGI,
        keyword: RUNTIME_KEYWORD,
        event_ids: &[DXGI_PRESENT_START, DXGI_PRESENT_MPO_START],
        pid_filter: true,
    },
    ProviderSpec { guid: D3D9, keyword: RUNTIME_KEYWORD, event_ids: &[D3D9_PRESENT_START], pid_filter: true },
    ProviderSpec { guid: DXGKRNL, keyword: DXGKRNL_PRESENT_KEYWORD, event_ids: &[DXGKRNL_PRESENT_INFO], pid_filter: false },
];

/// Where a present was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// DXGI or D3D9 (the API call itself).
    Runtime,
    /// DxgKrnl (any API, including Vulkan and OpenGL).
    Kernel,
}

/// The parts of an `EVENT_RECORD` the mapping needs.
pub struct RawEvent<'a> {
    pub provider: GUID,
    pub id: u16,
    /// Pointer fields in the payload are 4 bytes for a 32-bit process.
    pub pointer_size: usize,
    pub payload: &'a [u8],
}

/// A present (and from where), or `None` for any other event.
pub fn classify(event: &RawEvent) -> Option<Source> {
    let id = event.id;
    if event.provider == DXGI && (id == DXGI_PRESENT_START || id == DXGI_PRESENT_MPO_START) {
        let test = flags_after_pointer(event).is_some_and(|flags| flags & DXGI_PRESENT_TEST != 0);
        return (!test).then_some(Source::Runtime);
    }
    if event.provider == D3D9 && id == D3D9_PRESENT_START {
        return Some(Source::Runtime);
    }
    (event.provider == DXGKRNL && id == DXGKRNL_PRESENT_INFO).then_some(Source::Kernel)
}

/// The `Flags` u32 that follows the leading swap-chain pointer.
fn flags_after_pointer(event: &RawEvent) -> Option<u32> {
    let at = event.pointer_size;
    let bytes = event.payload.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// What one present does to the frame window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapped {
    Ignore,
    Frame(i64),
    /// The counted source changed (kernel fallback ↔ runtime): drop the old
    /// timestamps, then add this one.
    Restart(i64),
}

/// Turns the target's presents into frames, one source at a time: runtime
/// presents always count; kernel presents count only while the process has
/// shown no runtime present within [`FrameMapper::grace`].
#[derive(Debug)]
pub struct FrameMapper {
    target: u32,
    grace: i64,
    last_runtime: Option<i64>,
    counting: Option<Source>,
}

impl FrameMapper {
    /// `target` 0 = nothing counts. Grace = 2 s of `ticks_per_sec`.
    pub fn new(target: u32, ticks_per_sec: i64) -> Self {
        Self { target, grace: 2 * ticks_per_sec.max(1), last_runtime: None, counting: None }
    }

    pub fn map(&mut self, pid: u32, source: Source, at: i64) -> Mapped {
        if pid != self.target || self.target == 0 {
            return Mapped::Ignore;
        }
        if source == Source::Kernel
            && self.last_runtime.is_some_and(|runtime| at <= runtime.saturating_add(self.grace))
        {
            return Mapped::Ignore; // the same frame, already counted from DXGI / D3D9
        }
        if source == Source::Runtime {
            self.last_runtime = Some(self.last_runtime.map_or(at, |runtime| runtime.max(at)));
        }
        let switched = self.counting.is_some_and(|counting| counting != source);
        self.counting = Some(source);
        if switched {
            Mapped::Restart(at)
        } else {
            Mapped::Frame(at)
        }
    }
}

#[cfg(test)]
#[path = "present_events_tests.rs"]
mod tests;
