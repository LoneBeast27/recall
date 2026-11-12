//! The helper's command line, built by the app and parsed (and distrusted) by
//! the helper. Everything the helper acts on arrives here; it reads no input
//! file. It writes one result file into its own user's fixes folder, named by
//! a random nonce, which the app then reads.
//!
//! - apply: `--fix-apply <fix-id> <nonce> "<target>" <plan as hex JSON>`
//! - undo:  `--fix-undo <fix-id> <nonce> "<target>" <undo as hex JSON>`
//!
//! Apply carries the plan the user read: the helper builds the plan again
//! from the live state and changes nothing unless the two are identical.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plan::{FixId, Plan};
use crate::receipt::Undo;

pub const APPLY: &str = "--fix-apply";
pub const UNDO: &str = "--fix-undo";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Apply { fix: FixId, nonce: String, target: String, plan: Plan },
    Undo { fix: FixId, nonce: String, target: String, undo: Undo },
}

impl Request {
    pub fn nonce(&self) -> &str {
        match self {
            Request::Apply { nonce, .. } | Request::Undo { nonce, .. } => nonce,
        }
    }

    /// The parameters after the helper's file name. Fails on a value the
    /// command line cannot carry safely.
    pub fn arguments(&self) -> Result<String, String> {
        let (verb, fix, nonce, target, tail) = match self {
            Request::Apply { fix, nonce, target, plan } => (APPLY, fix, nonce, target, serde_json::to_vec(plan)),
            Request::Undo { fix, nonce, target, undo } => (UNDO, fix, nonce, target, serde_json::to_vec(undo)),
        };
        let tail = hex(&tail.map_err(|e| e.to_string())?);
        check_nonce(nonce)?;
        check_target(target)?;
        Ok(format!("{verb} {} {nonce} \"{target}\" {tail}", fix.as_str()))
    }

    /// `args` = the process arguments after the program name.
    pub fn parse(args: &[String]) -> Result<Request, String> {
        let verb = args.first().map(String::as_str).unwrap_or("");
        if verb != APPLY && verb != UNDO {
            return Err(format!("not a fix request: {verb:?}"));
        }
        if args.len() != 5 {
            return Err(format!("{verb} takes 4 values, got {}", args.len().saturating_sub(1)));
        }
        let fix = FixId::parse(&args[1]).ok_or_else(|| format!("unknown fix {:?}", args[1]))?;
        let nonce = args[2].clone();
        check_nonce(&nonce)?;
        let target = args[3].clone();
        check_target(&target)?;
        let bytes = unhex(&args[4]).ok_or("the last value is not hex")?;
        Ok(if verb == APPLY {
            let plan: Plan = serde_json::from_slice(&bytes).map_err(|e| format!("plan: {e}"))?;
            if plan.fix != fix {
                return Err("the plan is for another fix".into());
            }
            Request::Apply { fix, nonce, target, plan }
        } else {
            let undo: Undo = serde_json::from_slice(&bytes).map_err(|e| format!("undo data: {e}"))?;
            Request::Undo { fix, nonce, target, undo }
        })
    }
}

/// What the helper reports back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// `None` = done.
    pub error: Option<String>,
    /// Apply only: how to reverse what was done.
    pub undo: Vec<Undo>,
}

/// `%LOCALAPPDATA%\recall-hdr\fixes\results`, resolved by each side itself.
pub fn results_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("recall-hdr").join("fixes").join("results"))
}

pub fn result_path(dir: &Path, nonce: &str) -> Result<PathBuf, String> {
    check_nonce(nonce)?;
    Ok(dir.join(format!("{nonce}.json")))
}

/// 32 lowercase hex digits: a file name, never a path.
pub fn check_nonce(nonce: &str) -> Result<(), String> {
    if nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(())
    } else {
        Err(format!("bad nonce {nonce:?}"))
    }
}

/// A full local path with no quote (the command line could not carry it).
pub fn check_target(target: &str) -> Result<(), String> {
    let p = Path::new(target);
    let local = target.len() > 3 && target.as_bytes()[0].is_ascii_alphabetic() && &target[1..3] == ":\\";
    if !local || !p.is_absolute() || target.contains('"') || target.contains("..") || target.chars().any(char::is_control) {
        return Err(format!("bad target {target:?}"));
    }
    Ok(())
}

/// A fresh nonce from the OS's hasher seed, the clock and the process id.
pub fn new_nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
