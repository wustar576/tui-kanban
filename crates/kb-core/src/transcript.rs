//! Finds the session a conversation was moved from (docs/board-v2.md section 12.8).
//!
//! When Claude Code moves a running conversation to a new session id, it ends the old
//! transcript with a record like
//! `{"type":"continued-in","sessionId":"<old>","continuedInSessionId":"<new>"}`.
//! That record is undocumented, so every problem here means "no predecessor".

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Bytes read from the end of each transcript.
pub const TAIL_BYTES: u64 = 4096;
/// Transcripts examined at most, newest first.
pub const MAX_FILES: usize = 200;
/// Only transcripts modified within this time are examined.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Predecessors followed at most.
pub const MAX_HOPS: usize = 5;

/// The predecessors of `session_id`, nearest first, at most [`MAX_HOPS`]: the session it
/// was continued from, then that session's predecessor, and so on. Looks at the other
/// `*.jsonl` files in the directory of `transcript_path`. Empty on any failure.
pub fn predecessors(transcript_path: &Path, session_id: &str) -> Vec<String> {
    let Some(dir) = transcript_path.parent() else {
        return Vec::new();
    };
    let links = continued_links(dir, transcript_path, SystemTime::now());
    chain(&links, session_id)
}

/// Follows `new -> old` links from `session_id`, at most [`MAX_HOPS`], without cycles.
pub fn chain(links: &HashMap<String, String>, session_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = session_id.to_string();
    while out.len() < MAX_HOPS {
        let Some(prev) = links.get(&current) else {
            break;
        };
        if prev == session_id || out.contains(prev) {
            break;
        }
        out.push(prev.clone());
        current = prev.clone();
    }
    out
}

/// `continuedInSessionId -> sessionId` for every `continued-in` record found in the tails
/// of the recent `*.jsonl` files of `dir` (except `skip`).
pub fn continued_links(dir: &Path, skip: &Path, now: SystemTime) -> HashMap<String, String> {
    let mut links = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return links;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") || path == skip {
                return None;
            }
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta.modified().ok()?;
            let recent = now
                .duration_since(modified)
                .map(|age| age <= MAX_AGE)
                .unwrap_or(true); // modified "in the future": recent
            recent.then_some((modified, path))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, path) in files.into_iter().take(MAX_FILES) {
        let Some(tail) = read_tail(&path, TAIL_BYTES) else {
            continue;
        };
        for (new, old) in continued_in_records(&tail) {
            links.entry(new).or_insert(old);
        }
    }
    links
}

/// The last `max` bytes of a file, as text (invalid UTF-8 replaced). `None` when the file
/// cannot be read.
pub fn read_tail(path: &Path, max: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        // The first line is cut; drop it.
        text = text
            .split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default();
    }
    Some(text)
}

/// `(continuedInSessionId, sessionId)` of every `continued-in` line in `text`.
pub fn continued_in_records(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| l.contains("continued-in"))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("continued-in"))
        .filter_map(|v| {
            let field = |k: &str| {
                v.get(k)
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            Some((field("continuedInSessionId")?, field("sessionId")?))
        })
        .collect()
}
