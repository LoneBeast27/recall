//! Measured smoke against the real library. Read-only on the library drive: every write goes
//! to a scratch cache (`RECALL_SMOKE_CACHE`, else `<crate>/../.tmp/storage-smoke-<time>`).
//! Run alone: `cargo test storage_smoke -- --ignored --nocapture`
//! (`RECALL_SMOKE_COPY=1` also times one sequential video copy of a clip ≤ 600 MB;
//! `RECALL_SMOKE_OFFSET=N` measures clips N..N+200 instead of the newest, e.g. a window whose
//! thumbnails are not in the OS file cache from an earlier run).
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;
use crate::backend;

const NEWEST: usize = 200;

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

#[test]
#[ignore = "reads the real clip library; run explicitly"]
fn storage_smoke_listing_and_thumbs_cold_vs_warm() {
    let library = backend::library_dir();
    if !library.is_dir() {
        eprintln!("library {} missing; skipping", library.display());
        return;
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let root = std::env::var_os("RECALL_SMOKE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join(format!("storage-smoke-{nonce}")));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).map(|p| plain_path(&p)).unwrap();
    unsafe { std::env::set_var("RECALL_CACHE_DIR", &root) };
    assert_eq!(cache_root(), root, "run this test alone so the cache root is the scratch folder");
    println!("library drive: {:?}", drive_info(&library));
    println!("cache drive:   {:?}", drive_info(&root));

    // What the previous list_clips did on every launch: list the recorder thumbs folder and
    // canonicalize every clip (one open per clip) to look for cached thumbnails.
    let t = Instant::now();
    let recorder_thumbs = std::fs::read_dir(library.join("thumbs")).map(|l| l.count()).unwrap_or(0);
    let old_listing = t.elapsed();

    // ---- cold: empty cache --------------------------------------------------------------
    let start = stats();
    let t = Instant::now();
    let cold = backend::list_clips().unwrap();
    let list_cold = t.elapsed();
    let offset: usize = std::env::var("RECALL_SMOKE_OFFSET").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let newest: Vec<_> = cold.iter().skip(offset).take(NEWEST).cloned().collect();
    println!("window: clips {offset}..{}", offset + newest.len());
    let cold_hits = newest.iter().filter(|e| e.thumb.is_some()).count();
    let t = Instant::now();
    let canonicalized = cold.iter().filter(|e| std::fs::canonicalize(&e.path).is_ok()).count();
    let old_canonicalize = t.elapsed();

    let t = Instant::now();
    let (tx, rx) = mpsc::channel();
    for entry in &newest {
        let (tx, path) = (tx.clone(), entry.path.clone());
        submit_io(Path::new(&entry.path), Priority::Visible, move || {
            let started = Instant::now();
            let result = backend::thumbnail(&path);
            let _ = tx.send((result, started.elapsed()));
        });
    }
    drop(tx);
    let results: Vec<_> = rx.iter().collect();
    let thumbs_cold = t.elapsed();
    let ok = results.iter().filter(|(r, _)| r.is_ok()).count();
    let slowest = results.iter().map(|(_, d)| *d).max().unwrap_or_default();
    let after_cold = stats();
    for (result, _) in results.iter().filter(|(r, _)| r.is_err()).take(3) {
        println!("thumb error: {result:?}");
    }

    // ---- warm: second launch ------------------------------------------------------------
    let t = Instant::now();
    let warm = backend::list_clips().unwrap();
    let list_warm = t.elapsed();
    let warm_hits = warm.iter().skip(offset).take(NEWEST).filter(|e| e.thumb.as_deref().is_some_and(|p| Path::new(p).starts_with(&root))).count();
    let after_warm = stats();

    println!("clips listed: {} (recorder thumbs folder: {recorder_thumbs} files)", cold.len());
    println!("previous design, per launch: thumbs-folder listing {}, canonicalize {} clips {}", ms(old_listing), canonicalized, ms(old_canonicalize));
    println!("COLD list_clips {} | SSD thumb hits {cold_hits}/{NEWEST}", ms(list_cold));
    println!(
        "COLD thumbs for newest {NEWEST}: {} total, {ok} ok, slowest single {} | lane tasks {} | bytes read from HDD by this process {}",
        ms(thumbs_cold),
        ms(slowest),
        after_cold.hdd_tasks - start.hdd_tasks,
        after_cold.hdd_bytes_read - start.hdd_bytes_read
    );
    println!(
        "WARM list_clips {} | SSD thumb hits {warm_hits}/{NEWEST} | lane tasks {} | HDD bytes read {}",
        ms(list_warm),
        after_warm.hdd_tasks - after_cold.hdd_tasks,
        after_warm.hdd_bytes_read - after_cold.hdd_bytes_read
    );
    assert_eq!(cold_hits, 0, "scratch cache starts empty");
    assert_eq!(warm_hits, ok, "every thumbnail made cold is found warm");
    assert_eq!(after_warm.hdd_tasks, after_cold.hdd_tasks, "warm listing queues no disk-lane work");
    assert_eq!(after_warm.hdd_bytes_read, after_cold.hdd_bytes_read, "warm listing reads no thumbnail bytes from the HDD");

    if std::env::var_os("RECALL_SMOKE_COPY").is_some() {
        smoke_copy(&cold, &root);
    }
}

fn smoke_copy(clips: &[backend::ClipEntry], root: &Path) {
    let Some(clip) = clips.iter().take(50).filter(|c| c.bytes <= 600_000_000).max_by_key(|c| c.bytes) else { return };
    let before = stats();
    let t = Instant::now();
    let source = PathBuf::from(&clip.path);
    let copy = lane::run_io(&source.clone(), Priority::Interactive, {
        let root = root.to_path_buf();
        move || video::cache_in(&root, &source, 1 << 30)
    })
    .unwrap()
    .unwrap();
    let elapsed = t.elapsed();
    let read = stats().hdd_bytes_read - before.hdd_bytes_read;
    println!(
        "COPY {} ({:.0} MB) in {} = {:.0} MB/s, read {} bytes unbuffered",
        clip.name,
        clip.bytes as f64 / 1e6,
        ms(elapsed),
        clip.bytes as f64 / 1e6 / elapsed.as_secs_f64(),
        read
    );
    assert_eq!(copy.metadata().unwrap().len(), clip.bytes);
    let _ = std::fs::remove_file(copy);
}

/// Smart planner end to end on the real library (read-only on it): the newest
/// `RECALL_SMOKE_CLIPS` (300) clips, budget `RECALL_SMOKE_BUDGET_MB` (64). Waits until the fill
/// goes quiet, checks it landed near 95% of the budget, then halves the budget.
#[test]
#[ignore = "reads the real clip library; run explicitly and alone"]
fn storage_smoke_smart_fill() {
    use std::sync::{Arc, Mutex};
    let library = backend::library_dir();
    if !library.is_dir() {
        return;
    }
    let env_num = |key: &str, default: u64| std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join(format!("storage-smart-{nonce}"));
    std::fs::create_dir_all(&root).unwrap();
    let root = plain_path(&std::fs::canonicalize(&root).unwrap());
    unsafe { std::env::set_var("RECALL_CACHE_DIR", &root) };
    assert_eq!(cache_root(), root, "run this test alone");
    let budget = env_num("RECALL_SMOKE_BUDGET_MB", 64) << 20;
    evict::write_settings(&root, &evict::Settings { cache_budget_bytes: Some(budget), ..Default::default() }).unwrap();
    let entries: Vec<_> = backend::list_clips().unwrap().into_iter().take(env_num("RECALL_SMOKE_CLIPS", 300) as usize).collect();
    let store = crate::services::library::MetaStore::load();
    let records = crate::services::library::build_records(&entries, &store);
    let clips: Vec<PrefetchClip> = entries
        .iter()
        .zip(records)
        .map(|(e, r)| PrefetchClip {
            path: e.path.clone(),
            name: e.name.clone(),
            bytes: e.bytes,
            mtime_secs: e.mtime,
            has_thumb: e.thumb.is_some(),
            duration: r.duration,
            duration_unknown: false,
            game_key: r.game_key,
            favourite: r.favourite,
            edited: r.edited,
        })
        .collect();
    let events = Arc::new(Mutex::new([0usize; 4]));
    let counter = events.clone();
    let sink: prefetch::Sink = Arc::new(move |event| {
        let mut c = counter.lock().unwrap();
        match event {
            PrefetchEvent::Thumb(_, Ok(_)) => c[0] += 1,
            PrefetchEvent::Duration(_, Some(_)) => c[1] += 1,
            PrefetchEvent::Scrub(_, Ok(_)) => c[2] += 1,
            _ => c[3] += 1,
        }
    });
    let before = stats();
    let started = Instant::now();
    start_prefetch(clips, false, sink, backend::prefetch_editor_media);
    let (mut last, mut quiet) = ((0u64, 0u64), 0);
    while quiet < 5 && started.elapsed() < Duration::from_secs(600) {
        std::thread::sleep(Duration::from_secs(2));
        let now = (stats().idle_tasks, budget::inventory(&root, &|_| 0.0).total());
        quiet = if now == last { quiet + 1 } else { 0 };
        last = now;
    }
    let settled = started.elapsed().saturating_sub(Duration::from_secs(10));
    let usage = tiers::usage_in(&root);
    println!(
        "SMART fill of {} clips settled in ~{} | idle tasks {} | events thumb/duration/scrub/errors {:?}",
        entries.len(),
        ms(settled),
        stats().idle_tasks - before.idle_tasks,
        *events.lock().unwrap()
    );
    for tier in &usage.tiers {
        println!("  {:<20} {:>8.1} MB  {} items", tier.kind.label(), tier.bytes as f64 / 1e6, tier.items);
    }
    println!(
        "  total {:.1} MB of {:.1} MB budget ({:.0}%), video cap {:.1} MB | {}",
        usage.total() as f64 / 1e6,
        budget as f64 / 1e6,
        usage.total() as f64 * 100.0 / budget as f64,
        usage.video_cap as f64 / 1e6,
        smart_status()
    );
    assert!(usage.total() <= budget, "never over budget");
    assert!(usage.total() >= budget / 100 * 80, "fill should approach 95% of the budget");
    let video = usage.tiers.iter().find(|t| t.kind == CacheKind::Video).unwrap().bytes;
    assert!(video <= usage.video_cap);
    // Halve the budget: lowest-score items go, thumbnails stay.
    let thumbs_before = usage.tiers.iter().find(|t| t.kind == CacheKind::Thumbs).unwrap().bytes;
    evict::write_settings(&root, &evict::Settings { cache_budget_bytes: Some(budget / 2), ..Default::default() }).unwrap();
    let freed = enforce_budget();
    let after = tiers::usage_in(&root);
    println!("HALVED budget: freed {:.1} MB, total now {:.1} MB of {:.1} MB", freed as f64 / 1e6, after.total() as f64 / 1e6, (budget / 2) as f64 / 1e6);
    for tier in &after.tiers {
        println!("  {:<20} {:>8.1} MB  {} items", tier.kind.label(), tier.bytes as f64 / 1e6, tier.items);
    }
    assert!(after.total() <= budget / 2);
    assert_eq!(after.tiers.iter().find(|t| t.kind == CacheKind::Thumbs).unwrap().bytes, thumbs_before, "thumbnails kept");
    prefetch::stop();
}
