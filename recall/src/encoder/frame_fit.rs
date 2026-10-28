//! Small capture-size jitter normalization (crop/pad, never scale), and the
//! RGBA -> RGB0 frame fill NVENC and the thumbnail scaler read from.

use ffmpeg_next as ff;

use super::PacedFrame;

/// Copy tightly packed RGBA into an RGBZ video frame, honoring the stride.
pub(super) fn fill_rgbz(vf: &mut ff::frame::Video, w: u32, h: u32, rgba: &[u8]) {
    let stride = vf.stride(0);
    let src_row = (w * 4) as usize;
    let dst = vf.data_mut(0);
    for y in 0..h as usize {
        let n = src_row.min(rgba.len() - y * src_row);
        dst[y * stride..y * stride + n].copy_from_slice(&rgba[y * src_row..y * src_row + n]);
    }
}

/// Keep an initialized encoder stable across the few pixels of client-border
/// jitter that WGC can report while a window gains or loses focus. This is
/// intentionally a crop/pad, never a scale: larger geometry changes still
/// reopen NVENC in the encoder loop.
pub(super) fn normalize_small_size_jitter(
    frame: &mut PacedFrame,
    target_width: u32,
    target_height: u32,
) -> Result<bool, String> {
    const MAX_JITTER_PIXELS: u32 = 8;

    if frame.width == target_width && frame.height == target_height {
        return Ok(false);
    }
    if frame.width.abs_diff(target_width) > MAX_JITTER_PIXELS
        || frame.height.abs_diff(target_height) > MAX_JITTER_PIXELS
    {
        return Ok(false);
    }

    let rgba_len = |width: u32, height: u32| {
        usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
            .and_then(|pixels| pixels.checked_mul(4))
    };
    let source_len = rgba_len(frame.width, frame.height)
        .ok_or("capture size overflows RGBA buffer length")?;
    let target_len = rgba_len(target_width, target_height)
        .ok_or("encoder size overflows RGBA buffer length")?;
    if source_len == 0 || target_len == 0 {
        return Err("cannot normalize a zero-sized capture".into());
    }
    if frame.rgba.len() != source_len {
        return Err(format!(
            "capture RGBA length {} does not match {}x{}",
            frame.rgba.len(),
            frame.width,
            frame.height
        ));
    }

    let source_row = usize::try_from(frame.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or("capture row overflows RGBA buffer length")?;
    let target_row = usize::try_from(target_width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or("encoder row overflows RGBA buffer length")?;
    let rows = usize::try_from(frame.height.min(target_height))
        .map_err(|_| "capture height does not fit address space")?;
    let copy_row = source_row.min(target_row);

    // Grow before moves so every destination range exists. Do not shrink until
    // afterward: later source rows can still lie past the smaller target tail.
    if target_len > source_len {
        frame.rgba.resize(target_len, 0);
    }
    if target_row > source_row {
        for y in (0..rows).rev() {
            let source = y * source_row;
            let target = y * target_row;
            frame.rgba.copy_within(source..source + copy_row, target);
            frame.rgba[target + copy_row..target + target_row].fill(0);
        }
    } else {
        for y in 0..rows {
            let source = y * source_row;
            let target = y * target_row;
            frame.rgba.copy_within(source..source + copy_row, target);
            frame.rgba[target + copy_row..target + target_row].fill(0);
        }
    }
    let target_rows = usize::try_from(target_height)
        .map_err(|_| "encoder height does not fit address space")?;
    for y in rows..target_rows {
        let target = y * target_row;
        frame.rgba[target..target + target_row].fill(0);
    }
    if target_len < source_len {
        frame.rgba.truncate(target_len);
    }
    frame.width = target_width;
    frame.height = target_height;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::PacedFrame;
    use super::normalize_small_size_jitter;

    fn patterned_frame(width: u32, height: u32, idx: u64) -> PacedFrame {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[x as u8, y as u8, 0xaa, 0xff]);
            }
        }
        PacedFrame {
            idx,
            width,
            height,
            rgba,
            gpu: None,
        }
    }

    fn pixel(frame: &PacedFrame, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * frame.width + x) * 4) as usize;
        frame.rgba[start..start + 4].try_into().unwrap()
    }

    #[test]
    fn jitter_padding_preserves_pixels_and_pts() {
        let mut frame = patterned_frame(4, 3, 42_123);

        assert_eq!(normalize_small_size_jitter(&mut frame, 6, 4).unwrap(), true);
        assert_eq!((frame.width, frame.height, frame.idx), (6, 4, 42_123));
        assert_eq!(frame.rgba.len(), 6 * 4 * 4);
        assert_eq!(pixel(&frame, 3, 2), [3, 2, 0xaa, 0xff]);
        assert_eq!(pixel(&frame, 4, 2), [0, 0, 0, 0]);
        assert_eq!(pixel(&frame, 0, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn jitter_cropping_keeps_top_left_pixel_mapping() {
        let mut frame = patterned_frame(6, 5, 7);

        assert_eq!(normalize_small_size_jitter(&mut frame, 4, 3).unwrap(), true);
        assert_eq!((frame.width, frame.height, frame.idx), (4, 3, 7));
        assert_eq!(frame.rgba.len(), 4 * 3 * 4);
        assert_eq!(pixel(&frame, 0, 0), [0, 0, 0xaa, 0xff]);
        assert_eq!(pixel(&frame, 3, 2), [3, 2, 0xaa, 0xff]);
    }

    #[test]
    fn large_change_is_left_for_encoder_reconfiguration() {
        let mut frame = patterned_frame(4, 3, 9);
        let before = frame.rgba.clone();

        assert_eq!(normalize_small_size_jitter(&mut frame, 13, 3).unwrap(), false);
        assert_eq!((frame.width, frame.height, frame.idx), (4, 3, 9));
        assert_eq!(frame.rgba, before);
    }
}
