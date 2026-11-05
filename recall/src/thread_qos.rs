//! Background threads that must not take a game's CPU: below-normal thread priority plus
//! EcoQoS (`THREAD_POWER_THROTTLING_EXECUTION_SPEED`). With EcoQoS Windows prefers the
//! slowest cores at low clocks for the thread (E-cores on Intel hybrid CPUs), so save-time
//! work stays off the cores a game runs on. Capture and the live encoder never call this.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetThreadTimes, SetThreadInformation, SetThreadPriority, ThreadPowerThrottling, THREAD_POWER_THROTTLING_CURRENT_VERSION,
    THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE, THREAD_PRIORITY_BELOW_NORMAL,
};

/// Lower the calling thread: below-normal priority, then EcoQoS. Returns false when Windows
/// refused EcoQoS (older than Windows 11 21H2 ignores it); the priority still applies.
pub(crate) fn eco_background() -> bool {
    // SAFETY: GetCurrentThread is this thread's pseudo-handle; both calls touch only this thread.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    let state = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
    };
    // SAFETY: a correctly sized THREAD_POWER_THROTTLING_STATE for ThreadPowerThrottling.
    unsafe {
        SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            (&state as *const THREAD_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .is_ok()
}

/// EcoQoS for the whole process (every thread, present and future): a pre-encode the app
/// starts at idle (`RECALL_ECOQOS=1`). Returns false when Windows refused it.
pub(crate) fn eco_process() -> bool {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    // SAFETY: a correctly sized PROCESS_POWER_THROTTLING_STATE for this process's pseudo-handle.
    unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .is_ok()
}

/// CPU time (user + kernel) the calling thread has used so far, in ms.
pub(crate) fn thread_cpu_ms() -> f64 {
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: four out-params for this thread's own pseudo-handle.
    if unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) }.is_err() {
        return 0.0;
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64;
    (ticks(kernel) + ticks(user)) / 10_000.0
}

#[cfg(test)]
mod tests {
    #[test]
    fn eco_background_lowers_this_thread() {
        let lowered = std::thread::spawn(|| {
            let eco = super::eco_background();
            // SAFETY: reads this thread's own priority.
            let priority = unsafe { windows::Win32::System::Threading::GetThreadPriority(windows::Win32::System::Threading::GetCurrentThread()) };
            (eco, priority)
        })
        .join()
        .unwrap();
        assert!(lowered.0, "EcoQoS refused");
        assert_eq!(lowered.1, -1, "below-normal priority");
    }

    #[test]
    fn thread_cpu_counts_work() {
        let before = super::thread_cpu_ms();
        let mut x = 0u64;
        let started = std::time::Instant::now();
        while started.elapsed().as_millis() < 60 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        assert!(super::thread_cpu_ms() - before >= 30.0);
    }
}
