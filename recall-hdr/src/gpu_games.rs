//! The GPU vendor's game list, for the record path's render check: a
//! fullscreen exe that the vendor's driver treats as a game commits on the
//! 300 ms rule with no render check (`record_render`). The deny lists still
//! run first (`record_target::is_target`), so a vendor never makes a browser
//! or a launcher a target.
//!
//! Each vendor is its own crate behind [`GameKnowledge`]: NVIDIA today
//! (`recall-nvidia`, driver profile DB); AMD and Intel can be added beside it
//! in [`open_vendor`]. Lookups run on one lazy worker thread, one per new exe
//! path, and the verdict is cached for the controller's life: nothing polls,
//! and the pump thread never waits for a driver call. Without a vendor (no
//! NVIDIA driver) the worker logs once and every answer is "not known".

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::log;

/// A GPU vendor's knowledge of which exes are games.
pub trait GameKnowledge {
    fn vendor(&self) -> &'static str;
    /// One verdict per exe path, in order: `true` = the vendor treats it as a game.
    fn games(&self, exe_paths: &[String]) -> Result<Vec<bool>, String>;
}

struct Nvidia(recall_nvidia::Nvapi);

impl GameKnowledge for Nvidia {
    fn vendor(&self) -> &'static str {
        "NVIDIA"
    }

    fn games(&self, exe_paths: &[String]) -> Result<Vec<bool>, String> {
        let apps: Vec<&str> = exe_paths.iter().map(String::as_str).collect();
        Ok(self.0.lookup(&apps)?.iter().map(recall_nvidia::Lookup::is_game).collect())
    }
}

/// This PC's vendor, or why there is none.
pub fn open_vendor() -> Result<Box<dyn GameKnowledge>, String> {
    // AMD / Intel: try their crates here when the NVIDIA driver is absent.
    recall_nvidia::Nvapi::load().map(|nv| Box::new(Nvidia(nv)) as Box<dyn GameKnowledge>)
}

type Opener = fn() -> Result<Box<dyn GameKnowledge>, String>;

enum Reply {
    Ready(&'static str),
    Off(String),
    Verdicts(Vec<(String, bool)>),
    Failed(Vec<String>, String),
}

/// Cached vendor verdicts by exe path, asked on a worker thread.
pub struct KnownGames {
    open: Opener,
    link: Option<(Sender<String>, Receiver<Reply>)>,
    vendor: &'static str,
    off: bool,
    /// Lower-case exe path -> verdict.
    verdicts: HashMap<String, bool>,
    asked: HashSet<String>,
    /// pid -> (exe name, lower-case path), so a pid is resolved once.
    paths: HashMap<u32, (String, String)>,
}

impl Default for KnownGames {
    fn default() -> Self {
        Self::new()
    }
}

impl KnownGames {
    pub fn new() -> Self {
        Self::with(open_vendor)
    }

    pub(crate) fn with(open: Opener) -> Self {
        KnownGames { open, link: None, vendor: "GPU", off: false, verdicts: HashMap::new(), asked: HashSet::new(), paths: HashMap::new() }
    }

    /// Is `pid` (running `exe_name`) a game per the vendor? `None` = not
    /// known yet (the lookup is on its way) or the path is unreadable.
    pub fn for_pid(&mut self, pid: u32, exe_name: &str) -> Option<bool> {
        let path = match self.paths.get(&pid) {
            Some((name, path)) if name == exe_name => path.clone(),
            _ => {
                let path = crate::watcher::exe_path(pid)?.to_lowercase();
                if self.paths.len() >= 256 {
                    self.paths.clear();
                }
                self.paths.insert(pid, (exe_name.to_string(), path.clone()));
                path
            }
        };
        self.for_path(&path)
    }

    /// The vendor's verdict for an exe path (see [`Self::for_pid`]).
    pub fn for_path(&mut self, path: &str) -> Option<bool> {
        self.drain();
        if self.off {
            return Some(false);
        }
        let key = path.to_lowercase();
        if let Some(v) = self.verdicts.get(&key) {
            return Some(*v);
        }
        if self.asked.insert(key.clone()) {
            self.send(key);
        }
        None
    }

    /// The vendor's name for log lines ("NVIDIA").
    pub fn vendor(&self) -> &'static str {
        self.vendor
    }

    fn send(&mut self, path: String) {
        if self.link.is_none() {
            self.link = Some(spawn(self.open));
        }
        let gone = self.link.as_ref().is_some_and(|(tx, _)| tx.send(path).is_err());
        if gone {
            log!("record: GPU game list: lookup thread ended; the render check decides alone");
            self.off = true;
        }
    }

    fn drain(&mut self) {
        let Some((_, rx)) = &self.link else { return };
        let replies: Vec<Reply> = rx.try_iter().collect();
        for r in replies {
            match r {
                Reply::Ready(vendor) => {
                    self.vendor = vendor;
                    log!("record: GPU game list: {vendor} driver profiles; a fullscreen exe {vendor} lists as a game skips the render check");
                }
                Reply::Off(why) => {
                    log!("record: GPU game list off ({why}); the render check decides alone");
                    self.off = true;
                }
                Reply::Verdicts(v) => {
                    for (path, game) in v {
                        if game {
                            log!("record: GPU game list: {} lists {path} as a game", self.vendor);
                        }
                        self.verdicts.insert(path, game);
                    }
                }
                Reply::Failed(paths, why) => {
                    log!("record: GPU game list: lookup failed ({why}); {} exe(s) left to the render check", paths.len());
                    for p in paths {
                        self.verdicts.insert(p, false);
                    }
                }
            }
        }
    }
}

/// The lookup thread: opens the vendor once, then answers batches of paths
/// as they arrive (blocking receive: no polling). It ends with the sender.
fn spawn(open: Opener) -> (Sender<String>, Receiver<Reply>) {
    let (req_tx, req_rx) = channel::<String>();
    let (reply_tx, reply_rx) = channel::<Reply>();
    let started = std::thread::Builder::new().name("gpu-games".into()).spawn(move || {
        let vendor = match open() {
            Ok(v) => {
                let _ = reply_tx.send(Reply::Ready(v.vendor()));
                v
            }
            Err(why) => {
                let _ = reply_tx.send(Reply::Off(why));
                return;
            }
        };
        while let Ok(first) = req_rx.recv() {
            let mut batch = vec![first];
            batch.extend(req_rx.try_iter());
            let reply = match vendor.games(&batch) {
                Ok(v) if v.len() == batch.len() => Reply::Verdicts(batch.into_iter().zip(v).collect()),
                Ok(v) => Reply::Failed(batch, format!("{} answers for the paths asked", v.len())),
                Err(why) => Reply::Failed(batch, why),
            };
            if reply_tx.send(reply).is_err() {
                return;
            }
        }
    });
    if let Err(e) = started {
        // The receiver below then reports nothing; `send` sees the dropped thread and turns off.
        log!("record: GPU game list: lookup thread did not start ({e})");
    }
    (req_tx, reply_rx)
}

#[cfg(test)]
#[path = "gpu_games_tests.rs"]
mod tests;
