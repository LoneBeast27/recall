use super::*;

const HZ: i64 = 1_000_000;

/// A DXGI `Present_Start` payload: swap-chain pointer, Flags, SyncInterval.
fn dxgi_payload(pointer_size: usize, flags: u32) -> Vec<u8> {
    let mut bytes = vec![0xAB; pointer_size];
    bytes.extend(flags.to_le_bytes());
    bytes.extend(1i32.to_le_bytes());
    bytes
}

fn event(provider: GUID, id: u16, payload: &[u8]) -> RawEvent<'_> {
    RawEvent { provider, id, pointer_size: 8, payload }
}

#[test]
fn dxgi_d3d9_and_kernel_presents_are_recognised() {
    let plain = dxgi_payload(8, 0);
    assert_eq!(classify(&event(DXGI, DXGI_PRESENT_START, &plain)), Some(Source::Runtime));
    assert_eq!(classify(&event(DXGI, DXGI_PRESENT_MPO_START, &plain)), Some(Source::Runtime));
    assert_eq!(classify(&event(D3D9, D3D9_PRESENT_START, &[0; 12])), Some(Source::Runtime));
    assert_eq!(classify(&event(DXGKRNL, DXGKRNL_PRESENT_INFO, &[])), Some(Source::Kernel));
}

#[test]
fn stops_other_ids_and_other_providers_are_not_presents() {
    let plain = dxgi_payload(8, 0);
    assert_eq!(classify(&event(DXGI, 0x002b, &plain)), None, "Present_Stop");
    assert_eq!(classify(&event(D3D9, 0x0002, &plain)), None, "D3D9 Present_Stop");
    assert_eq!(classify(&event(DXGKRNL, 0x00a8, &[])), None, "Flip_Info");
    assert_eq!(classify(&event(GUID::zeroed(), DXGI_PRESENT_START, &plain)), None);
    assert_eq!(classify(&event(D3D9, DXGI_PRESENT_START, &plain)), None, "ids are per provider");
}

#[test]
fn dxgi_present_test_is_an_occlusion_check_not_a_frame() {
    let test = dxgi_payload(8, 0x1 | 0x8);
    assert_eq!(classify(&event(DXGI, DXGI_PRESENT_START, &test)), None);
    let restart = dxgi_payload(8, 0x4); // DXGI_PRESENT_RESTART is a real frame
    assert_eq!(classify(&event(DXGI, DXGI_PRESENT_START, &restart)), Some(Source::Runtime));
    // A 32-bit game: the pointer is 4 bytes, so Flags sits at offset 4.
    let test32 = dxgi_payload(4, 0x1);
    let ev = RawEvent { provider: DXGI, id: DXGI_PRESENT_START, pointer_size: 4, payload: &test32 };
    assert_eq!(classify(&ev), None);
    // A short payload cannot say TEST: counted.
    assert_eq!(classify(&event(DXGI, DXGI_PRESENT_START, &[0; 3])), Some(Source::Runtime));
}

#[test]
fn the_provider_table_matches_presentmon() {
    let dxgi = &PROVIDERS[0];
    assert_eq!(format!("{:?}", dxgi.guid), "CA11C036-0102-4A2D-A6AD-F03CFED5D3C9");
    assert_eq!((dxgi.event_ids, dxgi.keyword, dxgi.pid_filter), (&[42u16, 55][..], 0x8000_0000_0000_0002, true));
    assert_eq!(format!("{:?}", PROVIDERS[1].guid), "783ACA0A-790E-4D7F-8451-AA850511C6B9");
    assert_eq!(PROVIDERS[1].event_ids, &[1]);
    let kernel = &PROVIDERS[2];
    assert_eq!(format!("{:?}", kernel.guid), "802EC45A-1E99-4B83-9920-87C98277BA9D");
    assert_eq!((kernel.event_ids, kernel.keyword, kernel.pid_filter), (&[184u16][..], 0x0800_0001, false));
}

#[test]
fn only_the_target_process_counts() {
    let mut mapper = FrameMapper::new(42, HZ);
    assert_eq!(mapper.map(7, Source::Runtime, 100), Mapped::Ignore);
    assert_eq!(mapper.map(42, Source::Runtime, 100), Mapped::Frame(100));
    let mut nobody = FrameMapper::new(0, HZ);
    assert_eq!(nobody.map(0, Source::Kernel, 100), Mapped::Ignore, "target 0 = no target");
}

#[test]
fn a_d3d_game_counts_each_frame_once() {
    // Per frame: DXGI Present_Start, then the kernel's Present_Info.
    let mut mapper = FrameMapper::new(42, HZ);
    let mut frames = 0;
    for i in 0..100 {
        let t = i * 7_000;
        for (source, at) in [(Source::Runtime, t), (Source::Kernel, t + 300)] {
            if matches!(mapper.map(42, source, at), Mapped::Frame(_) | Mapped::Restart(_)) {
                frames += 1;
            }
        }
    }
    assert_eq!(frames, 100);
}

#[test]
fn vulkan_and_opengl_fall_back_to_kernel_presents() {
    let mut mapper = FrameMapper::new(9, HZ);
    let mapped: Vec<Mapped> = (0..3).map(|i| mapper.map(9, Source::Kernel, i * 16_000)).collect();
    assert_eq!(mapped, [Mapped::Frame(0), Mapped::Frame(16_000), Mapped::Frame(32_000)]);
}

#[test]
fn switching_sources_restarts_the_window() {
    let mut mapper = FrameMapper::new(5, HZ);
    // A kernel present delivered before the game's first DXGI one.
    assert_eq!(mapper.map(5, Source::Kernel, 1_000), Mapped::Frame(1_000));
    assert_eq!(mapper.map(5, Source::Runtime, 900), Mapped::Restart(900), "runtime takes over");
    assert_eq!(mapper.map(5, Source::Kernel, 1_200), Mapped::Ignore, "paired with a runtime present");
    assert_eq!(mapper.map(5, Source::Runtime, 8_000), Mapped::Frame(8_000));
    // The runtime goes quiet for longer than the 2 s grace: kernel presents count again.
    assert_eq!(mapper.map(5, Source::Kernel, 8_000 + 2 * HZ), Mapped::Ignore, "grace is inclusive");
    assert_eq!(mapper.map(5, Source::Kernel, 8_001 + 2 * HZ), Mapped::Restart(8_001 + 2 * HZ));
    assert_eq!(mapper.map(5, Source::Kernel, 9_000 + 2 * HZ), Mapped::Frame(9_000 + 2 * HZ));
}

#[test]
fn a_late_runtime_event_never_moves_the_grace_backwards() {
    let mut mapper = FrameMapper::new(5, HZ);
    mapper.map(5, Source::Runtime, 5 * HZ);
    mapper.map(5, Source::Runtime, HZ); // out of order
    assert_eq!(mapper.map(5, Source::Kernel, 6 * HZ), Mapped::Ignore, "grace runs from the newest runtime present");
}
