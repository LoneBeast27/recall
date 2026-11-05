//! The engine's CPU pin (`RECALL_CPU_MASK`), fitted to the machine's CPU count so a mask
//! written for a 32-thread desktop still means "the background half" on a 4- or 8-core laptop.
//! On a hybrid CPU (Intel P-cores + E-cores) the background cores are the E-cores, whatever the
//! mask says: Windows reports them as the lowest `EfficiencyClass` of its CPU sets.

/// `RECALL_CPU_MASK`: pin to the mask's CPUs that exist (the high half of the machine's
/// CPUs when none do, no pin on 4 or fewer) and drop to BelowNormal. A pin Windows refuses
/// twice exits with code 2, as before: an unpinned engine could take the game's cores.
pub(crate) fn apply(mask: &str) {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask, SetPriorityClass, SetProcessAffinityMask, BELOW_NORMAL_PRIORITY_CLASS};
    let wanted = usize::from_str_radix(mask.trim().trim_start_matches("0x"), 16).unwrap_or(0);
    let (mut process, mut system) = (0usize, 0usize);
    let _ = unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) };
    match fit(wanted, system, efficient_cpus()) {
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
/// `None` (no pin) with 4 or fewer CPUs, where a pin would only crowd the game. On a hybrid
/// CPU, `efficient` (the E-cores, 0 elsewhere) wins when it holds at least 2 of `system`.
pub(crate) fn fit(wanted: usize, system: usize, efficient: usize) -> Option<usize> {
    let cpus = system.count_ones();
    if cpus <= 4 {
        return None;
    }
    let e_cores = efficient & system;
    if e_cores.count_ones() >= 2 && e_cores != system {
        return Some(e_cores);
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

/// The logical CPUs (group 0) in the lowest efficiency class, when the CPU has more than one
/// class (Intel hybrid: the E-cores); 0 on CPUs whose cores are all one class (AMD, older Intel).
pub(crate) fn efficient_cpus() -> usize {
    use windows::Win32::System::SystemInformation::{GetSystemCpuSetInformation, CpuSetInformation, SYSTEM_CPU_SET_INFORMATION};
    let mut length = 0u32;
    // SAFETY: a size query (null buffer), then a buffer of the size Windows asked for.
    let _ = unsafe { GetSystemCpuSetInformation(None, 0, &mut length, None, None) };
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<SYSTEM_CPU_SET_INFORMATION>();
    if length == 0 || !unsafe { GetSystemCpuSetInformation(Some(info), length, &mut length, None, None) }.as_bool() {
        return 0;
    }
    let mut sets = Vec::new(); // (logical CPU, efficiency class)
    let mut offset = 0usize;
    while offset + std::mem::size_of::<SYSTEM_CPU_SET_INFORMATION>() <= length as usize {
        // SAFETY: each entry starts at `offset` inside the filled buffer and states its own size.
        let entry = unsafe { &*buffer.as_ptr().cast::<u8>().add(offset).cast::<SYSTEM_CPU_SET_INFORMATION>() };
        if entry.Size == 0 {
            break;
        }
        if entry.Type == CpuSetInformation {
            // SAFETY: Type says the union holds a CpuSet.
            let set = unsafe { entry.Anonymous.CpuSet };
            if set.Group == 0 && (set.LogicalProcessorIndex as u32) < usize::BITS {
                sets.push((set.LogicalProcessorIndex, set.EfficiencyClass));
            }
        }
        offset += entry.Size as usize;
    }
    lowest_class(&sets)
}

/// Mask of the CPUs in the lowest class of `sets`, 0 when every CPU has the same class.
fn lowest_class(sets: &[(u8, u8)]) -> usize {
    let (Some(low), Some(high)) = (sets.iter().map(|s| s.1).min(), sets.iter().map(|s| s.1).max()) else { return 0 };
    if low == high {
        return 0;
    }
    sets.iter().filter(|s| s.1 == low).fold(0, |mask, s| mask | 1 << s.0)
}

#[cfg(test)]
mod tests {
    use super::{fit, lowest_class};

    #[test]
    fn a_hybrid_cpu_runs_on_its_e_cores() {
        // 6 P-cores with HT (logical 0-11) + 8 E-cores (12-19): class 1 = P, class 0 = E.
        let sets: Vec<(u8, u8)> = (0..20).map(|cpu| (cpu, if cpu < 12 { 1 } else { 0 })).collect();
        let e = lowest_class(&sets);
        assert_eq!(e, 0xff000);
        assert_eq!(fit(0xffff_0000, 0xfffff, e), Some(0xff000), "E-cores, not the mask's 4 CPUs");
        let one_class: Vec<(u8, u8)> = (0..32).map(|cpu| (cpu, 0)).collect();
        assert_eq!(lowest_class(&one_class), 0, "AMD: one class, the mask decides");
        assert_eq!(fit(0xffff_0000, 0xffff_ffff, 0), Some(0xffff_0000));
    }

    #[test]
    fn this_machine_reads_its_cpu_sets() {
        let e = super::efficient_cpus();
        let system = (1usize << std::thread::available_parallelism().unwrap().get().min(63)) - 1;
        assert!(e == 0 || (e & system != 0 && e != system), "efficient {e:#x}");
    }

    #[test]
    fn a_desktop_mask_is_kept_where_its_cpus_exist() {
        assert_eq!(fit(0xffff_0000, 0xffff_ffff, 0), Some(0xffff_0000));
        assert_eq!(fit(0xffff_0000, 0x00ff_ffff, 0), Some(0x00ff_0000));
    }

    #[test]
    fn a_mask_with_no_existing_cpu_takes_the_high_half() {
        assert_eq!(fit(0xffff_0000, 0xff, 0), Some(0xf0));
        assert_eq!(fit(0xffff_0000, 0xfff, 0), Some(0xfc0));
    }

    #[test]
    fn four_or_fewer_cpus_are_never_pinned() {
        assert_eq!(fit(0xffff_0000, 0xf, 0), None);
        assert_eq!(fit(0x3, 0x3, 0), None);
    }
}
