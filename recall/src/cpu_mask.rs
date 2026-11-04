//! The engine's CPU pin (`RECALL_CPU_MASK`), fitted to the machine's CPU count so a mask
//! written for a 32-thread desktop still means "the background half" on a 4- or 8-core laptop.

/// `RECALL_CPU_MASK`: pin to the mask's CPUs that exist (the high half of the machine's
/// CPUs when none do, no pin on 4 or fewer) and drop to BelowNormal. A pin Windows refuses
/// twice exits with code 2, as before: an unpinned engine could take the game's cores.
pub(crate) fn apply(mask: &str) {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask, SetPriorityClass, SetProcessAffinityMask, BELOW_NORMAL_PRIORITY_CLASS};
    let wanted = usize::from_str_radix(mask.trim().trim_start_matches("0x"), 16).unwrap_or(0);
    let (mut process, mut system) = (0usize, 0usize);
    let _ = unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) };
    match fit(wanted, system) {
        Some(m) => {
            let pin = || unsafe { SetProcessAffinityMask(GetCurrentProcess(), m) }.is_ok();
            if !pin() && !pin() {
                eprintln!("Recall CPU affinity 0x{m:x} could not be applied");
                std::process::exit(2);
            }
        }
        None => eprintln!("[recall] {} CPUs: running unpinned", system.count_ones()),
    }
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
}

/// The CPUs of `wanted` that exist in `system`; when none do, the high half of `system`;
/// `None` (no pin) with 4 or fewer CPUs, where a pin would only crowd the game.
pub(crate) fn fit(wanted: usize, system: usize) -> Option<usize> {
    let cpus = system.count_ones();
    if cpus <= 4 {
        return None;
    }
    let fit = wanted & system;
    if fit != 0 {
        return Some(fit);
    }
    let (mut high, mut seen) = (0usize, 0u32);
    for bit in (0..usize::BITS).rev() {
        if system & (1 << bit) != 0 && seen < cpus / 2 {
            high |= 1 << bit;
            seen += 1;
        }
    }
    Some(high)
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn a_desktop_mask_is_kept_where_its_cpus_exist() {
        assert_eq!(fit(0xffff_0000, 0xffff_ffff), Some(0xffff_0000));
        assert_eq!(fit(0xffff_0000, 0x00ff_ffff), Some(0x00ff_0000));
    }

    #[test]
    fn a_mask_with_no_existing_cpu_takes_the_high_half() {
        assert_eq!(fit(0xffff_0000, 0xff), Some(0xf0));
        assert_eq!(fit(0xffff_0000, 0xfff), Some(0xfc0));
    }

    #[test]
    fn four_or_fewer_cpus_are_never_pinned() {
        assert_eq!(fit(0xffff_0000, 0xf), None);
        assert_eq!(fit(0x3, 0x3), None);
    }
}
