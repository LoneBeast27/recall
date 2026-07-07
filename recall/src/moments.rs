//! SteelSeries Moments gallery injection (feature `encode`, Phase-0 interim
//! gallery per PRD §8). One INSERT into `moments_clips` per saved clip.
//!
//! Safety contract:
//! - The live DB is BACKED UP (db + -wal sidecar) once per engine run before
//!   the first write, into `<out_dir>\moments-db-backups\`.
//! - The goose schema version is checked against the version this module was
//!   written for; a mismatch SKIPS injection (clip + thumb still saved).
//! - Desktop captures use Moments' own desktop-mode convention observed live:
//!   `library_game_unique_id = fe47c2b0-…` / name key `moments.desktopModeGameName`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Goose migration version this INSERT was validated against (recon 2026-07-08).
const EXPECTED_GOOSE_VERSION: i64 = 20260615105341;

/// Moments' fixed id/name for desktop-mode (non-game) captures.
const DESKTOP_GAME_ID: &str = "fe47c2b0-5b55-49e1-869a-f78424437a59";
const DESKTOP_GAME_NAME: &str = "moments.desktopModeGameName";

static BACKED_UP: AtomicBool = AtomicBool::new(false);

pub struct ClipMeta {
    pub path: String,
    pub thumbnail_path: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub seconds: f64,
    pub filesize: u64,
    /// ISO 8601 with local offset, e.g. 2026-07-08T03:45:00+05:30.
    pub recorded_iso: String,
    pub session_id: String,
    /// Track names in mux order (mirrored into `audio_tracks_props`).
    pub audio_tracks: Vec<String>,
}

pub fn db_path() -> String {
    std::env::var("RECALL_MOMENTS_DB")
        .unwrap_or_else(|_| r"C:\ProgramData\SteelSeries\GG\apps\moments\db\database.db".to_string())
}

pub fn enabled() -> bool {
    std::env::var("RECALL_MOMENTS").map(|v| v != "0").unwrap_or(true)
}

/// Copy the DB (+ WAL sidecar) into `<backup_dir>` once per process.
fn backup_once(db: &str, backup_dir: &Path) -> Result<(), String> {
    if BACKED_UP.swap(true, Relaxed) {
        return Ok(());
    }
    std::fs::create_dir_all(backup_dir).map_err(|e| e.to_string())?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let dst = backup_dir.join(format!("database-{stamp}.db"));
    std::fs::copy(db, &dst).map_err(|e| format!("backup copy: {e}"))?;
    let wal = format!("{db}-wal");
    if Path::new(&wal).exists() {
        let _ = std::fs::copy(&wal, backup_dir.join(format!("database-{stamp}.db-wal")));
    }
    Ok(())
}

/// Insert one clip row. Returns the new row's UUID.
pub fn insert_clip(meta: &ClipMeta) -> Result<String, String> {
    let db = db_path();
    if !Path::new(&db).exists() {
        return Err(format!("moments db not found at {db}"));
    }
    let backup_dir = Path::new(&meta.path)
        .parent()
        .map(|p| p.join("moments-db-backups"))
        .unwrap_or_else(|| PathBuf::from("moments-db-backups"));
    backup_once(&db, &backup_dir)?;

    let conn = rusqlite::Connection::open(&db).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(|e| e.to_string())?;

    let goose: i64 = conn
        .query_row("select max(version_id) from goose_db_version", [], |r| r.get(0))
        .map_err(|e| format!("goose version read: {e}"))?;
    if goose != EXPECTED_GOOSE_VERSION {
        return Err(format!(
            "schema version {goose} != validated {EXPECTED_GOOSE_VERSION}; skipping insert (clip+thumb still on disk)"
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let name = format!(
        "Recall clip from {}",
        chrono::Local::now().format("%b %d, %Y %H:%M")
    );
    let keyframes = format!("[0,{}]", meta.seconds);
    let tracks_json = format!(
        "[{}]",
        meta.audio_tracks
            .iter()
            .map(|n| format!(r#"{{"name":"{n}","volume":1,"muted":false}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    conn.execute(
        "INSERT INTO moments_clips (
            id, name, description, path, clip_start_point, clip_end_point,
            is_autoclipped_legacy, is_manually_trimmed, framerate, audio_bitrate,
            audio_samplerate, video_bitrate, resolution_height, resolution_width,
            recording_timestamp, last_edit_timestamp, trimmed_clip_path,
            library_game_unique_id, last_game_name, keyframes, full_length,
            filesize, trimmed_filesize, thumbnail_path, gamesense_events,
            is_processing, is_deleted, tags, effects, last_view_timestamp,
            effects_schema_version, file_resolution_height, file_resolution_width,
            trigger_name, session_id, audio_tracks_props, audio_drop, video_drop,
            trigger_type, last_user_edit_timestamp, gsdk_version, capture_mode,
            video_audio_last_ts_diff, duplicated_frames_count, crop_format, crop_top_left
        ) VALUES (
            ?1, ?2, '', ?3, 0.0, ?4,
            0, 0, ?5, 192,
            48000, 0, ?6, ?7,
            ?8, '', '',
            ?9, ?10, ?11, ?4,
            ?12, 0, ?13, '[]',
            0, 0, '[]', '[]', '',
            1, ?6, ?7,
            '', ?14, ?15, 0.0, 0.0,
            'shortcut', '', 'recall-0.0.1', 'monitor',
            0.0, 0, '', ''
        )",
        rusqlite::params![
            id,
            name,
            meta.path,
            meta.seconds,
            meta.fps,
            meta.height,
            meta.width,
            meta.recorded_iso,
            DESKTOP_GAME_ID,
            DESKTOP_GAME_NAME,
            keyframes,
            meta.filesize as i64,
            meta.thumbnail_path,
            meta.session_id,
            tracks_json,
        ],
    )
    .map_err(|e| format!("insert: {e}"))?;
    Ok(id)
}
