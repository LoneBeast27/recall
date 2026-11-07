use super::*;
use crate::render_verdict::MAX_WATCH;

#[test]
fn payload_fields_read_little_endian_and_never_past_the_end() {
    let payload = [0x01, 0x02, 0, 0, 0, 0, 0, 0, 0x00, 0x02, 0, 0];
    assert_eq!(read_uint(&payload, 0, 8), Some(0x0201));
    assert_eq!(read_uint(&payload, 8, 4), Some(0x200), "DXGI Flags after a 64-bit pointer");
    assert_eq!(read_uint(&payload, 10, 4), None);
    assert_eq!(read_uint(&payload, usize::MAX, 4), None);
}

#[test]
fn stopping_a_session_that_does_not_run_is_harmless() {
    // Never stops a live probe: tests do not start one outside the ignored check.
    if !session_running() {
        assert!(!stop_stale());
    }
}

/// Live check (ignored: needs trace rights and a GPU). Presents from a hidden
/// window in this process (never shown, no audio), watches them, and measures
/// the session's start/stop cost and event delivery delay.
/// `cargo test -p recall-hdr live_probe -- --ignored --nocapture --test-threads=1`
#[test]
#[ignore]
fn live_probe_counts_this_process_presents() {
    assert!(!session_running(), "a RecallDetectRender session is already running");
    let pid = std::process::id();
    let presenter = hidden::Presenter::new();
    let probe = RenderProbe::start(pid).expect("start (needs Performance Log Users)");
    assert!(session_running());
    let start_cost = probe.start_cost;
    // Delivery delay: one present, then wait until the callback has it (5 rounds).
    let mut delays = Vec::new();
    for round in 1..=5u32 {
        let t = Instant::now();
        presenter.present();
        while probe.stats().runtime_presents < round && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        delays.push(t.elapsed().as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(37)); // de-phase from the flush timer
    }
    for _ in 0..55 {
        presenter.present();
        std::thread::sleep(Duration::from_millis(8));
    }
    std::thread::sleep(Duration::from_millis(150)); // > FLUSH_MS: the last buffer arrives
    let stats = probe.stats();
    let t = Instant::now();
    drop(probe);
    let stop_cost = t.elapsed();
    let running_after = session_running();
    println!(
        "RENDER_PROBE pid={pid} presents_called=60 stats={stats:?} start_ms={:.1} delivery_ms={delays:.1?} stop_ms={:.1}",
        start_cost.as_secs_f64() * 1e3,
        stop_cost.as_secs_f64() * 1e3,
    );
    assert!(!running_after, "the session must stop on drop");
    assert!(stats.runtime_presents >= 54, "runtime presents seen: {stats:?}");
    assert!(stats.tearing_presents >= 54, "tearing flag read: {stats:?}");
    assert!(stats.independent_frames + stats.composed_frames >= 54, "DWM frames matched to our surface: {stats:?}");
    assert_eq!(stats.video_presents, 0);
    assert_eq!(stats.legacy_flips, 0, "a hidden window never flips");
}

/// Start + stop cost over several rounds, with nothing presenting from this process.
#[test]
#[ignore]
fn live_probe_start_stop_cost() {
    assert!(!session_running());
    for _ in 0..5 {
        let probe = RenderProbe::start(std::process::id()).expect("start");
        let start = probe.start_cost;
        std::thread::sleep(MAX_WATCH_FOR_COST);
        let t = Instant::now();
        drop(probe);
        println!("RENDER_PROBE_COST start_ms={:.2} stop_ms={:.2}", start.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3);
        assert!(!session_running());
    }
}

/// CPU of this process while it presents ~500 frames a second for 8 s, without
/// and with the probe watching it (a busy game's event rate). GetProcessTimes
/// ticks are 15.6 ms, so 8 s runs resolve 0.2 % of a core.
#[test]
#[ignore]
fn live_probe_cpu_under_load() {
    assert!(!session_running());
    let presenter = hidden::Presenter::new();
    let run = |probe: bool| {
        let probe = probe.then(|| RenderProbe::start(std::process::id()).expect("start"));
        let cpu0 = cpu_time();
        let t = Instant::now();
        let mut n = 0u32;
        while t.elapsed() < Duration::from_secs(8) {
            presenter.present();
            n += 1;
            std::thread::sleep(Duration::from_millis(1));
        }
        let cpu = cpu_time() - cpu0;
        let wall = t.elapsed();
        let seen = probe.as_ref().map(|p| p.stats().runtime_presents);
        (cpu.as_secs_f64() / wall.as_secs_f64() * 100.0, f64::from(n) / wall.as_secs_f64(), seen)
    };
    let (base, rate0, _) = run(false);
    let (watched, rate1, seen) = run(true);
    let (base2, rate2, _) = run(false);
    println!(
        "RENDER_PROBE_LOAD base_pct_core={base:.2} ({rate0:.0} fps) watched_pct_core={watched:.2} ({rate1:.0} fps, seen {seen:?}) base_again_pct_core={base2:.2} ({rate2:.0} fps)"
    );
    assert!(!session_running());
}

const MAX_WATCH_FOR_COST: Duration = MAX_WATCH;

fn cpu_time() -> Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut a, mut b, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u) }.expect("GetProcessTimes");
    let ticks = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Duration::from_nanos((ticks(k) + ticks(u)) * 100)
}

mod hidden {
    use windows::core::{w, Interface};
    use windows::Win32::Foundation::{HMODULE, HWND};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT_ALLOW_TEARING, DXGI_SWAP_CHAIN_DESC1,
        DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP};

    /// A flip-model swap chain that allows tearing, on a window that is never
    /// shown (no `WS_VISIBLE`, no `ShowWindow`).
    pub struct Presenter {
        hwnd: HWND,
        chain: Option<IDXGISwapChain1>,
    }

    impl Presenter {
        pub fn new() -> Self {
            unsafe {
                let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("recall-render-probe-test"), WS_POPUP, 0, 0, 64, 64, None, None, None, None)
                    .expect("hidden window");
                let mut device: Option<ID3D11Device> = None;
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, None)
                    .expect("device");
                let device = device.expect("device");
                let factory: IDXGIFactory2 = CreateDXGIFactory1().expect("factory");
                let desc = DXGI_SWAP_CHAIN_DESC1 {
                    Width: 64,
                    Height: 64,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32,
                    ..Default::default()
                };
                let unknown = device.cast::<windows::core::IUnknown>().expect("device as IUnknown");
                let chain = factory.CreateSwapChainForHwnd(&unknown, hwnd, &desc, None, None).expect("swap chain");
                Presenter { hwnd, chain: Some(chain) }
            }
        }

        pub fn present(&self) {
            if let Some(chain) = &self.chain {
                let _ = unsafe { chain.Present(0, DXGI_PRESENT_ALLOW_TEARING) };
            }
        }
    }

    impl Drop for Presenter {
        fn drop(&mut self) {
            self.chain = None;
            let _ = unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

#[test]
fn the_render_session_name_differs_per_windows_session() {
    // Two signed-in users each run a controller; one's stale-session stop must not end the
    // other's probe (audit G3 leftover, R2-deep-startup-8).
    assert_eq!(session_name_for(Some(1)), "RecallDetectRender-1");
    assert_eq!(session_name_for(Some(2)), "RecallDetectRender-2");
    assert_eq!(session_name_for(None), "RecallDetectRender", "no session id: the bare name");
    let longest = session_name_for(Some(u32::MAX));
    assert!(longest.encode_utf16().count() < 64, "fits the properties' name buffer");
}
