//! A communications loopback is separate only while the two Windows playback roles differ.
use wasapi::{Device, Direction, Role};

pub fn distinct_device() -> Option<Device> {
    let direction = Direction::Render;
    let system = wasapi::get_default_device(&direction).ok()?;
    let communications = wasapi::get_default_device_for_role(&direction, &Role::Communications).ok()?;
    (system.get_id().ok()? != communications.get_id().ok()?).then_some(communications)
}
