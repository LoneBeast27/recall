//! A silent AAC frame grown to an exact byte size with fill elements (`part_mute.rs`), so a
//! frame rewritten in place moves no offset in the file. An AAC raw data block is a list of
//! syntax elements ending with `ID_END` (0b111) and zero bits to the byte. A fill element
//! (`ID_FIL` 0b110, a 4-bit count, an 8-bit escape when the count is 15, then that many
//! bytes) carries nothing a decoder plays; zero bytes are an `EXT_FILL` payload.

const ID_FIL: u32 = 0b110;
const ID_END: u32 = 0b111;
/// The most bytes one fill element carries (count 15 + escape 255 - 1).
const FIL_MAX: usize = 269;

struct Bits {
    out: Vec<u8>,
    len: usize,
}

impl Bits {
    fn push(&mut self, value: u32, n: usize) {
        for i in (0..n).rev() {
            if self.len.is_multiple_of(8) {
                self.out.push(0);
            }
            if (value >> i) & 1 == 1 {
                let last = self.out.len() - 1;
                self.out[last] |= 0x80 >> (self.len % 8);
            }
            self.len += 1;
        }
    }
}

fn bit(data: &[u8], i: usize) -> u32 {
    u32::from((data[i / 8] >> (7 - i % 8)) & 1)
}

/// Bits a fill element of `count` bytes takes.
fn fill_cost(count: usize) -> usize {
    if count < 15 { 3 + 4 + 8 * count } else { 3 + 4 + 8 + 8 * count }
}

/// Fill elements whose bits add up to `room - 0..=7` (the rest is the final padding).
fn plan_fill(mut room: usize) -> Vec<usize> {
    let mut out = Vec::new();
    while room > 7 {
        let count = if room >= fill_cost(15) { ((room - 15) / 8).min(FIL_MAX) } else { ((room - 7) / 8).min(14) };
        out.push(count);
        room -= fill_cost(count);
    }
    out
}

/// `silent` (one AAC raw frame ending in `ID_END`) as exactly `size` bytes, or None when it
/// does not fit (a frame that small holds next to nothing) or does not end in `ID_END`.
pub fn pad_to(silent: &[u8], size: usize) -> Option<Vec<u8>> {
    let last_one = (0..silent.len() * 8).rev().find(|&i| bit(silent, i) == 1)?;
    let end_at = last_one.checked_sub(2)?;
    if (0..3).any(|k| bit(silent, end_at + k) != 1) {
        return None;
    }
    let room = (size * 8).checked_sub(end_at + 3)?;
    let mut bits = Bits { out: Vec::with_capacity(size), len: 0 };
    for i in 0..end_at {
        bits.push(bit(silent, i), 1);
    }
    for count in plan_fill(room) {
        bits.push(ID_FIL, 3);
        if count < 15 {
            bits.push(count as u32, 4);
        } else {
            bits.push(15, 4);
            bits.push((count - 14) as u32, 8);
        }
        bits.push(0, 8 * count);
    }
    bits.push(ID_END, 3);
    bits.out.resize(size, 0);
    (bits.out.len() == size && bits.len <= size * 8).then_some(bits.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_padded_to_every_size_from_its_own() {
        let silent = [0b0010_0001u8, 0b0000_0011, 0b1000_0000]; // 15 element bits, then ID_END
        for size in silent.len()..2_000 {
            let padded = pad_to(&silent, size).unwrap_or_else(|| panic!("size {size}"));
            assert_eq!(padded.len(), size);
            assert_eq!(&padded[..1], &silent[..1], "the elements are kept");
        }
        assert_eq!(pad_to(&silent, 2), None, "smaller than the frame");
    }
}
