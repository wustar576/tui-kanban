//! The background waiter of `kb hook wait` (docs/board-v2.md sections 13.3 and 13.4).
//!
//! Claude Code runs `kb hook wait` as an `asyncRewake` hook. The waiter blocks until the
//! session's board has an answer or a comment the session was not woken for yet, then
//! returns [`WaitEnd::Wake`]; the binary prints one line on standard error and exits with 2,
//! which wakes the session. Every other end is exit 0 (nothing shown).
//!
//! - One waiter per session: a session-level advisory lock held on the waiter's own
//!   connection (released when the process ends, even when it crashes), plus the row in
//!   `kb_waiter` that records who holds it.
//! - The waiter never marks anything delivered. It only sets `announced_at`, so it does not
//!   wake the session twice for the same item; `kb inbox` and the hooks still deliver it.
//! - It listens on `kb_events` (board changes) and `kb_waiter` (take-over requests), checks
//!   `CLAUDE_PID` natively every few seconds and polls the database slowly as a fallback.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use sqlx::postgres::{PgConnection, PgListener};
use tokio::sync::mpsc;

use crate::db;
use crate::error::{Error, Result};
use crate::events::{self, BoardEvent};
use crate::model::BoardStatus;
use crate::process;
use crate::read;

/// Channel on which a new waiter asks the current one to step down; the payload is the
/// session key.
pub const TAKEOVER_CHANNEL: &str = "kb_waiter";

/// Timing of the waiter. [`WaitOptions::new`] gives the values of docs/board-v2.md 13.3.
#[derive(Debug, Clone)]
pub struct WaitOptions {
    pub session_key: String,
    /// `CLAUDE_PID`: the Claude Code process the waiter belongs to. `None` when unknown;
    /// then the waiter does not check it.
    pub claude_pid: Option<u32>,
    /// Give up after this long (23 hours, below the hook's 24-hour timeout).
    pub max_wait: Duration,
    /// How often `CLAUDE_PID` is checked (locally, no database).
    pub alive_every: Duration,
    /// Fallback database poll, in case a notification is lost.
    pub poll_every: Duration,
    /// After finding items, wait this long and look again, so that answers and comments
    /// made in quick succession become one wake-up.
    pub debounce: Duration,
    /// How long a new waiter waits for the old one to step down.
    pub takeover_limit: Duration,
    /// Connection time limit.
    pub connect_timeout: Duration,
}

impl WaitOptions {
    pub fn new(session_key: impl Into<String>, claude_pid: Option<u32>) -> WaitOptions {
        WaitOptions {
            session_key: session_key.into(),
            claude_pid,
            max_wait: Duration::from_secs(23 * 3600),
            alive_every: Duration::from_secs(2),
            poll_every: Duration::from_secs(30),
            debounce: Duration::from_secs(1),
            takeover_limit: Duration::from_secs(10),
            connect_timeout: Duration::from_millis(3000),
        }
    }
}

/// Parses `CLAUDE_PID`; `None` when missing or not a positive number.
pub fn parse_pid(value: Option<&str>) -> Option<u32> {
    value?.trim().parse::<u32>().ok().filter(|p| *p > 0)
}

/// The fixed end of the wake line (docs/board-v2.md section 19.2): the `prompt` hook puts
/// the content into the context of the turn the wake starts, so `kb inbox` is a fallback.
pub const WAKE_TAIL: &str =
    "The content is delivered in this turn's context; run \"kb inbox\" only if you do not see it.";

/// Items the waiter woke the session for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Announcement {
    /// `(question id, card number)` of answered questions, in answer order.
    pub answers: Vec<(i64, i32)>,
    /// `(note id, card number)` of comments, oldest first.
    pub comments: Vec<(i64, i32)>,
}

impl Announcement {
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.comments.is_empty()
    }

    /// The one line given to the agent, e.g.
    /// `[kb] question 7 answered; 1 new comment on card 12.` followed by [`WAKE_TAIL`]
    /// (docs/board-v2.md section 19.2).
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if !self.answers.is_empty() {
            let ids: Vec<String> = self.answers.iter().map(|(id, _)| id.to_string()).collect();
            let noun = if ids.len() == 1 {
                "question"
            } else {
                "questions"
            };
            parts.push(format!("{noun} {} answered", ids.join(", ")));
        }
        if !self.comments.is_empty() {
            let n = self.comments.len();
            let cards: BTreeSet<i32> = self.comments.iter().map(|(_, seq)| *seq).collect();
            let cards: Vec<String> = cards.iter().map(i32::to_string).collect();
            let noun = if n == 1 { "comment" } else { "comments" };
            let card_noun = if cards.len() == 1 { "card" } else { "cards" };
            parts.push(format!(
                "{n} new {noun} on {card_noun} {}",
                cards.join(", ")
            ));
        }
        format!("[kb] {}. {WAKE_TAIL}", parts.join("; "))
    }
}

/// Why the waiter ended. Only [`WaitEnd::Wake`] wakes the session (exit 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitEnd {
    /// At the start: the session has no board, or it is archived.
    NoBoard,
    /// At the start: a live waiter of the same Claude Code process already waits.
    Duplicate,
    /// At the start: another waiter holds the lock and did not step down in time.
    TakeoverFailed,
    /// The Claude Code process (`CLAUDE_PID`) is gone.
    ClaudeGone,
    /// The board was archived, or the session no longer has it.
    BoardClosed,
    /// A newer waiter took this session over.
    TakenOver,
    /// Waited `max_wait`.
    Deadline,
    /// The database connection (or the notification connection) was lost.
    Disconnected,
    /// New items: they are now marked as announced (never as delivered).
    Wake(Announcement),
}

/// Runs the waiter (docs/board-v2.md section 13.3) against the database at `url` (the
/// writer URL). Connection failures are `Err(Error::Unreachable)`; the caller exits 0.
pub async fn wait(url: &str, opts: &WaitOptions) -> Result<WaitEnd> {
    let mut conn = db::connect_timeout(url, opts.connect_timeout).await?;
    let key = opts.session_key.as_str();
    let Some(board) = read::board_by_session(&mut conn, key).await? else {
        return Ok(WaitEnd::NoBoard);
    };
    if board.status != BoardStatus::Active {
        return Ok(WaitEnd::NoBoard);
    }
    let token = new_token();
    let lock: i64 = sqlx::query_scalar("SELECT hashtextextended('kb_waiter:' || $1, 0)")
        .bind(key)
        .fetch_one(&mut conn)
        .await?;
    match acquire(&mut conn, opts, lock, &token).await? {
        Acquire::Acquired => {}
        Acquire::Duplicate => return Ok(WaitEnd::Duplicate),
        Acquire::Failed => {
            // The take-over request wrote our row; do not leave it behind (section 20.4).
            forget(&mut conn, key, &token).await;
            return Ok(WaitEnd::TakeoverFailed);
        }
    }
    let end = match listen(url, opts.connect_timeout).await {
        Ok(signals) => watch(&mut conn, opts, board.id, &token, signals).await,
        Err(e) => Err(e),
    };
    // Best effort: step down cleanly (the lock goes with the connection anyway). Every end
    // after the lock comes here: wake, board closed, deadline, Claude gone, disconnected.
    // A waiter that was taken over deletes nothing: the row is the new waiter's.
    forget(&mut conn, key, &token).await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock)
        .execute(&mut conn)
        .await;
    match end {
        Err(Error::Unreachable(_)) => Ok(WaitEnd::Disconnected),
        other => other,
    }
}

/// Deletes the session's `kb_waiter` row if it is still this waiter's (docs/board-v2.md
/// section 20.4). Best effort: errors are ignored.
async fn forget(conn: &mut PgConnection, key: &str, token: &str) {
    let _ = sqlx::query("DELETE FROM kb_waiter WHERE session_key = $1 AND token = $2")
        .bind(key)
        .bind(token)
        .execute(&mut *conn)
        .await;
}

/// Deletes the session's `kb_waiter` row whoever wrote it: the session ended
/// (`kb hook session-end`, docs/board-v2.md section 20.4). A waiter still running then
/// finds its token gone and ends at its next check.
pub async fn forget_session(conn: &mut PgConnection, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM kb_waiter WHERE session_key = $1")
        .bind(key)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

fn new_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos:x}", process::current_pid())
}

enum Acquire {
    Acquired,
    Duplicate,
    Failed,
}

/// Takes the session's lock (docs/board-v2.md section 13.4).
async fn acquire(
    conn: &mut PgConnection,
    opts: &WaitOptions,
    lock: i64,
    token: &str,
) -> Result<Acquire> {
    let key = opts.session_key.as_str();
    let mine = opts.claude_pid.map(i64::from);
    let start = Instant::now();
    let mut requested = false;
    loop {
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(lock)
            .fetch_one(&mut *conn)
            .await?;
        if got {
            record(conn, key, token, mine).await?;
            return Ok(Acquire::Acquired);
        }
        if !requested {
            let holder: Option<Option<i64>> =
                sqlx::query_scalar("SELECT claude_pid FROM kb_waiter WHERE session_key = $1")
                    .bind(key)
                    .fetch_optional(&mut *conn)
                    .await?;
            match holder {
                // A live waiter of the same Claude Code process: nothing to do.
                Some(pid) if pid == mine => return Ok(Acquire::Duplicate),
                Some(_) => {
                    // Another process: ask it to step down.
                    record(conn, key, token, mine).await?;
                    sqlx::query("SELECT pg_notify($1, $2)")
                        .bind(TAKEOVER_CHANNEL)
                        .bind(key)
                        .execute(&mut *conn)
                        .await?;
                    requested = true;
                }
                // The holder has only just started and not written its row yet.
                None if start.elapsed() < Duration::from_secs(1) => {}
                None => return Ok(Acquire::Duplicate),
            }
        }
        if start.elapsed() >= opts.takeover_limit {
            return Ok(Acquire::Failed);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Writes this waiter as the session's waiter.
async fn record(
    conn: &mut PgConnection,
    key: &str,
    token: &str,
    claude: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO kb_waiter (session_key, token, waiter_pid, claude_pid, started_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (session_key) DO UPDATE
            SET token = $2, waiter_pid = $3, claude_pid = $4, started_at = now()",
    )
    .bind(key)
    .bind(token)
    .bind(i64::from(process::current_pid()))
    .bind(claude)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// What the notification connection observed.
enum Signal {
    /// `kb_events`: a board changed.
    Board(i64),
    /// `kb_waiter`: a take-over request for a session.
    Waiter(String),
    /// The connection was lost.
    Lost,
}

/// A `LISTEN kb_events, kb_waiter` connection on a background task (receiving there keeps
/// the main loop's waiting cancel-safe).
async fn listen(url: &str, limit: Duration) -> Result<mpsc::UnboundedReceiver<Signal>> {
    let connect = async {
        let mut listener = PgListener::connect(url).await?;
        listener.eager_reconnect(false);
        listener
            .listen_all([events::CHANNEL, TAKEOVER_CHANNEL])
            .await?;
        Ok::<_, sqlx::Error>(listener)
    };
    let mut listener = match tokio::time::timeout(limit, connect).await {
        Ok(Ok(l)) => l,
        Ok(Err(e)) => return Err(Error::Unreachable(e.to_string())),
        Err(_) => {
            return Err(Error::Unreachable(
                "timed out connecting for LISTEN".to_string(),
            ));
        }
    };
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let signal = match listener.try_recv().await {
                Ok(Some(n)) if n.channel() == TAKEOVER_CHANNEL => {
                    Signal::Waiter(n.payload().to_string())
                }
                Ok(Some(n)) => match BoardEvent::parse(n.payload()) {
                    Some(ev) => Signal::Board(ev.board),
                    None => continue,
                },
                Ok(None) | Err(_) => Signal::Lost,
            };
            let lost = matches!(signal, Signal::Lost);
            if tx.send(signal).is_err() || lost {
                return;
            }
        }
    });
    Ok(rx)
}

/// The waiting loop, after the lock is held.
async fn watch(
    conn: &mut PgConnection,
    opts: &WaitOptions,
    board_id: i64,
    token: &str,
    mut signals: mpsc::UnboundedReceiver<Signal>,
) -> Result<WaitEnd> {
    let key = opts.session_key.as_str();
    let deadline = Instant::now() + opts.max_wait;
    let mut next_poll = Instant::now();
    let mut dirty = true;
    loop {
        if let Some(pid) = opts.claude_pid
            && !process::is_alive(pid)
        {
            return Ok(WaitEnd::ClaudeGone);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(WaitEnd::Deadline);
        }
        if dirty || now >= next_poll {
            dirty = false;
            next_poll = now + opts.poll_every;
            if let Some(end) = check(conn, opts, board_id, token).await? {
                return Ok(end);
            }
        }
        let until = next_poll.min(deadline);
        let tick = opts
            .alive_every
            .min(until.saturating_duration_since(Instant::now()));
        match tokio::time::timeout(tick, signals.recv()).await {
            Ok(Some(Signal::Board(id))) => dirty |= id == board_id,
            Ok(Some(Signal::Waiter(k))) => dirty |= k == key,
            Ok(Some(Signal::Lost)) | Ok(None) => return Ok(WaitEnd::Disconnected),
            Err(_) => {}
        }
    }
}

/// One look at the database: take-over, board state, new items.
async fn check(
    conn: &mut PgConnection,
    opts: &WaitOptions,
    board_id: i64,
    token: &str,
) -> Result<Option<WaitEnd>> {
    if let Some(end) = still_ours(conn, &opts.session_key, board_id, token).await? {
        return Ok(Some(end));
    }
    if pending(conn, board_id).await?.is_empty() {
        return Ok(None);
    }
    // Let answers and comments made together arrive, and let a hook that is delivering
    // them right now finish first.
    tokio::time::sleep(opts.debounce).await;
    if let Some(end) = still_ours(conn, &opts.session_key, board_id, token).await? {
        return Ok(Some(end));
    }
    let items = pending(conn, board_id).await?;
    if items.is_empty() {
        return Ok(None);
    }
    let questions: Vec<i64> = items.answers.iter().map(|(id, _)| *id).collect();
    let notes: Vec<i64> = items.comments.iter().map(|(id, _)| *id).collect();
    sqlx::query("UPDATE question SET announced_at = now() WHERE id = ANY($1)")
        .bind(&questions)
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE card_note SET announced_at = now() WHERE id = ANY($1)")
        .bind(&notes)
        .execute(&mut *conn)
        .await?;
    Ok(Some(WaitEnd::Wake(items)))
}

/// `TakenOver` when another waiter's token is in `kb_waiter`; `BoardClosed` when the
/// session's board is archived or no longer this board.
async fn still_ours(
    conn: &mut PgConnection,
    key: &str,
    board_id: i64,
    token: &str,
) -> Result<Option<WaitEnd>> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT token FROM kb_waiter WHERE session_key = $1")
            .bind(key)
            .fetch_optional(&mut *conn)
            .await?;
    if current.as_deref() != Some(token) {
        return Ok(Some(WaitEnd::TakenOver));
    }
    match read::board_by_session(conn, key).await? {
        Some(b) if b.id == board_id && b.status == BoardStatus::Active => Ok(None),
        _ => Ok(Some(WaitEnd::BoardClosed)),
    }
}

/// Undelivered items the session was not woken for yet (docs/board-v2.md section 13.3):
/// answered questions (not `chat`) and comments.
pub async fn pending(conn: &mut PgConnection, board_id: i64) -> Result<Announcement> {
    let answers: Vec<(i64, i32)> = sqlx::query_as(
        "SELECT q.id, c.seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.board_id = $1 AND q.status = 'answered'
           AND q.delivered_at IS NULL AND q.announced_at IS NULL
         ORDER BY q.answered_at, q.id",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?;
    let comments: Vec<(i64, i32)> = sqlx::query_as(
        "SELECT n.id, c.seq FROM card_note n JOIN card c ON c.id = n.card_id
         WHERE c.board_id = $1 AND n.kind = 'comment'
           AND n.delivered_at IS NULL AND n.announced_at IS NULL
         ORDER BY n.id",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Announcement { answers, comments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        let a = Announcement {
            answers: vec![(7, 3)],
            comments: vec![(31, 12)],
        };
        assert_eq!(
            a.message(),
            format!("[kb] question 7 answered; 1 new comment on card 12. {WAKE_TAIL}")
        );
        let a = Announcement {
            answers: vec![(7, 3), (9, 4)],
            comments: vec![],
        };
        assert_eq!(
            a.message(),
            format!("[kb] questions 7, 9 answered. {WAKE_TAIL}")
        );
        let a = Announcement {
            answers: vec![],
            comments: vec![(31, 14), (32, 12), (33, 12)],
        };
        assert_eq!(
            a.message(),
            format!("[kb] 3 new comments on cards 12, 14. {WAKE_TAIL}")
        );
        let a = Announcement {
            answers: vec![],
            comments: vec![(31, 12), (32, 12)],
        };
        assert_eq!(
            a.message(),
            format!("[kb] 2 new comments on card 12. {WAKE_TAIL}")
        );
        // One ASCII line that no longer tells the agent to run kb inbox unconditionally
        // (docs/board-v2.md section 19.2).
        let a = Announcement {
            answers: vec![(20, 4)],
            comments: vec![(46, 12)],
        };
        assert_eq!(
            a.message(),
            "[kb] question 20 answered; 1 new comment on card 12. The content is delivered in \
             this turn's context; run \"kb inbox\" only if you do not see it."
        );
        assert!(a.message().is_ascii() && !a.message().contains('\n'));
        assert!(!a.message().contains("Run: kb inbox"));
    }

    #[test]
    fn pids() {
        assert_eq!(parse_pid(Some(" 123 ")), Some(123));
        assert_eq!(parse_pid(Some("0")), None);
        assert_eq!(parse_pid(Some("x")), None);
        assert_eq!(parse_pid(None), None);
    }
}
