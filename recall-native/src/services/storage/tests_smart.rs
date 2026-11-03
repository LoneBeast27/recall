//! Smart cache: score ordering on synthetic histories, score-based eviction under budget
//! pressure, the 95% fill gate, interaction persistence, pausing while the recorder works.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::evict::{Tier, Unit, plan as evict_plan};
use super::history::{History, Interaction};
use super::lane::{IdleSource, Lane, Priority, Task};
use super::prefetch::{Gate, Step, estimate, recorder_busy, set_recorder_busy};
use super::score::{ClipFacts, Weights, score_all};
use super::*;

const DAY: u64 = 86_400_000;
const NOW: u64 = 1_800_000_000_000;

fn facts(name: &str, game: &str, age_days: f64) -> ClipFacts {
    ClipFacts { name: name.into(), game: game.into(), mtime_ms: NOW - (age_days * DAY as f64) as u64, favourite: false, edited: false }
}

/// Clip path whose file name parses to `game` (recorder naming).
fn path(game: &str, n: u32) -> String {
    format!("V:\\ALL RECORDINGS\\{game}__2026-10-{:02}__10-00-00-000__abcdef0123456789abcdef01234567{n:02}.mp4", 1 + n % 28)
}

fn name_of(path: &str) -> String {
    std::path::Path::new(path).file_name().unwrap().to_string_lossy().to_lowercase()
}

fn rank(clips: &[ClipFacts], history: &History) -> Vec<String> {
    let scores = score_all(clips, history, NOW, &Weights::default());
    let mut order: Vec<usize> = (0..clips.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    order.into_iter().map(|i| clips[i].name.clone()).collect()
}

#[test]
fn newer_clips_rank_higher_when_nothing_else_differs() {
    let clips = [facts("old", "a", 30.0), facts("new", "a", 1.0), facts("mid", "a", 7.0)];
    assert_eq!(rank(&clips, &History::default()), ["new", "mid", "old"]);
}

#[test]
fn a_fresh_save_outranks_everything_else() {
    let mut clips = [facts("fresh", "a", 0.003), facts("yesterday", "a", 1.0)];
    clips[1].favourite = true;
    clips[1].edited = true;
    assert_eq!(rank(&clips, &History::default())[0], "fresh", "~4 minutes old");
}

#[test]
fn game_affinity_and_own_interactions_lift_older_clips() {
    let w = Weights::default();
    let mut history = History::default();
    let played = (0..6).map(|n| path("Rocket-League", n)).collect::<Vec<_>>();
    for (i, clip) in played.iter().enumerate() {
        history.apply(&Interaction::Opened(clip.clone()), NOW - (i as u64 + 1) * DAY, &w);
    }
    let rl = played[0].clone();
    let rl_name = name_of(&path("Rocket-League", 20));
    let other = facts("valorant-new", "valorant", 3.0);
    // An unopened Rocket League clip a few days older still beats a never-touched game.
    assert_eq!(rank(&[other.clone(), facts(&rl_name, "rocketleague", 5.0)], &history)[0], rl_name);
    // A clip opened several times beats a slightly newer, unopened clip of the same game.
    history.apply(&Interaction::Exported(rl.clone()), NOW - DAY, &w);
    let opened = facts(&name_of(&rl), "rocketleague", 10.0);
    let unopened = facts(&rl_name, "rocketleague", 8.0);
    assert_eq!(rank(&[unopened, opened], &history)[0], name_of(&rl));
    assert_eq!(history.favoured_games(NOW, &w, 2), ["Rocket League"]);
}

#[test]
fn browse_depth_and_favourites_shift_the_ranking() {
    let w = Weights::default();
    let shallow = History::default();
    let mut deep = History::default();
    deep.apply(&Interaction::ScrolledTo { days_back: 40 }, NOW - DAY, &w);
    let score = |history: &History, clip: &ClipFacts| score_all(std::slice::from_ref(clip), history, NOW, &w)[0];
    let month_old = facts("month", "a", 30.0);
    assert!(score(&deep, &month_old) > score(&shallow, &month_old) + 0.3, "a user who scrolls back a month keeps month-old clips warm");
    let mut favourite = facts("fav", "a", 20.0);
    favourite.favourite = true;
    assert_eq!(rank(&[facts("plain", "a", 15.0), favourite], &shallow)[0], "fav");
}

#[test]
fn a_clips_interest_halves_every_week_and_its_game_every_two() {
    let w = Weights::default();
    assert_eq!(super::score::CLIP_HALF_LIFE_DAYS, 7.0);
    let mut history = History::default();
    history.apply(&Interaction::Opened(path("Game", 1)), NOW - 14 * DAY, &w);
    let memory = &history.clips[&name_of(&path("Game", 1))];
    let now_value = memory.interest.value_at(NOW, w.clip_half_life_ms());
    assert!((now_value - w.open_event / 4.0).abs() < 1e-9, "two weeks = two clip half-lives: {now_value}");
    let game = history.games.values().next().unwrap().value_at(NOW, w.memory_half_life_ms());
    assert!((game - w.open_event / 2.0).abs() < 1e-9, "game affinity fades slower: {game}");
    history.prune(NOW + 400 * DAY, &w);
    assert!(history.clips.is_empty() && history.games.is_empty(), "decayed memories are dropped");
}

#[test]
fn an_open_outweighs_a_play_which_outweighs_hovers() {
    let w = Weights::default();
    let mut history = History::default();
    let (opened, played, hovered) = (path("Game", 1), path("Game", 2), path("Game", 3));
    history.apply(&Interaction::Opened(opened.clone()), NOW, &w);
    history.apply(&Interaction::Played(played.clone()), NOW, &w);
    for _ in 0..5 {
        history.apply(&Interaction::Hovered(hovered.clone()), NOW, &w);
    }
    let value = |p: &str| history.clips[&name_of(p)].interest.value;
    assert_eq!((value(&opened), value(&played)), (1.0, 0.5));
    assert!((value(&hovered) - 0.25).abs() < 1e-9, "five hovers are a quarter of an open: {}", value(&hovered));
}

#[test]
fn interactions_round_trip_through_the_history_file() {
    let w = Weights::default();
    let mut history = History::default();
    let clip = path("VALORANT", 3);
    for event in [
        Interaction::Opened(clip.clone()),
        Interaction::Hovered(clip.clone()),
        Interaction::Exported(clip.clone()),
        Interaction::Favourited(clip.clone()),
        Interaction::Saved(path("VALORANT", 4)),
        Interaction::ScrolledTo { days_back: 12 },
    ] {
        history.apply(&event, NOW, &w);
    }
    let memory = &history.clips[&name_of(&clip)];
    assert!(memory.favourite);
    assert!((memory.interest.value - (w.open_event + w.hover_event + w.export_event + w.favourite_event)).abs() < 1e-9);
    assert_eq!(history.clips[&name_of(&path("VALORANT", 4))].saved_at_ms, Some(NOW));
    assert_eq!(history.clips[&name_of(&path("VALORANT", 4))].interest.value, w.open_event, "a fresh save starts as one open");
    assert_eq!(history.browse_days.value, 12.0);
    let file = std::env::temp_dir().join(format!("recall-history-{}-{}.json", std::process::id(), NOW));
    history.save(&file).unwrap();
    assert_eq!(History::load(&file), history);
    assert!(std::fs::metadata(&file).unwrap().len() < 2048, "small file");
    let _ = std::fs::remove_file(file);
    assert_eq!(History::load(&PathBuf::from("Z:\\missing\\interactions.json")), History::default());
}

fn unit(tier: Tier, name: &str, mb: u64, score: f64) -> Unit {
    Unit { tier, name: name.into(), bytes: mb << 20, last_used: 0, score }
}

#[test]
fn eviction_drops_lowest_scores_first_and_thumbnails_last() {
    let units = [
        unit(Tier::Thumb, "thumbs/low.jpg", 1, 0.1),
        unit(Tier::Scrub, "visuals/low#scrub", 1, 0.1),
        unit(Tier::EditorMedia, "editor/low", 40, 0.1),
        unit(Tier::Video, "video/low.mp4", 300, 0.1),
        unit(Tier::Scrub, "visuals/high#scrub", 1, 3.0),
        unit(Tier::EditorMedia, "editor/high", 40, 3.0),
        unit(Tier::Video, "video/high.mp4", 300, 3.0),
        unit(Tier::Thumb, "thumbs/high.jpg", 1, 3.0),
    ];
    let order = evict_plan(&units, 0, 0, u64::MAX, 0, &[]);
    assert_eq!(
        order,
        [
            "video/low.mp4",
            "editor/low",
            "visuals/low#scrub",
            "video/high.mp4",
            "editor/high",
            "visuals/high#scrub",
            "thumbs/low.jpg",
            "thumbs/high.jpg"
        ],
        "low-score clip loses everything (costliest first) before a high-score clip loses anything; thumbnails last"
    );
    // Budget pressure of 300 MB: only the low-score video copy goes.
    let total: u64 = units.iter().map(|u| u.bytes).sum();
    assert_eq!(evict_plan(&units, 0, total - (300 << 20), u64::MAX, 0, &[]), ["video/low.mp4"]);
    // Video cap: copies beyond half the budget go even when the total fits.
    assert_eq!(evict_plan(&units, 0, u64::MAX, 400 << 20, 0, &[]), ["video/low.mp4"]);
    // The open clip is kept whole.
    let keep = ["video/low.mp4".to_string(), "editor/low".to_string()];
    assert_eq!(evict_plan(&units, 0, total - (300 << 20), u64::MAX, 0, &keep), ["visuals/low#scrub", "video/high.mp4"]);
}

#[test]
fn fill_stops_at_95_percent_of_the_budget() {
    let budget = 1000u64;
    let gate = Gate::new(budget, 900, 0);
    assert!(gate.admit(Step::Scrub, 50), "900 + 50 = 950 = 95%");
    gate.add(Step::Scrub, 50);
    assert!(!gate.admit(Step::Scrub, 1), "full at 95%");
    assert!(!gate.admit(Step::Editor, 1));
    assert!(gate.admit(Step::Thumb, 30) && gate.admit(Step::Duration, 0), "thumbnails / durations always fill");
    assert_eq!(gate.used(), 950);
    let video = Gate::new(budget, 0, 400);
    assert!(video.admit(Step::Video, 100), "400 + 100 = half the budget");
    assert!(!video.admit(Step::Video, 101), "never past half the budget");
    let mut clip = super::tests::clip("x.mp4", true, true);
    clip.duration = Some(100.0);
    assert_eq!(estimate(Step::Editor, &clip), 28_800_000 + (200 << 10));
    assert_eq!(estimate(Step::Video, &clip), 100 * super::light::LIGHT_BYTES_PER_SEC, "a light copy, not the whole clip");
}

#[test]
fn the_recorder_holds_only_slow_drive_reads() {
    let ssd = std::env::temp_dir();
    // This machine's temp folder is on the NVMe system drive; elsewhere the check still holds.
    assert_eq!(super::prefetch::holds(true, &ssd), super::is_slow(&ssd));
    if super::drive_kind(&ssd) == super::DriveKind::Ssd && std::env::var_os("RECALL_SLOW_DRIVE").is_none() {
        assert!(!super::prefetch::holds(true, &ssd), "an SSD read (the video-tier copy) never waits for the recorder");
    }
    // No Q: drive here: an unknown volume counts as a hard drive (`drive_kind`).
    let hdd = std::path::Path::new(r"Q:\no-such-drive\clip.mp4");
    assert!(super::prefetch::holds(true, hdd), "an unknown drive counts as a hard drive");
    assert!(!super::prefetch::holds(false, hdd), "nothing waits while the recorder is idle");
}

struct BusyAware(Mutex<VecDeque<Task>>);
impl IdleSource for BusyAware {
    fn next_task(&self) -> Option<Task> {
        self.0.lock().unwrap().pop_front()
    }
    fn paused(&self) -> bool {
        recorder_busy()
    }
}

#[test]
fn planner_pauses_while_the_recorder_is_busy() {
    let lane = Lane::spawn("test-lane-busy", false, Duration::from_millis(40));
    let ran = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    set_recorder_busy(true);
    let log = ran.clone();
    lane.set_idle(Some(Arc::new(BusyAware(Mutex::new(VecDeque::from([Box::new(move || log.lock().unwrap().push("fill")) as Task]))))));
    let demand = ran.clone();
    lane.submit(Priority::Interactive, Box::new(move || demand.lock().unwrap().push("on-demand")));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(*ran.lock().unwrap(), ["on-demand"], "on-demand work runs, the fill waits");
    set_recorder_busy(false);
    let start = Instant::now();
    while ran.lock().unwrap().len() < 2 {
        assert!(start.elapsed() < Duration::from_secs(5), "fill never resumed");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(*ran.lock().unwrap(), ["on-demand", "fill"]);
}

#[test]
fn inventory_classifies_tiers_and_enforcement_frees_on_disk() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("recall-smart-{}-{nonce}", std::process::id()));
    let write = |rel: &str, len: usize| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![1u8; len]).unwrap();
    };
    write("video/aaaa.mp4", 5000);
    write("editor/k1/audio-0.wav", 3000);
    write("editor/k1/clip.json", 200);
    write("visuals/aaaa/poster.jpg", 300);
    write("visuals/aaaa/frame-0.jpg", 300);
    write("visuals/aaaa/scrub-0.jpg", 400);
    write("thumbs/aaaa.jpg", 250);
    write("projects/aaaa.json", 100);
    std::fs::write(root.join("media.json"), br#"{"owners":{"k1":"aaaa"}}"#).unwrap();
    let inventory = budget::inventory(&root, &|id| if id == "aaaa" { 2.0 } else { 0.0 });
    let tier = |t: Tier| inventory.tier_bytes(t);
    assert_eq!((tier(Tier::Video), tier(Tier::EditorMedia), tier(Tier::Scrub), tier(Tier::Thumb)), (5000, 3800, 400, 250));
    assert!(inventory.units.iter().all(|u| u.score == 2.0), "every unit maps to its clip, the engine folder via media.json owners");
    assert_eq!(inventory.projects, 100);
    let usage = tiers::usage_in(&root);
    assert_eq!(usage.total(), inventory.total());
    assert_eq!(usage.video_cap, usage.budget / 2);
    // Budget just above the thumbnail + project + bookkeeping bytes: everything else goes.
    // (Reading the budget saved a first-run storage.json: bookkeeping grew.)
    let other = budget::inventory(&root, &|_| 0.0).other;
    assert!(root.join("storage.json").exists());
    let freed = budget::enforce(&root, 250 + 100 + other + 10, 0);
    assert_eq!(freed, 5000 + 3800 + 400);
    assert!(root.join("thumbs/aaaa.jpg").exists() && root.join("projects/aaaa.json").exists());
    assert!(!root.join("editor/k1").exists() && !root.join("video/aaaa.mp4").exists());
    let _ = std::fs::remove_dir_all(root);
}

fn video(name: &str, bytes: u64, score: f64) -> Unit {
    Unit { tier: Tier::Video, name: format!("video/{name}.mp4"), bytes, last_used: 0, score }
}

#[test]
fn warm_up_stops_at_the_margin_and_never_drops_a_better_clip() {
    use super::evict::room_below;
    let (limit, cap) = (950, 500);
    // Room as it is: nothing to drop.
    assert_eq!(room_below(&[video("a", 100, 1.0)], 0, limit, cap, 200, 0.5, &[]), Some(vec![]));
    // The video cap is reached: a lower clip makes way, the lowest first.
    let units = [video("low", 200, 0.2), video("lower", 200, 0.1), video("high", 100, 3.0)];
    assert_eq!(room_below(&units, 0, limit, cap, 200, 1.0, &[]), Some(vec!["video/lower.mp4".to_string()]));
    // Only better clips hold the room: the copy is skipped, nothing is dropped.
    assert_eq!(room_below(&units, 0, limit, cap, 200, 0.05, &[]), None);
    // Equal scores do not trade places either.
    assert_eq!(room_below(&units, 0, limit, cap, 200, 0.1, &[]), None, "strictly lower only");
    // The clip open in the editor is never the one dropped.
    assert_eq!(room_below(&units, 0, limit, cap, 200, 1.0, &["video/lower.mp4".into()]), Some(vec!["video/low.mp4".to_string()]));
    // Other tiers fill the cache to the margin: dropping lower video copies is not enough.
    assert_eq!(room_below(&units, 700, limit, cap, 200, 1.0, &[]), None, "the fill stops at budget minus the margin");
    // The same rules at any budget: a 50 GB cache takes a 400 MB clip without dropping anything.
    let gb = 1u64 << 30;
    let big = [video("x", 400 << 20, 0.1)];
    assert_eq!(room_below(&big, gb, 50 * gb / 100 * 95, 25 * gb, 400 << 20, 2.0, &[]), Some(vec![]));
    assert_eq!(super::prefetch::warm_limit(2 * gb), 2 * gb / 100 * 95);
}

#[test]
fn a_first_run_budget_is_20_gib_a_quarter_of_free_space_and_keeps_a_tenth_of_the_drive() {
    use super::evict::{DEFAULT_CACHE_BUDGET, MIN_CACHE_BUDGET, default_budget};
    let gib = 1u64 << 30;
    assert_eq!(DEFAULT_CACHE_BUDGET, 20 * gib);
    assert_eq!(default_budget(500 * gib, 1000 * gib), 20 * gib, "plenty of room: 20 GiB");
    assert_eq!(default_budget(60 * gib, 100 * gib), 15 * gib, "a quarter of the free space");
    assert_eq!(default_budget(40 * gib, 300 * gib), 10 * gib, "a quarter, when the drive keeps its tenth anyway");
    assert_eq!(default_budget(40 * gib + gib / 2, 350 * gib), 5 * gib, "free less a tenth of the drive, whole GiB down");
    // This PC: C: is 930.5 GiB with 38.7 GiB free, under a tenth of the drive already.
    assert_eq!(default_budget(387 * gib / 10, 9305 * gib / 10), MIN_CACHE_BUDGET, "a nearly full drive gets the 2 GiB floor");
    assert_eq!(default_budget(0, 0), MIN_CACHE_BUDGET, "unknown free space");
}

#[test]
fn a_set_budget_always_wins_and_a_first_run_saves_its_pick() {
    let nonce = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("recall-budget-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let picked = super::video::budget_in(&root);
    assert!((super::evict::MIN_CACHE_BUDGET..=super::evict::DEFAULT_CACHE_BUDGET).contains(&picked), "{picked}");
    assert_eq!(picked % (1 << 30), 0, "whole GiB");
    assert_eq!(super::evict::read_settings(&root).cache_budget_bytes, Some(picked), "saved, so it does not move with free space");
    let mut settings = super::evict::read_settings(&root);
    settings.cache_budget_bytes = Some(50 << 30);
    super::evict::write_settings(&root, &settings).unwrap();
    assert_eq!(super::video::budget_in(&root), 50 << 30, "the Settings value wins");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_clip_prepared_with_flac_stems_drops_its_wav_stems() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("recall-stems-{}-{nonce}", std::process::id()));
    let editor = root.join("editor");
    let write = |dir: &str, file: &str| {
        std::fs::create_dir_all(editor.join(dir)).unwrap();
        std::fs::write(editor.join(dir).join(file), vec![1u8; 500]).unwrap();
    };
    write("old-wav", "audio-0.wav");
    write("other-clip", "audio-0.wav");
    write("new-flac", "audio-0.flac");
    std::fs::write(root.join("media.json"), br#"{"owners":{"old-wav":"aaaa","other-clip":"bbbb"}}"#).unwrap();
    budget::note_editor_media(&root, "aaaa", &[editor.join("new-flac")], false);
    assert!(!editor.join("old-wav").exists(), "this clip's WAV stems go once its FLAC stems exist");
    assert!(editor.join("other-clip").exists(), "another clip keeps its stems until it is prepared again");
    assert!(editor.join("new-flac").exists());
    let media = budget::read_media(&root);
    assert_eq!(media.owners.get("new-flac").map(String::as_str), Some("aaaa"));
    assert!(!media.owners.contains_key("old-wav"));
    let _ = std::fs::remove_dir_all(&root);
}


#[test]
fn light_copies_are_video_units_ranked_and_evicted_by_their_clip() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("recall-light-units-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(root.join("light")).unwrap();
    std::fs::write(root.join("light").join("aaaa.mp4"), vec![1u8; 3000]).unwrap();
    std::fs::write(root.join("light").join("bbbb.mp4"), vec![1u8; 3000]).unwrap();
    let inventory = budget::inventory(&root, &|id| if id == "aaaa" { 5.0 } else { 0.5 });
    let light: Vec<(&str, f64)> = inventory.units.iter().filter(|u| u.name.starts_with("light/")).map(|u| (u.name.as_str(), u.score)).collect();
    assert_eq!(light.len(), 2);
    assert!(inventory.units.iter().filter(|u| u.name.starts_with("light/")).all(|u| u.tier == Tier::Video));
    assert!(light.contains(&("light/aaaa.mp4", 5.0)));
    // Room for one more light copy of a clip scored 1.0: only the lower-scored one may go.
    let evicted = super::evict::room_below(&inventory.units, 0, 6000, 1 << 20, 3000, 1.0, &[]);
    assert_eq!(evicted, Some(vec!["light/bbbb.mp4".to_string()]));
    assert_eq!(budget::delete(&root, inventory.units.iter().find(|u| u.name == "light/bbbb.mp4").unwrap()), 3000);
    assert!(!root.join("light").join("bbbb.mp4").exists());
    let _ = std::fs::remove_dir_all(&root);
}
