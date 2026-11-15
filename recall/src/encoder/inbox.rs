//! The encoder thread's inbox: frames and save wakes arrive on one bounded channel, the
//! save requests themselves on their own unbounded one.
//!
//! The encoder thread blocks on the frame channel. A save request alone would sit there
//! until the next frame (none comes while the game is static or minimized), so sending a
//! save also posts a [`Inbox::Wake`] on the frame channel: the loop then handles it at
//! once instead of polling the save channel every 100 ms.
//!
//! The frame senders share one `Arc`: the loop ends when the last frame sender drops (the
//! quit path), exactly as before. A save sender only holds a `Weak` to it, so a save hook
//! that outlives the encoder never keeps the loop alive.

use std::sync::mpsc::{channel, sync_channel, Receiver, SendError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Weak};

use super::{PacedFrame, SaveRequest};

/// One item on the frame channel.
pub(crate) enum Inbox {
    Frame(PacedFrame),
    /// A save request is waiting on the save channel.
    Wake,
}

/// Capture's side: frames for the encoder (`try_send` never blocks capture).
#[derive(Clone)]
pub struct FrameSender(Arc<SyncSender<Inbox>>);

impl FrameSender {
    pub fn try_send(&self, frame: PacedFrame) -> Result<(), TrySendError<PacedFrame>> {
        self.0.try_send(Inbox::Frame(frame)).map_err(|e| match e {
            TrySendError::Full(Inbox::Frame(f)) => TrySendError::Full(f),
            TrySendError::Disconnected(Inbox::Frame(f)) => TrySendError::Disconnected(f),
            _ => unreachable!("a frame was sent"),
        })
    }

    /// Blocking send (tests that must not drop a frame).
    #[cfg(test)]
    pub fn send(&self, frame: PacedFrame) -> Result<(), SendError<PacedFrame>> {
        self.0.send(Inbox::Frame(frame)).map_err(|SendError(e)| match e {
            Inbox::Frame(f) => SendError(f),
            Inbox::Wake => unreachable!("a frame was sent"),
        })
    }
}

/// The save hook's side: a request, then a wake for the loop.
#[derive(Clone)]
pub struct SaveSender {
    saves: Sender<SaveRequest>,
    wake: Weak<SyncSender<Inbox>>,
}

impl SaveSender {
    pub fn send(&self, req: SaveRequest) -> Result<(), SendError<SaveRequest>> {
        self.saves.send(req)?;
        if let Some(wake) = self.wake.upgrade() {
            // Full: frames are queued, so the loop is awake and reads saves before the
            // next one. Disconnected: the loop is ending and drains saves on its way out.
            let _ = wake.try_send(Inbox::Wake);
        }
        Ok(())
    }
}

/// The encoder thread's receiving end.
pub(crate) struct InboxRx {
    pub frames: Receiver<Inbox>,
    pub saves: Receiver<SaveRequest>,
}

/// Frame channel of `depth` (frames and wakes), save channel unbounded.
pub(crate) fn inbox(depth: usize) -> (FrameSender, SaveSender, InboxRx) {
    let (ftx, frx) = sync_channel(depth);
    let (stx, srx) = channel();
    let frames = Arc::new(ftx);
    let saves = SaveSender { saves: stx, wake: Arc::downgrade(&frames) };
    (FrameSender(frames), saves, InboxRx { frames: frx, saves: srx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(idx: u64) -> PacedFrame {
        PacedFrame { idx, width: 2, height: 2, rgba: Vec::new(), gpu: None, content: None }
    }

    fn save() -> SaveRequest {
        SaveRequest { path: String::new(), game: None, seconds: 1, end_us: None, at_last_frame: false, requested: std::time::Instant::now() }
    }

    #[test]
    fn a_save_wakes_a_loop_that_waits_for_frames() {
        let (_frames, saves, rx) = inbox(3);
        saves.send(save()).unwrap();
        // No frame was sent: the wake alone ends the wait.
        assert!(matches!(rx.frames.recv_timeout(Duration::from_millis(50)), Ok(Inbox::Wake)));
        assert!(rx.saves.try_recv().is_ok());
    }

    #[test]
    fn a_save_sender_does_not_keep_the_loop_alive() {
        let (frames, saves, rx) = inbox(3);
        let more = frames.clone();
        drop(frames);
        assert!(rx.frames.try_recv().is_err_and(|e| e == std::sync::mpsc::TryRecvError::Empty));
        drop(more);
        // The quit path: the last frame sender gone, the save sender still alive.
        assert!(matches!(rx.frames.recv_timeout(Duration::from_millis(50)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected)));
        saves.send(save()).unwrap(); // the request still queues, for the loop's final drain
        assert!(rx.saves.try_recv().is_ok());
    }

    #[test]
    fn a_full_queue_keeps_its_frames_and_still_gets_the_save() {
        let (frames, saves, rx) = inbox(2);
        frames.try_send(frame(1)).ok().unwrap();
        frames.try_send(frame(2)).ok().unwrap();
        assert!(matches!(frames.try_send(frame(3)), Err(TrySendError::Full(f)) if f.idx == 3));
        saves.send(save()).unwrap();
        assert!(matches!(rx.frames.try_recv(), Ok(Inbox::Frame(f)) if f.idx == 1));
        assert!(matches!(rx.frames.try_recv(), Ok(Inbox::Frame(f)) if f.idx == 2));
        assert!(rx.saves.try_recv().is_ok());
    }
}
