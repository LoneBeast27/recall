//! Startup timeline (`RECALL_NATIVE_STARTUP=<file>`): one JSON line per step, with the time
//! since the process was created (so the loader and DLL mapping before `main` count too).
//! Off unless the variable is set: a mark is then one relaxed load.
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Timeline {
    file: std::sync::Mutex<std::fs::File>,
    created: SystemTime,
}

static TIMELINE: OnceLock<Option<Timeline>> = OnceLock::new();

/// When Windows created this process (falls back to now).
fn process_created() -> SystemTime {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.is_err() {
        return SystemTime::now();
    }
    // FILETIME: 100 ns ticks since 1601-01-01.
    let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    let unix_ticks = ticks.saturating_sub(116_444_736_000_000_000);
    UNIX_EPOCH + Duration::from_nanos(unix_ticks * 100)
}

fn timeline() -> Option<&'static Timeline> {
    if crate::TESTING {
        return None;
    }
    TIMELINE
        .get_or_init(|| {
            let path = std::env::var_os("RECALL_NATIVE_STARTUP")?;
            let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
            Some(Timeline { file: std::sync::Mutex::new(file), created: process_created() })
        })
        .as_ref()
}

/// Records `step` with its time since process creation and the process CPU so far.
pub fn mark(step: &str) {
    let Some(timeline) = timeline() else { return };
    let ms = SystemTime::now().duration_since(timeline.created).unwrap_or_default().as_secs_f64() * 1000.0;
    let line = serde_json::json!({ "step": step, "ms": (ms * 10.0).round() / 10.0, "cpu_ms": cpu_ms(), "thread": thread_name() });
    if let Ok(mut file) = timeline.file.lock() {
        let _ = writeln!(file, "{line}");
    }
}

/// Times `work` as one step (`<step>` marks its end, with its own duration).
pub fn span<T>(step: &str, work: impl FnOnce() -> T) -> T {
    if timeline().is_none() {
        return work();
    }
    let started = std::time::Instant::now();
    let value = work();
    mark(&format!("{step} ({:.1} ms)", started.elapsed().as_secs_f64() * 1000.0));
    value
}

fn cpu_ms() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.is_err() {
        return 0;
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    (ticks(kernel) + ticks(user)) / 10_000
}

fn thread_name() -> String {
    std::thread::current().name().unwrap_or("?").to_owned()
}

/// CPU time of the calling thread so far, in microseconds.
fn thread_cpu_us() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) }.is_err() {
        return 0;
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    (ticks(kernel) + ticks(user)) / 10
}

/// Background work item: logs wall and this thread's CPU when it takes at least `min_ms` of either.
pub fn task<T>(label: impl FnOnce() -> String, min_ms: f64, work: impl FnOnce() -> T) -> T {
    if timeline().is_none() {
        return work();
    }
    let (started, cpu0) = (std::time::Instant::now(), thread_cpu_us());
    let value = work();
    let wall = started.elapsed().as_secs_f64() * 1000.0;
    let cpu = thread_cpu_us().saturating_sub(cpu0) as f64 / 1000.0;
    if wall >= min_ms || cpu >= min_ms {
        mark(&format!("task {} wall={wall:.1} thread_cpu={cpu:.1}", label()));
    }
    value
}
