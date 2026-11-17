//! Finding the predecessor of a moved conversation from transcript tails
//! (docs/board-v2.md section 12.8). No database.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kb_core::transcript::{self, MAX_HOPS};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/transcripts");
const CURRENT: &str = "33333333-cccc";

/// A fresh copy of the fixture directory (fresh modification times).
fn fixture_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("kb-transcripts-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for entry in std::fs::read_dir(FIXTURES).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
    }
    dir
}

#[test]
fn record_parsing_accepts_only_continued_in_objects() {
    let text = "{\"type\":\"user\",\"sessionId\":\"a\"}\r\n\
                {\"type\":\"continued-in\",\"timestamp\":\"2026-11-16T03:28:08.567Z\",\"sessionId\":\"old\",\"continuedInSessionId\":\"new\"}\r\n\
                {\"type\":\"continued-in\",\"sessionId\":\"x\"}\n\
                not json continued-in\n\
                [\"continued-in\"]\n";
    assert_eq!(
        transcript::continued_in_records(text),
        [("new".to_string(), "old".to_string())]
    );
    assert!(transcript::continued_in_records("").is_empty());
}

#[test]
fn predecessors_follow_the_chain_in_the_transcript_directory() {
    let dir = fixture_dir();
    let current = dir.join("current.jsonl");
    // chain_b: bbbb -> cccc; chain_a: aaaa -> bbbb. Broken JSON, other sessions and other
    // file extensions are ignored.
    assert_eq!(
        transcript::predecessors(&current, CURRENT),
        ["22222222-bbbb", "11111111-aaaa"]
    );
    assert_eq!(
        transcript::predecessors(&current, "22222222-bbbb"),
        ["11111111-aaaa"]
    );
    assert!(transcript::predecessors(&current, "11111111-aaaa").is_empty());
    assert!(transcript::predecessors(&current, "unknown").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_failure_means_no_predecessor() {
    // No directory, a path without a parent, an empty directory.
    let missing = std::env::temp_dir()
        .join("kb-no-such-dir-4711")
        .join("x.jsonl");
    assert!(transcript::predecessors(&missing, CURRENT).is_empty());
    assert!(transcript::predecessors(Path::new(""), CURRENT).is_empty());
    let empty = std::env::temp_dir().join(format!("kb-empty-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    assert!(transcript::predecessors(&empty.join("x.jsonl"), CURRENT).is_empty());
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn old_files_are_skipped() {
    let dir = fixture_dir();
    let skip = dir.join("current.jsonl");
    let now = SystemTime::now();
    assert_eq!(
        transcript::continued_links(&dir, &skip, now)
            .get(CURRENT)
            .map(String::as_str),
        Some("22222222-bbbb")
    );
    // Eight days later every fixture is too old.
    let later = now + Duration::from_secs(8 * 24 * 3600);
    assert!(transcript::continued_links(&dir, &skip, later).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_the_tail_of_a_large_file_is_read() {
    let dir = fixture_dir();
    let big = dir.join("big.jsonl");
    let mut f = std::fs::File::create(&big).unwrap();
    // A record near the start, beyond the tail: not seen.
    writeln!(
        f,
        "{{\"type\":\"continued-in\",\"sessionId\":\"early\",\"continuedInSessionId\":\"early-new\"}}"
    )
    .unwrap();
    for i in 0..200 {
        writeln!(
            f,
            "{{\"type\":\"assistant\",\"n\":{i},\"text\":\"{}\"}}",
            "x".repeat(40)
        )
        .unwrap();
    }
    writeln!(
        f,
        "{{\"type\":\"continued-in\",\"sessionId\":\"late\",\"continuedInSessionId\":\"late-new\"}}"
    )
    .unwrap();
    drop(f);
    assert!(std::fs::metadata(&big).unwrap().len() > transcript::TAIL_BYTES * 2);
    let tail = transcript::read_tail(&big, transcript::TAIL_BYTES).unwrap();
    assert!(tail.len() <= transcript::TAIL_BYTES as usize);
    assert!(
        tail.starts_with("{\"type\":\"assistant\""),
        "cut line dropped"
    );
    let links = transcript::continued_links(&dir, &dir.join("current.jsonl"), SystemTime::now());
    assert_eq!(links.get("late-new").map(String::as_str), Some("late"));
    assert!(!links.contains_key("early-new"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chains_stop_at_cycles_and_after_five_hops() {
    let links: HashMap<String, String> = (0..10)
        .map(|i| (format!("s{}", i + 1), format!("s{i}")))
        .collect();
    let chain = transcript::chain(&links, "s10");
    assert_eq!(chain.len(), MAX_HOPS);
    assert_eq!(chain, ["s9", "s8", "s7", "s6", "s5"]);
    let cycle: HashMap<String, String> = [("a", "b"), ("b", "a")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(transcript::chain(&cycle, "a"), ["b"]);
}
