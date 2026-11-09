//! The mover's one `encoder_log` timing line per move (`mover.rs`).

use super::mover::Moved;

pub(super) fn timing_line(m: &Moved, total_ms: f64, queue_wait_ms: f64) -> String {
    let mb_s = if m.copy_ms > 0.0 { m.bytes as f64 / 1e6 / (m.copy_ms / 1000.0) } else { 0.0 };
    format!(
        "move timing ({}): bytes={} queue_wait={queue_wait_ms:.1}ms copy={:.1}ms ({mb_s:.1} MB/s) \
         flush={:.1}ms rename={:.1}ms total={total_ms:.1}ms -> {}",
        m.how,
        m.bytes,
        m.copy_ms,
        m.flush_ms,
        m.rename_ms,
        m.dest.display()
    )
}
