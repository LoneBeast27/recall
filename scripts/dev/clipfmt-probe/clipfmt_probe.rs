//! Clip-format drive probe (experimental): times the proposed hard-drive layout against plain
//! MP4 reads on a real clips drive, with the Windows file cache bypassed
//! (`FILE_FLAG_NO_BUFFERING`), so every read is a real disk read.
//!
//! Build:  rustc -O --edition 2021 clipfmt_probe.rs
//! Lay out: clipfmt_probe build <clips folder> <test folder> [pieces]
//! Run:     clipfmt_probe run <clips folder> <test folder> [opens]
//!
//! `build` only reads the clips. It writes into <test folder>:
//! - `starts.bin`: one fixed-size entry per clip (a stand-in for description + byte map +
//!   first keyframe: the clip's first ENTRY bytes), reserved in full before writing;
//! - `starts.idx`: the clip paths in entry order;
//! - `pieces\`: unbroken copies of a few clips (reserved in full before writing).
//! `run` reads only. Delete the test folder when done.

use std::alloc::{alloc, dealloc, Layout};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

const SECTOR: u64 = 4096;
/// One starts entry: 600 KiB (description, map, decoder setup and the start keyframe).
const ENTRY: u64 = 150 * SECTOR;
/// One second of 25 Mbit/s video with its audio.
const CHUNK: u64 = 768 * SECTOR; // 3 MiB
const NO_BUFFERING: u32 = 0x2000_0000;

/// A sector-aligned buffer, as unbuffered reads need.
struct Aligned {
    ptr: *mut u8,
    layout: Layout,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, SECTOR as usize).expect("layout");
        let ptr = unsafe { alloc(layout) };
        assert!(!ptr.is_null(), "out of memory");
        Self { ptr, layout }
    }

    fn slice(&mut self, len: usize) -> &mut [u8] {
        assert!(len <= self.layout.size());
        unsafe { std::slice::from_raw_parts_mut(self.ptr, len) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

fn open_raw(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).share_mode(7).custom_flags(NO_BUFFERING).open(path)
}

/// Reads `len` bytes at `at` (both sector multiples); short reads at the file end are fine.
fn read_at(file: &mut File, at: u64, buf: &mut Aligned, len: u64) -> std::io::Result<usize> {
    file.seek(SeekFrom::Start(at))?;
    let dst = buf.slice(len as usize);
    let mut got = 0;
    while got < dst.len() {
        match file.read(&mut dst[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

fn floor(v: u64) -> u64 {
    v / SECTOR * SECTOR
}

/// xorshift: the same clips and offsets on every run with one seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn clips(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out: Vec<(PathBuf, u64)> = fs::read_dir(dir)
        .expect("clips folder")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.len())))
        .filter(|(_, len)| *len >= ENTRY + 2 * CHUNK)
        .collect();
    out.sort();
    out
}

fn build(clips_dir: &Path, test_dir: &Path, pieces: usize) {
    let all = clips(clips_dir);
    fs::create_dir_all(test_dir.join("pieces")).expect("test folder");
    let started = Instant::now();
    let mut starts = File::create(test_dir.join("starts.bin")).expect("starts.bin");
    starts.set_len(all.len() as u64 * ENTRY).expect("reserve starts.bin");
    let mut idx = String::new();
    let mut entry = vec![0u8; ENTRY as usize];
    for (i, (path, _)) in all.iter().enumerate() {
        let mut f = File::open(path).expect("clip");
        f.read_exact(&mut entry).expect("clip head");
        starts.write_all(&entry).expect("append entry");
        idx.push_str(&path.to_string_lossy());
        idx.push('\n');
        if i % 500 == 0 {
            eprintln!("starts: {i}/{}", all.len());
        }
    }
    starts.sync_all().ok();
    fs::write(test_dir.join("starts.idx"), idx).expect("starts.idx");
    println!("starts.bin: {} entries, {:.2} GiB, {:.0} s", all.len(), (all.len() as u64 * ENTRY) as f64 / (1u64 << 30) as f64, started.elapsed().as_secs_f64());

    // Unbroken copies of a few mid-size clips.
    let mut rng = Rng(0x5EED_C11F);
    let mid: Vec<&(PathBuf, u64)> = all.iter().filter(|(_, len)| (200 << 20..=900 << 20).contains(len)).collect();
    let mut list = String::new();
    let mut buf = vec![0u8; 8 << 20];
    for _ in 0..pieces.min(mid.len()) {
        let (src, len) = mid[rng.below(mid.len() as u64) as usize];
        let dst = test_dir.join("pieces").join(src.file_name().unwrap());
        if dst.exists() {
            continue;
        }
        let mut out = File::create(&dst).expect("piece");
        out.set_len(*len).expect("reserve piece");
        let mut input = File::open(src).expect("clip");
        loop {
            let n = input.read(&mut buf).expect("read clip");
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).expect("write piece");
        }
        out.sync_all().ok();
        list.push_str(&format!("{}\t{}\n", src.to_string_lossy(), dst.to_string_lossy()));
    }
    fs::write(test_dir.join("pieces.idx"), list).expect("pieces.idx");
    println!("pieces written, {:.0} s total", started.elapsed().as_secs_f64());
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn report(name: &str, mut v: Vec<f64>) {
    if v.is_empty() {
        return println!("{name}: no samples");
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    println!("{name:<58} n={:<3} median {:>7.1} ms   p90 {:>7.1}   max {:>7.1}", v.len(), at(0.5), at(0.9), at(1.0));
}

fn run(clips_dir: &Path, test_dir: &Path, opens: usize) {
    let idx = fs::read_to_string(test_dir.join("starts.idx")).expect("starts.idx (run build first)");
    let entries: Vec<&str> = idx.lines().collect();
    let _ = clips_dir;
    let mut rng = Rng(0xC0FF_EE00 ^ std::process::id() as u64);
    let mut buf = Aligned::new((16 * CHUNK) as usize);
    // The starts file is opened once, as the app would at start.
    let mut starts = open_raw(&test_dir.join("starts.bin")).expect("starts.bin");
    // PROBE_ENTRY_KIB: read only this much of each entry (a smaller first picture).
    let entry_read = std::env::var("PROBE_ENTRY_KIB").ok().and_then(|v| v.parse::<u64>().ok()).map_or(ENTRY, |k| floor(k * 1024).clamp(SECTOR, ENTRY));
    println!("entry read: {} KiB", entry_read / 1024);

    let (mut new_first, mut new_second, mut plain_first, mut plain_open, mut next5) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..opens {
        let i = rng.below(entries.len() as u64);
        let path = Path::new(entries[i as usize]);
        let Ok(len) = fs::metadata(path).map(|m| m.len()) else { continue };
        // The snippet the user wants: a random second of the clip (a trim point).
        if len < ENTRY + 8 * CHUNK {
            continue;
        }
        let chunk_at = floor(ENTRY + rng.below(len - ENTRY - 7 * CHUNK));

        // New layout: one read of the starts entry = first picture.
        let t = Instant::now();
        read_at(&mut starts, i * ENTRY, &mut buf, entry_read).expect("starts entry");
        new_first.push(ms(t));
        // ... then the clip's own file is opened and the full first second read.
        let Ok(mut clip) = open_raw(path) else { continue };
        read_at(&mut clip, chunk_at, &mut buf, CHUNK).expect("chunk");
        new_second.push(ms(t));
        // ... then the next 5 seconds straight on.
        let t5 = Instant::now();
        read_at(&mut clip, chunk_at + CHUNK, &mut buf, 5 * CHUNK).expect("next 5 s");
        next5.push(ms(t5));
        drop(clip);

        // Plain MP4 read from the drive, on another clip (so nothing is warm): open, head,
        // the index at the end, then the wanted second.
        let j = rng.below(entries.len() as u64);
        let other = Path::new(entries[j as usize]);
        let Ok(olen) = fs::metadata(other).map(|m| m.len()) else { continue };
        if olen < ENTRY + 8 * CHUNK {
            continue;
        }
        let t = Instant::now();
        let Ok(mut clip) = open_raw(other) else { continue };
        read_at(&mut clip, 0, &mut buf, 16 * SECTOR).expect("head");
        plain_open.push(ms(t));
        read_at(&mut clip, floor(olen) - 16 * SECTOR, &mut buf, 16 * SECTOR).expect("tail");
        let at = floor(ENTRY + rng.below(olen - ENTRY - 2 * CHUNK));
        read_at(&mut clip, at, &mut buf, CHUNK).expect("chunk");
        plain_first.push(ms(t));
    }
    println!("== opening a clip, cache bypassed ==");
    report("NEW: starts entry -> first picture's data", new_first);
    report("NEW: ... plus open clip + full first second", new_second);
    report("NEW: next 5 s after that (15 MiB straight on)", next5);
    report("PLAIN MP4: open + first 64 KiB", plain_open);
    report("PLAIN MP4: open + head + index at end + first second", plain_first);

    // Unbroken pieces against their fragmented originals: random seconds, and a whole-clip load.
    let pieces = fs::read_to_string(test_dir.join("pieces.idx")).unwrap_or_default();
    let (mut frag, mut solid, mut frag_rate, mut solid_rate) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for line in pieces.lines() {
        let Some((src, dst)) = line.split_once('\t') else { continue };
        let (Ok(mut a), Ok(mut b)) = (open_raw(Path::new(src)), open_raw(Path::new(dst))) else { continue };
        let len = fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
        for _ in 0..12 {
            let at = floor(rng.below(len - 2 * CHUNK));
            let t = Instant::now();
            read_at(&mut a, at, &mut buf, CHUNK).expect("fragmented chunk");
            frag.push(ms(t));
            let t = Instant::now();
            read_at(&mut b, at, &mut buf, CHUNK).expect("unbroken chunk");
            solid.push(ms(t));
        }
        for (file, rates) in [(&mut a, &mut frag_rate), (&mut b, &mut solid_rate)] {
            let t = Instant::now();
            let mut at = 0;
            while at + 16 * CHUNK <= len.min(256 << 20) {
                read_at(file, at, &mut buf, 16 * CHUNK).expect("load");
                at += 16 * CHUNK;
            }
            rates.push(at as f64 / (1 << 20) as f64 / t.elapsed().as_secs_f64());
        }
    }
    println!("== one second of video at a random spot ==");
    report("clip as saved today (fragmented)", frag);
    report("unbroken copy (space reserved first)", solid);
    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    println!("== loading 256 MiB of a clip ==\nas saved today: {:.0} MiB/s   unbroken copy: {:.0} MiB/s   (n={})", avg(&frag_rate), avg(&solid_rate), solid_rate.len());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let num = |i: usize, default: usize| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(default);
    match args.get(1).map(String::as_str) {
        Some("build") if args.len() >= 4 => build(Path::new(&args[2]), Path::new(&args[3]), num(4, 8)),
        Some("run") if args.len() >= 4 => run(Path::new(&args[2]), Path::new(&args[3]), num(4, 60)),
        _ => eprintln!("usage: clipfmt_probe build|run <clips folder> <test folder> [count]"),
    }
}
