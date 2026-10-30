//! A minimal PNG writer for the offline exports: 8-bit RGBA (straight
//! alpha), per-row adaptive filters, zlib *stored* blocks (no compressor in
//! the overlay). The files are valid as written;
//! `scripts/dev/png-recompress.py` deflates them without touching a pixel.

/// `rgba` is `w`×`h` straight-alpha RGBA rows, top to bottom.
pub fn encode(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = w as usize * 4;
    assert_eq!(rgba.len(), stride * h as usize, "RGBA buffer size");
    let mut filtered = Vec::with_capacity((stride + 1) * h as usize);
    let mut prev = vec![0u8; stride];
    for row in rgba.chunks_exact(stride.max(1)).take(h as usize) {
        let (kind, line) = best_filter(row, &prev);
        filtered.push(kind);
        filtered.extend_from_slice(&line);
        prev.copy_from_slice(row);
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, deflate, adaptive filters, no interlace
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&filtered));
    chunk(&mut png, b"IEND", &[]);
    png
}

/// The filter (None, Sub, Up, Average, Paeth) with the smallest sum of
/// absolute residuals: the usual heuristic, so a later deflate packs well.
fn best_filter(row: &[u8], prev: &[u8]) -> (u8, Vec<u8>) {
    let left = |i: usize| if i >= 4 { row[i - 4] } else { 0 };
    let up_left = |i: usize| if i >= 4 { prev[i - 4] } else { 0 };
    let paeth = |a: u8, b: u8, c: u8| {
        let p = a as i16 + b as i16 - c as i16;
        let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
        if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
    };
    let predict = |kind: u8, i: usize| -> u8 {
        match kind {
            1 => left(i),
            2 => prev[i],
            3 => ((left(i) as u16 + prev[i] as u16) / 2) as u8,
            4 => paeth(left(i), prev[i], up_left(i)),
            _ => 0,
        }
    };
    (0..5u8)
        .map(|kind| (kind, (0..row.len()).map(|i| row[i].wrapping_sub(predict(kind, i))).collect::<Vec<u8>>()))
        .min_by_key(|(_, line)| line.iter().map(|&v| (v as i8).unsigned_abs() as u32).sum::<u32>())
        .expect("five filters")
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if data.is_empty() { vec![&[]] } else { data.chunks(65_535).collect() };
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in bytes.chunks(5_552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_the_reference_values() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082, "the IEND chunk's well-known CRC");
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn a_png_has_the_signature_chunks_and_round_trips_its_rows() {
        let (w, h) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 % 251) as u8).collect();
        let png = encode(w, h, &rgba);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // Undo the stored zlib and the row filters: the pixels come back exact.
        let idat = &png[33 + 8..];
        let len = u16::from_le_bytes([idat[3], idat[4]]) as usize;
        let filtered = &idat[7..7 + len];
        let stride = (w * 4) as usize;
        let mut prev = vec![0u8; stride];
        let mut back = Vec::new();
        for line in filtered.chunks(stride + 1) {
            let mut row = vec![0u8; stride];
            for i in 0..stride {
                let a = if i >= 4 { row[i - 4] } else { 0 };
                let c = if i >= 4 { prev[i - 4] } else { 0 };
                let pred = match line[0] {
                    1 => a,
                    2 => prev[i],
                    3 => ((a as u16 + prev[i] as u16) / 2) as u8,
                    4 => {
                        let p = a as i16 + prev[i] as i16 - c as i16;
                        let (pa, pb, pc) = ((p - a as i16).abs(), (p - prev[i] as i16).abs(), (p - c as i16).abs());
                        if pa <= pb && pa <= pc { a } else if pb <= pc { prev[i] } else { c }
                    }
                    _ => 0,
                };
                row[i] = line[1 + i].wrapping_add(pred);
            }
            back.extend_from_slice(&row);
            prev = row;
        }
        assert_eq!(back, rgba);
    }
}
