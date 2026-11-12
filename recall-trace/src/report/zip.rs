//! A minimal ZIP writer: stored entries only (no compression), so a report's size is the sum
//! of what went in and it cannot expand on the receiving side. Every entry carries the fixed
//! DOS date 1980-01-01, so the archive holds no clock time of the user's.

use std::io::{self, Write};

/// One file to store: its name inside the archive and its bytes.
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

/// The CRC-32 (IEEE) of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = (1 << 5) | 1; // 1980-01-01
const UTF8_NAMES: u16 = 0x0800;

/// Write `entries` as a ZIP archive. Fails past 4 GiB (no ZIP64), which a report never nears.
pub fn write(out: &mut impl Write, entries: &[Entry]) -> io::Result<u64> {
    let mut offset = 0u64;
    let mut central = Vec::new();
    for e in entries {
        let name = e.name.as_bytes();
        let size = u32::try_from(e.data.len()).map_err(|_| io::Error::other("entry too large"))?;
        let crc = crc32(&e.data);
        let local_at = u32::try_from(offset).map_err(|_| io::Error::other("archive too large"))?;
        let mut head = Vec::with_capacity(30 + name.len());
        head.extend(0x0403_4B50u32.to_le_bytes());
        head.extend(20u16.to_le_bytes());
        head.extend(UTF8_NAMES.to_le_bytes());
        head.extend(0u16.to_le_bytes()); // stored
        head.extend(DOS_TIME.to_le_bytes());
        head.extend(DOS_DATE.to_le_bytes());
        head.extend(crc.to_le_bytes());
        head.extend(size.to_le_bytes());
        head.extend(size.to_le_bytes());
        head.extend((name.len() as u16).to_le_bytes());
        head.extend(0u16.to_le_bytes());
        head.extend(name);
        out.write_all(&head)?;
        out.write_all(&e.data)?;
        offset += head.len() as u64 + e.data.len() as u64;

        central.extend(0x0201_4B50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(UTF8_NAMES.to_le_bytes());
        central.extend(0u16.to_le_bytes());
        central.extend(DOS_TIME.to_le_bytes());
        central.extend(DOS_DATE.to_le_bytes());
        central.extend(crc.to_le_bytes());
        central.extend(size.to_le_bytes());
        central.extend(size.to_le_bytes());
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0u8; 12]); // extra, comment, disk, internal and external attributes
        central.extend(local_at.to_le_bytes());
        central.extend(name);
    }
    let cd_at = u32::try_from(offset).map_err(|_| io::Error::other("archive too large"))?;
    out.write_all(&central)?;
    let mut end = Vec::with_capacity(22);
    end.extend(0x0605_4B50u32.to_le_bytes());
    end.extend([0u8; 4]);
    end.extend((entries.len() as u16).to_le_bytes());
    end.extend((entries.len() as u16).to_le_bytes());
    end.extend((central.len() as u32).to_le_bytes());
    end.extend(cd_at.to_le_bytes());
    end.extend(0u16.to_le_bytes());
    out.write_all(&end)?;
    Ok(offset + central.len() as u64 + 22)
}
