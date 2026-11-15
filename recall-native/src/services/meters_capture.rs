//! One meter's capture: the stream's event handle, and the loop that waits on the stream,
//! stop, endpoint changes and the captured process together (`meters`).
use super::*;

/// wasapi's event handle, closed on drop: wasapi 0.19's `Handle` has no `Drop`, so every meter
/// reopen leaked one. Drop it after the client that signals it.
struct Event(wasapi::Handle);

// `Handle` is one `HANDLE` field (wasapi 0.19 api.rs): the same size means it sits at offset 0.
const _: () = assert!(std::mem::size_of::<wasapi::Handle>() == std::mem::size_of::<*mut std::ffi::c_void>());

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the size check above; the handle came from CreateEventA and is closed once.
        let raw: *mut std::ffi::c_void = unsafe { std::mem::transmute_copy(&self.0) };
        if !raw.is_null() {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(raw)) };
        }
    }
}

impl Event {
    fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        // SAFETY: the size check above; the handle stays open for the life of `self`.
        windows::Win32::Foundation::HANDLE(unsafe { std::mem::transmute_copy(&self.0) })
    }
}

/// How often a default-device meter checks that the default has not moved, when endpoint
/// notifications could not be registered.
const DEFAULT_CHECK: Duration = Duration::from_secs(1);

/// Streams levels at ~10 Hz until asked to stop, a captured process exits, or the Windows
/// default it meters moves; device loss surfaces as an error from the read or the padding probe.
pub(super) fn capture(shared: &Shared, index: usize, opened: Opened, waits: &Waits) -> Result<Ended, String> {
    let Opened { mut client, period, pid, default, communications } = opened;
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
    client
        .initialize_client(&format, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: period })
        .map_err(|error| error.to_string())?;
    let event = Event(client.set_get_eventhandle().map_err(|error| error.to_string())?);
    let ended = levels_until_end(shared, index, (&mut client, &event), pid, (default.as_ref(), communications), waits);
    drop(client);
    drop(event);
    ended
}

/// What woke the capture loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Woke {
    Stop,
    /// An endpoint changed: check the default, probe the device.
    Endpoints,
    Audio,
    Exited,
    /// Nothing within the wait: silence, or a backstop probe.
    Quiet,
}

/// Index of a signalled handle in `[stop?, endpoints?, audio, process?]` → what it means.
pub(super) fn woke(index: Option<usize>, stop: bool, endpoints: bool) -> Woke {
    let Some(mut index) = index else { return Woke::Quiet };
    for (present, what) in [(stop, Woke::Stop), (endpoints, Woke::Endpoints)] {
        if present {
            if index == 0 {
                return what;
            }
            index -= 1;
        }
    }
    if index == 0 { Woke::Audio } else { Woke::Exited }
}

/// How long the capture waits for its stream: the long backstop only once silence was seen
/// (`quiet`) and the floor is what the meter shows. A silence wake that published a leftover
/// level keeps the short wait one more round, so the bar drops to the floor in ~0.4 s.
pub(super) fn wait_limit(quiet: bool, shown_floor: bool) -> Duration {
    if quiet && shown_floor { QUIET_BACKSTOP } else { SILENCE }
}

/// The capture loop of [`capture`]: waits on the stream's event, stop, endpoint changes and the
/// captured process's exit together, instead of a 200 ms read loop with a 1 s default check.
fn levels_until_end(
    shared: &Shared,
    index: usize,
    (client, event): (&mut AudioClient, &Event),
    pid: Option<u32>,
    (default, communications): (Option<&(Direction, String)>, bool),
    waits: &Waits,
) -> Result<Ended, String> {
    let text = |error: wasapi::WasapiError| error.to_string();
    let process = pid.and_then(crate::services::process_tree::Waitable::open);
    let capture = client.get_audiocaptureclient().map_err(text)?;
    client.start_stream().map_err(text)?;
    let mut more = vec![event.raw()];
    more.extend(process.as_ref().map(crate::services::process_tree::Waitable::handle));
    let handles = waits.handles(Some(&more));
    let (has_stop, has_endpoints) = (waits.stop.is_some(), waits.endpoints.is_some());
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut levels = Levels::default();
    let mut last = Instant::now();
    let mut checked = Instant::now();
    let mut quiet = false;
    // The last level published was the floor: only then may the wait grow to the backstop.
    let mut shown_floor = false;
    while !shared.stopped() {
        // Without endpoint notifications the default is checked every second, as before.
        let check_due = !has_endpoints && checked.elapsed() >= DEFAULT_CHECK;
        let limit = wait_limit(quiet, shown_floor);
        let limit = if has_endpoints { limit } else { limit.min(DEFAULT_CHECK) };
        let what = if check_due { Woke::Endpoints } else { woke(wait_any(&handles, Some(limit)), has_stop, has_endpoints) };
        if what == Woke::Endpoints {
            checked = Instant::now();
            if let Some((direction, opened)) = default {
                let current = if communications {
                    crate::services::audio_devices::communications_audio::distinct_device().and_then(|device| device.get_id().ok())
                } else {
                    default_id(direction)
                };
                if (communications && current.is_none()) || default_moved(Some(opened), current.as_deref()) {
                    let _ = client.stop_stream();
                    return Ok(Ended::DefaultMoved);
                }
            }
        }
        match what {
            Woke::Stop | Woke::Exited => break,
            Woke::Audio => {
                quiet = false;
                capture.read_from_device_to_deque(&mut raw).map_err(|error| format!("read failed: {error}"))?;
                let full = raw.len() - raw.len() % 4;
                levels.push_bytes(&raw.make_contiguous()[..full]);
                raw.drain(..full);
            }
            Woke::Quiet | Woke::Endpoints => {
                quiet = what == Woke::Quiet;
                client.get_current_padding().map_err(|error| format!("device lost: {error}"))?;
                if process.as_ref().is_some_and(|p| p.exited()) {
                    break;
                }
            }
        }
        if last.elapsed() >= UPDATE {
            let (rms_db, peak_db) = levels.take();
            shown_floor = rms_db <= FLOOR_DB && peak_db <= FLOOR_DB;
            shared.set(index, rms_db, peak_db, MeterState::Live);
            last = Instant::now();
        }
    }
    let _ = client.stop_stream();
    Ok(Ended::Closed)
}
