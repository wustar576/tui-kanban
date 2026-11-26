//! Write operations (docs/cli.md section 4).
//!
//! Every operation runs in one transaction through the same path:
//! [`begin`] locks the session's board (and rejects archived boards), the operation changes
//! data, and [`commit`] writes its activity rows, bumps `board.rev` and `updated_at`, and
//! commits. The `board` trigger then sends exactly one `kb_events` notification.

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnection, Postgres};
use sqlx::{Connection, Transaction};

use crate::error::{Error, Result};
use crate::model::{
    Board, BoardStatus, Card, CauseKind, CauseRef, Epic, Lane, LaneKind, LinkKind, NoteKind,
    Origin, Outcome, QuestionStatus, WorkspaceSource,
};
use crate::read::{CommentView, QuestionView};
use crate::resolve::{TerminalInfo, WorkspaceTarget};

/// Name of the lane `kb card drop` creates when a board has no `dropped` lane.
pub const DROPPED_LANE_NAME: &str = "捨棄";

/// Lanes `kb board init` creates when `--lanes` is not given.
pub const DEFAULT_LANES: &str = "待辦:todo,進行中:doing,完成:done";

/// Result of a write command.
#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub board_id: i64,
    pub session_key: String,
    /// `board.rev` after the write.
    pub rev: i64,
    /// One-line English summary, e.g. `card 8 added to 待辦`.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<i64>,
    /// Non-fatal problems, e.g. a WIP limit exceeded. The CLI prints them to stderr.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// A non-blocking suggestion; the CLI prints it as a last line `hint: <text>`
    /// (docs/board-v2.md section 18.5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The suggestion after a `decision` note or a drop without `--because`
/// (docs/board-v2.md sections 18.5 and 21.1), without the `hint: ` prefix.
pub const BECAUSE_HINT: &str = "if this follows from a card, a question, a comment or the user, \
     name it with --because <card> | q<id> | c<id> | user. Your own choice needs no --because.";

/// One activity row (docs/data-model.md section 2.7, docs/hooks.md section 7.2).
pub(crate) struct Change {
    pub(crate) card_id: Option<i64>,
    /// `agent` (default), `user` or `hook`.
    pub(crate) actor: &'static str,
    pub(crate) verb: &'static str,
    pub(crate) detail: Value,
}

impl Change {
    pub(crate) fn board(verb: &'static str, detail: Value) -> Self {
        Change {
            card_id: None,
            actor: "agent",
            verb,
            detail,
        }
    }
    pub(crate) fn card(card_id: i64, verb: &'static str, detail: Value) -> Self {
        Change {
            card_id: Some(card_id),
            actor: "agent",
            verb,
            detail,
        }
    }
    pub(crate) fn by(mut self, actor: &'static str) -> Self {
        self.actor = actor;
        self
    }
}

pub(crate) type Tx<'c> = Transaction<'c, Postgres>;

/// Inserts activity rows. Each row gets the board's current `turn_key`
/// (docs/hooks.md section 2.4), read inside the same transaction.
pub(crate) async fn insert_activity(
    conn: &mut PgConnection,
    board_id: i64,
    changes: &[Change],
) -> Result<()> {
    for c in changes {
        insert_one_activity(conn, board_id, c).await?;
    }
    Ok(())
}

/// Inserts one activity row and returns its id.
async fn insert_one_activity(conn: &mut PgConnection, board_id: i64, c: &Change) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO activity (board_id, card_id, actor, verb, detail, turn_key)
         VALUES ($1, $2, $3, $4, $5, (SELECT turn_key FROM board WHERE id = $1))
         RETURNING id",
    )
    .bind(board_id)
    .bind(c.card_id)
    .bind(c.actor)
    .bind(c.verb)
    .bind(sqlx::types::Json(&c.detail))
    .fetch_one(&mut *conn)
    .await?)
}

/// Starts a write: opens a transaction and locks the session's board.
pub(crate) async fn begin<'c>(
    conn: &'c mut PgConnection,
    session_key: &str,
    allow_archived: bool,
) -> Result<(Tx<'c>, Board)> {
    let mut tx = conn.begin().await?;
    let board = sqlx::query_as::<_, Board>("SELECT * FROM board WHERE session_key = $1 FOR UPDATE")
        .bind(session_key)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::NoBoard(session_key.to_string()))?;
    if board.status == BoardStatus::Archived && !allow_archived {
        return Err(Error::BoardArchived);
    }
    Ok((tx, board))
}

/// Finishes a write: activity rows, `rev + 1`, `updated_at`, commit.
pub(crate) async fn commit(
    mut tx: Tx<'_>,
    board: &Board,
    changes: Vec<Change>,
    message: String,
) -> Result<WriteOutcome> {
    insert_activity(&mut tx, board.id, &changes).await?;
    let rev: i64 = sqlx::query_scalar(
        "UPDATE board SET rev = rev + 1, updated_at = now() WHERE id = $1 RETURNING rev",
    )
    .bind(board.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome {
        board_id: board.id,
        session_key: board.session_key.clone(),
        rev,
        message,
        card: None,
        link: None,
        question: None,
        warnings: Vec::new(),
        hint: None,
    })
}

// ---------------------------------------------------------------------------------------
// The `next:` echo (docs/board-v2.md section 15.2).

/// Remembers `line` as the last `next:` line shown for the board. True when it differs
/// from the remembered one (or none was remembered), i.e. when the caller must print it.
///
/// One statement compares and writes, so of two concurrent commands computing the same
/// line only one sees a change (the second `UPDATE` waits for the first and re-checks the
/// condition on the committed row). Never touches `rev`, `updated_at` or the activity, so
/// the `kb_events` trigger does not fire.
pub async fn record_next(conn: &mut PgConnection, board_id: i64, line: &str) -> Result<bool> {
    let changed = sqlx::query(
        "UPDATE board SET last_next = $2 WHERE id = $1 AND last_next IS DISTINCT FROM $2",
    )
    .bind(board_id)
    .bind(line)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(changed > 0)
}

/// Forgets the last `next:` line: the agent's context received the board again without
/// it (session-start, automatic adoption, `kb board start`, `kb board adopt`), so the next
/// write prints it. Same guarantees as [`record_next`]: no `rev`, no activity.
pub async fn forget_next(conn: &mut PgConnection, board_id: i64) -> Result<()> {
    sqlx::query("UPDATE board SET last_next = NULL WHERE id = $1 AND last_next IS NOT NULL")
        .bind(board_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Lookups used inside a write transaction.

pub(crate) async fn lanes_of(conn: &mut PgConnection, board_id: i64) -> Result<Vec<Lane>> {
    Ok(
        sqlx::query_as::<_, Lane>("SELECT * FROM lane WHERE board_id = $1 ORDER BY position")
            .bind(board_id)
            .fetch_all(&mut *conn)
            .await?,
    )
}

/// Finds a lane by its exact name; the error lists every lane of the board.
async fn lane_by_name(conn: &mut PgConnection, board_id: i64, name: &str) -> Result<Lane> {
    let lanes = lanes_of(conn, board_id).await?;
    let available = lanes.iter().map(|l| l.name.clone()).collect();
    lanes
        .into_iter()
        .find(|l| l.name == name)
        .ok_or_else(|| Error::LaneNotFound {
            name: name.to_string(),
            available,
        })
}

async fn lane_by_id(conn: &mut PgConnection, lane_id: i64) -> Result<Lane> {
    Ok(
        sqlx::query_as::<_, Lane>("SELECT * FROM lane WHERE id = $1")
            .bind(lane_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// Leftmost lane of `kind`, if any.
async fn first_lane_of_kind(
    conn: &mut PgConnection,
    board_id: i64,
    kind: LaneKind,
) -> Result<Option<Lane>> {
    Ok(sqlx::query_as::<_, Lane>(
        "SELECT * FROM lane WHERE board_id = $1 AND kind = $2 ORDER BY position LIMIT 1",
    )
    .bind(board_id)
    .bind(kind)
    .fetch_optional(&mut *conn)
    .await?)
}

pub(crate) async fn card_by_seq(conn: &mut PgConnection, board_id: i64, seq: i32) -> Result<Card> {
    sqlx::query_as::<_, Card>("SELECT * FROM card WHERE board_id = $1 AND seq = $2 FOR UPDATE")
        .bind(board_id)
        .bind(seq)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(Error::CardNotFound(seq))
}

/// The head of a todo lane's queue (docs/board-v2.md sections 10.2 and 17.1): the number of
/// the first card, by `position` then number, that is ready: not under discussion and not
/// blocked (no blocked reason, and no unfinished card blocking it through a `blocks` link).
/// `None` when no card of the lane is ready.
async fn queue_head(conn: &mut PgConnection, lane_id: i64) -> Result<Option<i32>> {
    Ok(sqlx::query_scalar(
        "SELECT c.seq FROM card c
         WHERE c.lane_id = $1 AND c.blocked_reason IS NULL AND c.origin <> 'discussing'
           AND NOT EXISTS (SELECT 1 FROM link k
                           JOIN card f ON f.id = k.from_card_id
                           JOIN lane fl ON fl.id = f.lane_id
                           WHERE k.to_card_id = c.id AND k.kind = 'blocks'
                             AND fl.kind NOT IN ('done', 'dropped'))
         ORDER BY c.position, c.seq
         LIMIT 1",
    )
    .bind(lane_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Position for a card entering `lane_id`: current maximum + 1.
async fn next_card_position(conn: &mut PgConnection, lane_id: i64) -> Result<i32> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(max(position), 0) + 1 FROM card WHERE lane_id = $1")
            .bind(lane_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// Position for a card entering `lane_id` at the front: current minimum - 1 (positions
/// may go below 1; only their order matters, and `kb board tidy` renumbers them).
async fn front_card_position(conn: &mut PgConnection, lane_id: i64) -> Result<i32> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(min(position), 1) - 1 FROM card WHERE lane_id = $1")
            .bind(lane_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

async fn next_lane_position(conn: &mut PgConnection, board_id: i64) -> Result<i32> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(max(position), 0) + 1 FROM lane WHERE board_id = $1")
            .bind(board_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

async fn insert_lane(
    conn: &mut PgConnection,
    board_id: i64,
    name: &str,
    position: i32,
    kind: LaneKind,
    wip_limit: Option<i32>,
) -> Result<Lane> {
    Ok(sqlx::query_as::<_, Lane>(
        "INSERT INTO lane (board_id, name, position, kind, wip_limit)
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(board_id)
    .bind(name)
    .bind(position)
    .bind(kind)
    .bind(wip_limit)
    .fetch_one(&mut *conn)
    .await?)
}

async fn insert_note(
    conn: &mut PgConnection,
    card_id: i64,
    kind: NoteKind,
    body: &str,
) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO card_note (card_id, kind, body) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(card_id)
    .bind(kind)
    .bind(body)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query("UPDATE card SET updated_at = now() WHERE id = $1")
        .bind(card_id)
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------------------
// Causes of a decision (docs/board-v2.md section 18).

/// A `--because` reference checked against the board.
#[derive(Debug, Clone)]
struct ResolvedCause {
    kind: CauseKind,
    card_id: Option<i64>,
    question_id: Option<i64>,
    note_id: Option<i64>,
    /// Number of the cited card, or of the card holding the question or comment.
    seq: Option<i32>,
}

impl ResolvedCause {
    /// One element of an activity's `because` array (section 18.3).
    fn detail(&self) -> Value {
        let mut v = json!({ "kind": self.kind });
        if let Some(id) = self.question_id.or(self.note_id) {
            v["id"] = json!(id);
        }
        if let Some(seq) = self.seq {
            v["seq"] = json!(seq);
        }
        v
    }
}

/// Checks `refs` in order against the board; the first bad one fails the write (section
/// 18.2). Duplicates are dropped. `decider` is the number of the deciding card.
async fn resolve_causes(
    conn: &mut PgConnection,
    board_id: i64,
    decider: i32,
    refs: &[CauseRef],
) -> Result<Vec<ResolvedCause>> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for r in refs {
        if seen.contains(r) {
            continue;
        }
        seen.push(*r);
        let fail = |what: String| Error::invalid(format!("--because {r}: {what}"));
        let resolved = match *r {
            CauseRef::User => ResolvedCause {
                kind: CauseKind::User,
                card_id: None,
                question_id: None,
                note_id: None,
                seq: None,
            },
            CauseRef::Card(seq) => {
                if seq == decider {
                    return Err(fail(
                        "a decision cannot follow from its own card; name a question (q<id>) \
                         or a comment (c<id>) on it instead"
                            .to_string(),
                    ));
                }
                let id: Option<i64> =
                    sqlx::query_scalar("SELECT id FROM card WHERE board_id = $1 AND seq = $2")
                        .bind(board_id)
                        .bind(seq)
                        .fetch_optional(&mut *conn)
                        .await?;
                let id = id.ok_or_else(|| fail(format!("card {seq} not found on this board")))?;
                ResolvedCause {
                    kind: CauseKind::Card,
                    card_id: Some(id),
                    question_id: None,
                    note_id: None,
                    seq: Some(seq),
                }
            }
            CauseRef::Question(qid) => {
                let seq: Option<i32> = sqlx::query_scalar(
                    "SELECT c.seq FROM question q JOIN card c ON c.id = q.card_id
                     WHERE q.id = $1 AND q.board_id = $2",
                )
                .bind(qid)
                .bind(board_id)
                .fetch_optional(&mut *conn)
                .await?;
                let seq =
                    seq.ok_or_else(|| fail(format!("question {qid} not found on this board")))?;
                ResolvedCause {
                    kind: CauseKind::Question,
                    card_id: None,
                    question_id: Some(qid),
                    note_id: None,
                    seq: Some(seq),
                }
            }
            CauseRef::Comment(nid) => {
                let row: Option<(NoteKind, i32)> = sqlx::query_as(
                    "SELECT n.kind, c.seq FROM card_note n JOIN card c ON c.id = n.card_id
                     WHERE n.id = $1 AND c.board_id = $2",
                )
                .bind(nid)
                .bind(board_id)
                .fetch_optional(&mut *conn)
                .await?;
                let (kind, seq) =
                    row.ok_or_else(|| fail(format!("comment {nid} not found on this board")))?;
                if kind != NoteKind::Comment {
                    return Err(fail(format!("note {nid} is a {kind} note, not a comment")));
                }
                ResolvedCause {
                    kind: CauseKind::Comment,
                    card_id: None,
                    question_id: None,
                    note_id: Some(nid),
                    seq: Some(seq),
                }
            }
        };
        out.push(resolved);
    }
    Ok(out)
}

/// Inserts the decision's activity row (the last of `changes` is the decision; the others
/// are written first, in order) and one `cause` row per cause. Returns nothing: the caller
/// commits with no further changes.
async fn insert_decision(
    conn: &mut PgConnection,
    board_id: i64,
    card_id: i64,
    note_id: Option<i64>,
    changes: Vec<Change>,
    causes: &[ResolvedCause],
) -> Result<()> {
    let mut decision = None;
    for c in &changes {
        decision = Some(insert_one_activity(conn, board_id, c).await?);
    }
    let activity_id = decision.expect("a decision has an activity row");
    for c in causes {
        sqlx::query(
            "INSERT INTO cause (board_id, card_id, activity_id, note_id, kind,
                                cause_card_id, cause_question_id, cause_note_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(board_id)
        .bind(card_id)
        .bind(activity_id)
        .bind(note_id)
        .bind(c.kind)
        .bind(c.card_id)
        .bind(c.question_id)
        .bind(c.note_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Adds `because` to an activity detail when there are causes.
fn with_because(mut detail: Value, causes: &[ResolvedCause]) -> Value {
    if !causes.is_empty() {
        detail["because"] = Value::Array(causes.iter().map(ResolvedCause::detail).collect());
    }
    detail
}

async fn epics_of(conn: &mut PgConnection, board_id: i64) -> Result<Vec<Epic>> {
    Ok(
        sqlx::query_as::<_, Epic>("SELECT * FROM epic WHERE board_id = $1 ORDER BY position")
            .bind(board_id)
            .fetch_all(&mut *conn)
            .await?,
    )
}

/// Finds an epic by its exact name; the error lists every epic of the board.
async fn epic_by_name(conn: &mut PgConnection, board_id: i64, name: &str) -> Result<Epic> {
    let epics = epics_of(conn, board_id).await?;
    let available = epics.iter().map(|e| e.name.clone()).collect();
    epics
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| Error::EpicNotFound {
            name: name.to_string(),
            available,
        })
}

/// Name of epic `id`, `None` for no epic (the `from`/`to` of `card.epic`).
async fn epic_name(conn: &mut PgConnection, id: Option<i64>) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM epic WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?)
}

/// Appends an epic at the end of the board's epics.
async fn insert_epic(conn: &mut PgConnection, board_id: i64, name: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO epic (board_id, name, position)
         VALUES ($1, $2, (SELECT coalesce(max(position), 0) + 1 FROM epic WHERE board_id = $1))",
    )
    .bind(board_id)
    .bind(name)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// First 80 characters of a text, for `excerpt` fields of activity.
pub(crate) fn excerpt(text: &str) -> String {
    text.chars().take(80).collect()
}

fn require_text(what: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::invalid(format!("{what} must not be empty")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Board

/// A lane in a `--lanes` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneSpec {
    pub name: String,
    pub kind: LaneKind,
}

/// Parses `名稱:類別,名稱:類別` and checks it: unique non-empty names, at least one `todo`
/// lane and one `done` lane.
pub fn parse_lanes(spec: &str) -> Result<Vec<LaneSpec>> {
    let mut lanes = Vec::new();
    for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, kind) = item.rsplit_once(':').ok_or_else(|| {
            Error::invalid(format!(
                "invalid lane {item:?}: expected name:kind, e.g. 待辦:todo"
            ))
        })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::invalid(format!("invalid lane {item:?}: empty name")));
        }
        let kind: LaneKind = kind.trim().parse()?;
        if lanes.iter().any(|l: &LaneSpec| l.name == name) {
            return Err(Error::invalid(format!("duplicate lane name: {name}")));
        }
        lanes.push(LaneSpec {
            name: name.to_string(),
            kind,
        });
    }
    if !lanes.iter().any(|l| l.kind == LaneKind::Todo)
        || !lanes.iter().any(|l| l.kind == LaneKind::Done)
    {
        return Err(Error::invalid(
            "lanes must include at least one todo lane and one done lane",
        ));
    }
    Ok(lanes)
}

/// Parses `名稱,名稱,...` (`kb board init --epics`): unique non-empty names, in order.
pub fn parse_epics(spec: &str) -> Result<Vec<String>> {
    let mut epics: Vec<String> = Vec::new();
    for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if epics.iter().any(|e| e == name) {
            return Err(Error::invalid(format!("duplicate epic name: {name}")));
        }
        epics.push(name.to_string());
    }
    if epics.is_empty() {
        return Err(Error::invalid("--epics needs at least one name"));
    }
    Ok(epics)
}

/// Arguments of `kb board init`.
#[derive(Debug, Clone, Default)]
pub struct BoardInit {
    /// `None` keeps the title of an existing board that has one; a new board, or one whose
    /// title is empty, needs it (docs/board-v2.md section 20.1).
    pub title: Option<String>,
    /// `None` keeps the current goal of an existing board (empty for a new one).
    pub goal: Option<String>,
    /// Parsed `--lanes`; `None` when not given.
    pub lanes: Option<Vec<LaneSpec>>,
    /// Parsed `--epics`: created in order after the board's existing epics. Names the board
    /// already has are skipped.
    pub epics: Option<Vec<String>>,
    /// Working directory stored on a new board.
    pub cwd: Option<String>,
}

/// Creates the epics of `--epics` that the board does not have yet; one `epic.add` each.
async fn init_epics(
    conn: &mut PgConnection,
    board_id: i64,
    names: Option<&[String]>,
    changes: &mut Vec<Change>,
) -> Result<usize> {
    let existing = epics_of(conn, board_id).await?;
    let mut created = 0;
    for name in names.unwrap_or_default() {
        if existing.iter().any(|e| &e.name == name) {
            continue;
        }
        insert_epic(conn, board_id, name).await?;
        changes.push(Change::board("epic.add", json!({ "epic": name })));
        created += 1;
    }
    Ok(created)
}

/// `, N epics created` when N > 0.
fn epics_created(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(", {n} epics created")
    }
}

/// `kb board init` (docs/cli.md section 4.3). `resolve_workspace` is called only when the
/// session has no board yet, so the workspace is resolved only for new boards.
pub async fn board_init(
    conn: &mut PgConnection,
    session_key: &str,
    args: BoardInit,
    resolve_workspace: impl FnOnce() -> WorkspaceTarget,
) -> Result<WriteOutcome> {
    if let Some(title) = &args.title {
        require_text("title", title)?;
    }
    let existing = crate::read::board_by_session(conn, session_key).await?;
    let Some(_) = existing else {
        let Some(title) = args.title.clone() else {
            return Err(Error::invalid(TITLE_REQUIRED));
        };
        return board_create(conn, session_key, title, args, resolve_workspace()).await;
    };

    let (mut tx, board) = begin(conn, session_key, false).await?;
    // Without --title the board keeps its title, if it has one (docs/board-v2.md 20.1).
    let title = match args.title {
        Some(title) => title,
        None if !board.title.trim().is_empty() => board.title.clone(),
        None => return Err(Error::invalid(TITLE_REQUIRED)),
    };
    let lane_count: i64 = sqlx::query_scalar("SELECT count(*) FROM lane WHERE board_id = $1")
        .bind(board.id)
        .fetch_one(&mut *tx)
        .await?;
    if lane_count > 0 && args.lanes.is_some() {
        return Err(Error::invalid(
            "board already has lanes: use \"kb lane\" commands",
        ));
    }
    sqlx::query("UPDATE board SET title = $2, goal = coalesce($3, goal) WHERE id = $1")
        .bind(board.id)
        .bind(&title)
        .bind(&args.goal)
        .execute(&mut *tx)
        .await?;
    let mut message = "board updated".to_string();
    if lane_count == 0 {
        let lanes = match args.lanes {
            Some(lanes) => lanes,
            None => parse_lanes(DEFAULT_LANES)?,
        };
        for (i, l) in lanes.iter().enumerate() {
            insert_lane(&mut tx, board.id, &l.name, i as i32 + 1, l.kind, None).await?;
        }
        message = format!("board updated, {} lanes created", lanes.len());
    }
    // The goal only when it changed, so the feed can show it (docs/board-v2.md 20.2).
    let mut detail = json!({ "title": title });
    if let Some(goal) = args.goal.as_deref().filter(|g| *g != board.goal) {
        detail["goal"] = json!(goal);
    }
    let mut changes = vec![Change::board("board.update", detail)];
    let created = init_epics(&mut tx, board.id, args.epics.as_deref(), &mut changes).await?;
    message.push_str(&epics_created(created));
    commit(tx, &board, changes, message).await
}

/// `kb board init` without `--title` when there is no title to keep (docs/board-v2.md 20.1).
pub const TITLE_REQUIRED: &str = "--title is required: the board has no title yet";

async fn board_create(
    conn: &mut PgConnection,
    session_key: &str,
    title: String,
    args: BoardInit,
    ws: WorkspaceTarget,
) -> Result<WriteOutcome> {
    let lanes = match args.lanes {
        Some(lanes) => lanes,
        None => parse_lanes(DEFAULT_LANES)?,
    };
    let mut tx = conn.begin().await?;
    let board = insert_board(
        &mut tx,
        session_key,
        &title,
        args.goal.as_deref(),
        &ws,
        args.cwd.as_deref(),
    )
    .await?;
    for (i, l) in lanes.iter().enumerate() {
        insert_lane(&mut tx, board.id, &l.name, i as i32 + 1, l.kind, None).await?;
    }
    let mut changes = vec![Change::board("board.create", json!({ "title": title }))];
    let created = init_epics(&mut tx, board.id, args.epics.as_deref(), &mut changes).await?;
    let message = format!(
        "board created for session {session_key} in workspace {} ({} lanes{})",
        ws.name,
        lanes.len(),
        epics_created(created)
    );
    commit(tx, &board, changes, message).await
}

/// Creates the workspace `key`, or renames the existing one to `name` (docs/cli.md section
/// 2.4). Returns its id.
pub(crate) async fn upsert_workspace(
    tx: &mut PgConnection,
    key: &str,
    name: &str,
    source: WorkspaceSource,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO workspace (key, name, source) VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE
           SET name = EXCLUDED.name,
               updated_at = CASE WHEN workspace.name = EXCLUDED.name
                                 THEN workspace.updated_at ELSE now() END
         RETURNING id",
    )
    .bind(key)
    .bind(name)
    .bind(source)
    .fetch_one(&mut *tx)
    .await?)
}

/// Upserts the workspace of `ws` and inserts a board without lanes.
pub(crate) async fn insert_board(
    tx: &mut PgConnection,
    session_key: &str,
    title: &str,
    goal: Option<&str>,
    ws: &WorkspaceTarget,
    cwd: Option<&str>,
) -> Result<Board> {
    let workspace_id = upsert_workspace(tx, &ws.key, &ws.name, ws.source).await?;
    // The terminal fields come from the current terminal whatever the workspace source
    // (docs/board-v2.md section 12.7).
    let terminal = ws.terminal.as_ref();
    Ok(sqlx::query_as::<_, Board>(
        "INSERT INTO board (workspace_id, session_key, title, goal, terminal_space, terminal_tab,
                            terminal_id, cwd)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(workspace_id)
    .bind(session_key)
    .bind(title)
    .bind(goal.unwrap_or(""))
    .bind(terminal.and_then(|t| t.space.as_deref()))
    .bind(terminal.and_then(|t| t.tab.as_deref()))
    .bind(terminal.map(|t| t.terminal.as_str()))
    .bind(cwd)
    .fetch_one(&mut *tx)
    .await?)
}

/// Sets the board's terminal fields to the current terminal (docs/board-v2.md section
/// 12.7). Does nothing when the terminal is unknown.
pub(crate) async fn refresh_terminal(
    tx: &mut PgConnection,
    board_id: i64,
    terminal: Option<&TerminalInfo>,
) -> Result<()> {
    let Some(t) = terminal else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE board SET terminal_id = $2, terminal_space = $3, terminal_tab = $4 WHERE id = $1",
    )
    .bind(board_id)
    .bind(&t.terminal)
    .bind(t.space.as_deref())
    .bind(t.tab.as_deref())
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// What `kb board terminal --id` records (docs/board-v2.md section 26.3): values trimmed;
/// a blank `--space`, `--tab` or `--workspace-name` counts as not given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardTerminal {
    pub id: String,
    pub space: Option<String>,
    pub tab: Option<String>,
    /// Only with a space.
    pub workspace_name: Option<String>,
}

/// `kb board terminal` with a blank `--id`.
pub const TERMINAL_ID_BLANK: &str = "--id must not be blank";

impl BoardTerminal {
    /// Normalizes the options; fails when `id` is blank.
    pub fn new(
        id: &str,
        space: Option<&str>,
        tab: Option<&str>,
        workspace_name: Option<&str>,
    ) -> Result<Self> {
        let text = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(String::from);
        let id = text(Some(id)).ok_or_else(|| Error::invalid(TERMINAL_ID_BLANK))?;
        let space = text(space);
        let workspace_name = space.as_ref().and(text(workspace_name));
        Ok(BoardTerminal {
            id,
            space,
            tab: text(tab),
            workspace_name,
        })
    }
}

/// `kb board terminal --id` (docs/board-v2.md section 26.3): writes the three terminal
/// fields; with a space, moves the board to the workspace `space:<id>`. Nothing changes
/// (no activity, no `rev`) when every value is already the board's.
pub async fn board_terminal(
    conn: &mut PgConnection,
    session_key: &str,
    args: BoardTerminal,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let fields_same = board.terminal_id.as_deref() == Some(args.id.as_str())
        && board.terminal_space == args.space
        && board.terminal_tab == args.tab;
    // The workspace: (name, moved, renamed).
    let mut workspace: Option<(String, bool, bool)> = None;
    if let Some(space) = &args.space {
        let key = format!("space:{space}");
        let before: Option<String> =
            sqlx::query_scalar("SELECT name FROM workspace WHERE key = $1")
                .bind(&key)
                .fetch_optional(&mut *tx)
                .await?;
        let name = args
            .workspace_name
            .clone()
            .or_else(|| before.clone())
            .unwrap_or_else(|| space.clone());
        let renamed = before.as_ref().is_some_and(|b| *b != name);
        let workspace_id = upsert_workspace(&mut tx, &key, &name, WorkspaceSource::Space).await?;
        let moved = workspace_id != board.workspace_id;
        if moved {
            sqlx::query("UPDATE board SET workspace_id = $2 WHERE id = $1")
                .bind(board.id)
                .bind(workspace_id)
                .execute(&mut *tx)
                .await?;
        }
        workspace = Some((name, moved, renamed));
    }
    let ws_changed = workspace.as_ref().is_some_and(|(_, m, r)| *m || *r);
    if fields_same && !ws_changed {
        tx.rollback().await?;
        return Ok(unchanged(&board, "terminal unchanged".into()));
    }
    let info = TerminalInfo {
        terminal: args.id.clone(),
        space: args.space.clone(),
        tab: args.tab.clone(),
    };
    refresh_terminal(&mut tx, board.id, Some(&info)).await?;
    let mut detail = json!({ "terminal": args.id });
    if let Some(space) = &args.space {
        detail["space"] = json!(space);
    }
    if let Some(tab) = &args.tab {
        detail["tab"] = json!(tab);
    }
    let mut message = format!("terminal {} recorded for board {}", args.id, board.id);
    if let Some((name, moved, renamed)) = &workspace {
        if *moved || *renamed {
            detail["workspace"] = json!(name);
        }
        if *moved {
            message.push_str(&format!(", moved to workspace {name}"));
        } else if *renamed {
            message.push_str(&format!(", workspace renamed to {name}"));
        }
    }
    commit(
        tx,
        &board,
        vec![Change::board("board.terminal", detail)],
        message,
    )
    .await
}

// ---------------------------------------------------------------------------------------
// Manual start and adoption (docs/board-v2.md sections 12.4 and 12.5)

/// Arguments of `kb board start`.
#[derive(Debug, Clone, Default)]
pub struct BoardStartArgs {
    /// Title of a new board (empty when not given). Ignored for an existing board.
    pub title: Option<String>,
    /// Goal of a new board. Ignored for an existing board.
    pub goal: Option<String>,
    /// Create a new board even when there are predecessor candidates.
    pub new: bool,
    /// Working directory: stored on a new board and matched for candidates.
    pub cwd: Option<String>,
}

/// What `kb board start` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StartStatus {
    /// The session's board was already active; nothing changed.
    Active,
    /// The session's archived board was reopened.
    Reopened,
    /// A new board was created.
    Created,
    /// No board was created: predecessor candidates were listed instead.
    Candidates,
}

/// Result of `kb board start`.
#[derive(Debug, Clone, Serialize)]
pub struct BoardStart {
    pub status: StartStatus,
    pub session_key: String,
    /// The session's board after the command; `None` with [`StartStatus::Candidates`].
    pub board: Option<Board>,
    /// Possible predecessors, most recently changed first (at most 3).
    pub candidates: Vec<Board>,
}

/// Generates a turn key: for `kb board start`, and for `UserPromptSubmit` without a
/// `prompt_id` (docs/hooks.md section 2.4).
pub(crate) fn generated_turn_key() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("kb-{nanos:x}-{:x}", std::process::id())
}

/// `kb board start` starts the turn of the `/kb` prompt (docs/board-v2.md section 12.4): a
/// new `turn_key`, `turn_started_at = now()`, and the returned `user.prompt` change with
/// `detail.source = "board.start"`. The caller writes it as the transaction's last row, after
/// `board.create` or `board.reopen`.
async fn start_turn_at_board_start(tx: &mut PgConnection, board_id: i64) -> Result<Change> {
    sqlx::query("UPDATE board SET turn_key = $2, turn_started_at = now() WHERE id = $1")
        .bind(board_id)
        .bind(generated_turn_key())
        .execute(&mut *tx)
        .await?;
    Ok(Change::board(
        "user.prompt",
        json!({ "excerpt": "kb board start", "source": crate::rules::BOARD_START_SOURCE }),
    ))
}

/// Number of predecessor candidates `kb board start` lists.
pub const START_CANDIDATES: i64 = 3;

/// Boards that may belong to an earlier session of the same conversation: same terminal
/// when it is known, else same `cwd`; active, or archived within 7 days; most recently
/// changed first (docs/board-v2.md section 12.4).
pub async fn start_candidates(
    conn: &mut PgConnection,
    session_key: &str,
    terminal: Option<&str>,
    cwd: Option<&str>,
) -> Result<Vec<Board>> {
    Ok(sqlx::query_as::<_, Board>(
        "SELECT * FROM board
         WHERE session_key <> $1
           AND CASE WHEN $2::text IS NOT NULL THEN terminal_id = $2 ELSE cwd = $3 END
           AND (status = 'active' OR archived_at >= now() - interval '7 days')
         ORDER BY updated_at DESC, id DESC
         LIMIT $4",
    )
    .bind(session_key)
    .bind(terminal)
    .bind(cwd)
    .bind(START_CANDIDATES)
    .fetch_all(&mut *conn)
    .await?)
}

/// `kb board start` (docs/board-v2.md section 12.4). `terminal` is the current terminal;
/// `resolve_workspace` runs only when a board is created.
pub async fn board_start(
    conn: &mut PgConnection,
    session_key: &str,
    args: BoardStartArgs,
    terminal: Option<&TerminalInfo>,
    resolve_workspace: impl FnOnce() -> WorkspaceTarget,
) -> Result<BoardStart> {
    let mut tx = conn.begin().await?;
    let existing =
        sqlx::query_as::<_, Board>("SELECT * FROM board WHERE session_key = $1 FOR UPDATE")
            .bind(session_key)
            .fetch_optional(&mut *tx)
            .await?;
    let status = match existing {
        Some(board) if board.status == BoardStatus::Active => {
            tx.rollback().await?;
            return Ok(BoardStart {
                status: StartStatus::Active,
                session_key: session_key.to_string(),
                board: Some(board),
                candidates: Vec::new(),
            });
        }
        Some(board) => {
            reopen_in_tx(&mut tx, board.id).await?;
            refresh_terminal(&mut tx, board.id, terminal).await?;
            let turn = start_turn_at_board_start(&mut tx, board.id).await?;
            commit(
                tx,
                &board,
                vec![Change::board("board.reopen", json!({})), turn],
                String::new(),
            )
            .await?;
            StartStatus::Reopened
        }
        None => {
            let candidates = start_candidates(
                &mut tx,
                session_key,
                terminal.map(|t| t.terminal.as_str()),
                args.cwd.as_deref(),
            )
            .await?;
            if !candidates.is_empty() && !args.new {
                tx.rollback().await?;
                return Ok(BoardStart {
                    status: StartStatus::Candidates,
                    session_key: session_key.to_string(),
                    board: None,
                    candidates,
                });
            }
            let mut ws = resolve_workspace();
            if terminal.is_some() {
                ws.terminal = terminal.cloned();
            }
            let title = args.title.unwrap_or_default();
            let board = insert_board(
                &mut tx,
                session_key,
                &title,
                args.goal.as_deref(),
                &ws,
                args.cwd.as_deref(),
            )
            .await?;
            if let Some(prev) = candidates.first() {
                sqlx::query("UPDATE board SET prev_board_id = $2 WHERE id = $1")
                    .bind(board.id)
                    .bind(prev.id)
                    .execute(&mut *tx)
                    .await?;
            }
            let turn = start_turn_at_board_start(&mut tx, board.id).await?;
            let changes = vec![
                Change::board("board.create", json!({ "title": title })),
                turn,
            ];
            commit(tx, &board, changes, String::new()).await?;
            StartStatus::Created
        }
    };
    let board = crate::read::board_by_session(conn, session_key)
        .await?
        .ok_or_else(|| Error::NoBoard(session_key.to_string()))?;
    Ok(BoardStart {
        status,
        session_key: session_key.to_string(),
        board: Some(board),
        candidates: Vec::new(),
    })
}

/// Result of an adoption.
#[derive(Debug, Clone, Serialize)]
pub struct Adoption {
    /// The board after the adoption (its `session_key` is the current session).
    pub board: Board,
    /// The session key the board had before.
    pub from: String,
    /// The board was archived and has been reopened.
    pub reopened: bool,
}

/// Finds the board `kb board adopt <target>` names: all digits → a board id first, then a
/// session key (docs/board-v2.md section 12.5).
pub async fn adopt_target(conn: &mut PgConnection, target: &str) -> Result<Option<Board>> {
    let target = target.trim();
    if !target.is_empty()
        && target.bytes().all(|b| b.is_ascii_digit())
        && let Ok(id) = target.parse::<i64>()
        && let Some(board) = crate::read::board_by_id(conn, id).await?
    {
        return Ok(Some(board));
    }
    crate::read::board_by_session(conn, target).await
}

/// `kb board adopt` (docs/board-v2.md section 12.5).
pub async fn board_adopt(
    conn: &mut PgConnection,
    session_key: &str,
    target: &str,
    terminal: Option<&TerminalInfo>,
) -> Result<Adoption> {
    if let Some(own) = crate::read::board_by_session(conn, session_key).await? {
        return Err(Error::AlreadyHasBoard {
            session: session_key.to_string(),
            board: own.id,
        });
    }
    let board = adopt_target(conn, target)
        .await?
        .ok_or_else(|| Error::BoardNotFound(target.trim().to_string()))?;
    adopt_board_id(conn, session_key, board.id, terminal, false).await
}

/// Binds board `board_id` to `session_key`: replaces its `session_key`, reopens it when
/// archived, refreshes the terminal fields and records `board.adopt` (`detail`: `from`,
/// `auto`; actor `hook` when `auto`). Fails when the session already has a board.
pub async fn adopt_board_id(
    conn: &mut PgConnection,
    session_key: &str,
    board_id: i64,
    terminal: Option<&TerminalInfo>,
    auto: bool,
) -> Result<Adoption> {
    let mut tx = conn.begin().await?;
    let own: Option<i64> = sqlx::query_scalar("SELECT id FROM board WHERE session_key = $1")
        .bind(session_key)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(own) = own {
        return Err(Error::AlreadyHasBoard {
            session: session_key.to_string(),
            board: own,
        });
    }
    let mut board = sqlx::query_as::<_, Board>("SELECT * FROM board WHERE id = $1 FOR UPDATE")
        .bind(board_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::BoardNotFound(board_id.to_string()))?;
    let from = std::mem::replace(&mut board.session_key, session_key.to_string());
    sqlx::query("UPDATE board SET session_key = $2 WHERE id = $1")
        .bind(board.id)
        .bind(session_key)
        .execute(&mut *tx)
        .await?;
    let actor = if auto { "hook" } else { "agent" };
    let mut changes =
        vec![Change::board("board.adopt", json!({ "from": from, "auto": auto })).by(actor)];
    let reopened = board.status == BoardStatus::Archived;
    if reopened {
        reopen_in_tx(&mut tx, board.id).await?;
        changes.push(Change::board("board.reopen", json!({})).by(actor));
    }
    refresh_terminal(&mut tx, board.id, terminal).await?;
    commit(tx, &board, changes, String::new()).await?;
    let board = crate::read::board_by_id(conn, board_id)
        .await?
        .ok_or_else(|| Error::BoardNotFound(board_id.to_string()))?;
    Ok(Adoption {
        board,
        from,
        reopened,
    })
}

/// `kb board archive`. Without `outcome`: `completed` when every card is in a `done` or
/// `dropped` lane, otherwise `incomplete`.
pub async fn board_archive(
    conn: &mut PgConnection,
    session_key: &str,
    outcome: Option<Outcome>,
    summary: Option<String>,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let outcome = match outcome {
        Some(o) => o,
        None => automatic_outcome(&mut tx, board.id).await?,
    };
    archive_in_tx(&mut tx, board.id, outcome, summary.as_deref()).await?;
    let changes = vec![Change::board(
        "board.archive",
        json!({ "outcome": outcome }),
    )];
    commit(tx, &board, changes, format!("board archived ({outcome})")).await
}

/// `completed` when every card is in a `done` or `dropped` lane (also when there are no
/// cards), otherwise `incomplete` (docs/cli.md section 4.6).
pub(crate) async fn automatic_outcome(conn: &mut PgConnection, board_id: i64) -> Result<Outcome> {
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM card c JOIN lane l ON l.id = c.lane_id
         WHERE c.board_id = $1 AND l.kind NOT IN ('done', 'dropped')",
    )
    .bind(board_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(if open == 0 {
        Outcome::Completed
    } else {
        Outcome::Incomplete
    })
}

pub(crate) async fn archive_in_tx(
    tx: &mut PgConnection,
    board_id: i64,
    outcome: Outcome,
    summary: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE board SET status = 'archived', outcome = $2, summary = coalesce($3, summary),
                          archived_at = now()
         WHERE id = $1",
    )
    .bind(board_id)
    .bind(outcome)
    .bind(summary)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// `kb board reopen`: status back to `active`; `outcome`, `archived_at` and `summary`
/// cleared (docs/board-v2.md section 12.6).
pub async fn board_reopen(conn: &mut PgConnection, session_key: &str) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, true).await?;
    if board.status != BoardStatus::Archived {
        return Err(Error::invalid("board is not archived"));
    }
    reopen_in_tx(&mut tx, board.id).await?;
    commit(
        tx,
        &board,
        vec![Change::board("board.reopen", json!({}))],
        "board reopened".into(),
    )
    .await
}

pub(crate) async fn reopen_in_tx(tx: &mut PgConnection, board_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE board SET status = 'active', outcome = NULL, archived_at = NULL, summary = NULL
         WHERE id = $1",
    )
    .bind(board_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// `kb board tidy` (docs/hooks.md section 5.2): renumbers card positions in every lane to
/// 1, 2, 3... keeping their order, and records `turn.summary`.
pub async fn board_tidy(conn: &mut PgConnection, session_key: &str) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let renumbered = sqlx::query(
        "UPDATE card c SET position = r.rn
         FROM (SELECT id, row_number() OVER (PARTITION BY lane_id ORDER BY position, id) AS rn
               FROM card WHERE board_id = $1) r
         WHERE c.id = r.id AND c.position <> r.rn",
    )
    .bind(board.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let summary = turn_summary(&mut tx, &board).await?;
    let changes = vec![Change::board("turn.summary", summary)];
    commit(
        tx,
        &board,
        changes,
        format!("board tidied: {renumbered} card positions renumbered"),
    )
    .await
}

/// `detail` of `turn.summary` (docs/hooks.md section 7.2) for the board's current turn as
/// the rules see it ([`crate::rules::TurnStart`]: everything after the latest start marker,
/// such as the `turn.summary` of `kb hook stop`): cards with activity of the agent (not the
/// deliveries of `kb inbox`), notes written and tool uses.
pub(crate) async fn turn_summary(conn: &mut PgConnection, board: &Board) -> Result<Value> {
    let start = crate::rules::turn_start(conn, board.id).await?;
    let (cards_changed, notes, tools): (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(DISTINCT card_id) FROM activity
             WHERE board_id = $1 AND id > $2 AND card_id IS NOT NULL AND actor = 'agent'
               AND verb NOT IN ('question.deliver', 'comment.deliver')),
           (SELECT count(*) FROM card_note n JOIN card c ON c.id = n.card_id
             WHERE c.board_id = $1 AND n.created_at > coalesce($3, '-infinity')),
           (SELECT count(*) FROM activity
             WHERE board_id = $1 AND id > $2 AND verb = 'tool.use')",
    )
    .bind(board.id)
    .bind(start.after_id)
    .bind(start.after_at)
    .fetch_one(&mut *conn)
    .await?;
    Ok(json!({ "cards_changed": cards_changed, "notes": notes, "tools": tools }))
}

// ---------------------------------------------------------------------------------------
// Lanes

/// `kb lane add`. Without `after`, the lane goes to the far right.
pub async fn lane_add(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
    kind: LaneKind,
    after: Option<&str>,
    wip_limit: Option<i32>,
) -> Result<WriteOutcome> {
    require_text("lane name", name)?;
    if let Some(w) = wip_limit
        && w < 1
    {
        return Err(Error::invalid(format!(
            "invalid WIP limit: {w} (must be at least 1)"
        )));
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let lanes = lanes_of(&mut tx, board.id).await?;
    if lanes.iter().any(|l| l.name == name) {
        return Err(Error::invalid(format!("lane already exists: {name}")));
    }
    let position = match after {
        Some(after) => {
            let after = lane_by_name(&mut tx, board.id, after).await?;
            sqlx::query(
                "UPDATE lane SET position = position + 1 WHERE board_id = $1 AND position > $2",
            )
            .bind(board.id)
            .bind(after.position)
            .execute(&mut *tx)
            .await?;
            after.position + 1
        }
        None => next_lane_position(&mut tx, board.id).await?,
    };
    insert_lane(&mut tx, board.id, name, position, kind, wip_limit).await?;
    let changes = vec![Change::board(
        "lane.add",
        json!({ "lane": name, "kind": kind }),
    )];
    commit(
        tx,
        &board,
        changes,
        format!("lane {name} added at position {position}"),
    )
    .await
}

/// `kb lane rename`.
pub async fn lane_rename(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
    new_name: &str,
) -> Result<WriteOutcome> {
    require_text("lane name", new_name)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let lane = lane_by_name(&mut tx, board.id, name).await?;
    if lanes_of(&mut tx, board.id)
        .await?
        .iter()
        .any(|l| l.name == new_name)
    {
        return Err(Error::invalid(format!("lane already exists: {new_name}")));
    }
    sqlx::query("UPDATE lane SET name = $2 WHERE id = $1")
        .bind(lane.id)
        .bind(new_name)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::board(
        "lane.rename",
        json!({ "from": name, "to": new_name }),
    )];
    commit(
        tx,
        &board,
        changes,
        format!("lane {name} renamed to {new_name}"),
    )
    .await
}

/// `kb lane move`: moves the lane to `position` (1-based); other lanes shift.
pub async fn lane_move(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
    position: i32,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let lane = lane_by_name(&mut tx, board.id, name).await?;
    let mut lanes = lanes_of(&mut tx, board.id).await?;
    if position < 1 || position as usize > lanes.len() {
        return Err(Error::invalid(format!(
            "invalid position: {position} (board has {} lanes)",
            lanes.len()
        )));
    }
    let from = lanes
        .iter()
        .position(|l| l.id == lane.id)
        .expect("lane is on board");
    let moved = lanes.remove(from);
    lanes.insert(position as usize - 1, moved);
    // lane_position_uniq is deferred, so transient duplicates are fine.
    for (i, l) in lanes.iter().enumerate() {
        if l.position != i as i32 + 1 {
            sqlx::query("UPDATE lane SET position = $2 WHERE id = $1")
                .bind(l.id)
                .bind(i as i32 + 1)
                .execute(&mut *tx)
                .await?;
        }
    }
    let changes = vec![Change::board(
        "lane.move",
        json!({ "lane": name, "position": position }),
    )];
    commit(
        tx,
        &board,
        changes,
        format!("lane {name} moved to position {position}"),
    )
    .await
}

/// `kb lane remove`: only an empty lane can be removed.
pub async fn lane_remove(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let lane = lane_by_name(&mut tx, board.id, name).await?;
    let cards: i64 = sqlx::query_scalar("SELECT count(*) FROM card WHERE lane_id = $1")
        .bind(lane.id)
        .fetch_one(&mut *tx)
        .await?;
    if cards > 0 {
        return Err(Error::invalid(format!(
            "lane is not empty: {name} has {cards} cards"
        )));
    }
    sqlx::query("DELETE FROM lane WHERE id = $1")
        .bind(lane.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE lane SET position = position - 1 WHERE board_id = $1 AND position > $2")
        .bind(board.id)
        .bind(lane.position)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::board("lane.remove", json!({ "lane": name }))];
    commit(tx, &board, changes, format!("lane {name} removed")).await
}

// ---------------------------------------------------------------------------------------
// Epics (docs/board-v2.md section 4.1)

/// `kb epic add`. Without `after`, the epic goes last.
pub async fn epic_add(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
    after: Option<&str>,
) -> Result<WriteOutcome> {
    require_text("epic name", name)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    if epics_of(&mut tx, board.id)
        .await?
        .iter()
        .any(|e| e.name == name)
    {
        return Err(Error::invalid(format!("epic already exists: {name}")));
    }
    let position = match after {
        Some(after) => {
            let after = epic_by_name(&mut tx, board.id, after).await?;
            sqlx::query(
                "UPDATE epic SET position = position + 1 WHERE board_id = $1 AND position > $2",
            )
            .bind(board.id)
            .bind(after.position)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO epic (board_id, name, position) VALUES ($1, $2, $3)")
                .bind(board.id)
                .bind(name)
                .bind(after.position + 1)
                .execute(&mut *tx)
                .await?;
            after.position + 1
        }
        None => {
            insert_epic(&mut tx, board.id, name).await?;
            epics_of(&mut tx, board.id).await?.len() as i32
        }
    };
    let changes = vec![Change::board("epic.add", json!({ "epic": name }))];
    commit(
        tx,
        &board,
        changes,
        format!("epic {name} added at position {position}"),
    )
    .await
}

/// `kb epic rename`.
pub async fn epic_rename(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
    new_name: &str,
) -> Result<WriteOutcome> {
    require_text("epic name", new_name)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let epic = epic_by_name(&mut tx, board.id, name).await?;
    if epics_of(&mut tx, board.id)
        .await?
        .iter()
        .any(|e| e.name == new_name)
    {
        return Err(Error::invalid(format!("epic already exists: {new_name}")));
    }
    sqlx::query("UPDATE epic SET name = $2 WHERE id = $1")
        .bind(epic.id)
        .bind(new_name)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::board(
        "epic.rename",
        json!({ "from": name, "to": new_name }),
    )];
    commit(
        tx,
        &board,
        changes,
        format!("epic {name} renamed to {new_name}"),
    )
    .await
}

/// `kb epic move`: moves the epic to `position` (1-based); other epics shift.
pub async fn epic_move(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
    position: i32,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let epic = epic_by_name(&mut tx, board.id, name).await?;
    let mut epics = epics_of(&mut tx, board.id).await?;
    if position < 1 || position as usize > epics.len() {
        return Err(Error::invalid(format!(
            "invalid position: {position} (board has {} epics)",
            epics.len()
        )));
    }
    let from = epics
        .iter()
        .position(|e| e.id == epic.id)
        .expect("epic is on board");
    let moved = epics.remove(from);
    epics.insert(position as usize - 1, moved);
    // epic_position_uniq is deferred, so transient duplicates are fine.
    for (i, e) in epics.iter().enumerate() {
        if e.position != i as i32 + 1 {
            sqlx::query("UPDATE epic SET position = $2 WHERE id = $1")
                .bind(e.id)
                .bind(i as i32 + 1)
                .execute(&mut *tx)
                .await?;
        }
    }
    let changes = vec![Change::board(
        "epic.move",
        json!({ "epic": name, "position": position }),
    )];
    commit(
        tx,
        &board,
        changes,
        format!("epic {name} moved to position {position}"),
    )
    .await
}

/// `kb epic remove`: only an epic without cards (in any lane) can be removed.
pub async fn epic_remove(
    conn: &mut PgConnection,
    session_key: &str,
    name: &str,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let epic = epic_by_name(&mut tx, board.id, name).await?;
    let cards: i64 = sqlx::query_scalar("SELECT count(*) FROM card WHERE epic_id = $1")
        .bind(epic.id)
        .fetch_one(&mut *tx)
        .await?;
    if cards > 0 {
        return Err(Error::invalid("epic is not empty"));
    }
    sqlx::query("DELETE FROM epic WHERE id = $1")
        .bind(epic.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE epic SET position = position - 1 WHERE board_id = $1 AND position > $2")
        .bind(board.id)
        .bind(epic.position)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::board("epic.remove", json!({ "epic": name }))];
    commit(tx, &board, changes, format!("epic {name} removed")).await
}

// ---------------------------------------------------------------------------------------
// Cards

/// Arguments of `kb card add`.
#[derive(Debug, Clone)]
pub struct CardAdd {
    pub lane: String,
    pub title: String,
    pub priority: i16,
    pub body: String,
    pub owner: String,
    /// `--from`: the new card is spawned from this card.
    pub from: Option<i32>,
    /// `--epic`: epic name. Without it, a card added `--from` another card inherits that
    /// card's epic.
    pub epic: Option<String>,
    /// `--discuss` / `--agreed`; default `derived`.
    pub origin: Origin,
    /// `--next`: the card goes to the front of the lane instead of the end.
    pub next: bool,
}

impl CardAdd {
    pub fn new(lane: impl Into<String>, title: impl Into<String>) -> Self {
        CardAdd {
            lane: lane.into(),
            title: title.into(),
            priority: crate::model::DEFAULT_PRIORITY,
            body: String::new(),
            owner: "agent".to_string(),
            from: None,
            epic: None,
            origin: Origin::Derived,
            next: false,
        }
    }
}

/// `kb card add`. With `from`, also creates a `spawned` link from that card to the new one.
pub async fn card_add(
    conn: &mut PgConnection,
    session_key: &str,
    args: CardAdd,
) -> Result<WriteOutcome> {
    require_text("title", &args.title)?;
    require_text("owner", &args.owner)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let lane = lane_by_name(&mut tx, board.id, &args.lane).await?;
    let from = match args.from {
        Some(seq) => Some(card_by_seq(&mut tx, board.id, seq).await?),
        None => None,
    };
    let epic_id = match &args.epic {
        Some(name) => Some(epic_by_name(&mut tx, board.id, name).await?.id),
        None => from.as_ref().and_then(|f| f.epic_id),
    };
    let seq: i32 = sqlx::query_scalar(
        "UPDATE board SET next_card_seq = next_card_seq + 1 WHERE id = $1 RETURNING next_card_seq - 1",
    )
    .bind(board.id)
    .fetch_one(&mut *tx)
    .await?;
    let position = if args.next {
        front_card_position(&mut tx, lane.id).await?
    } else {
        next_card_position(&mut tx, lane.id).await?
    };
    let card_id: i64 = sqlx::query_scalar(
        "INSERT INTO card (board_id, seq, lane_id, title, body, priority, position, owner, closed_at,
                           epic_id, origin)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CASE WHEN $9 THEN now() END, $10, $11)
         RETURNING id",
    )
    .bind(board.id)
    .bind(seq)
    .bind(lane.id)
    .bind(&args.title)
    .bind(&args.body)
    .bind(args.priority)
    .bind(position)
    .bind(&args.owner)
    .bind(lane.kind.is_closed())
    .bind(epic_id)
    .bind(args.origin)
    .fetch_one(&mut *tx)
    .await?;
    let mut changes = vec![Change::card(
        card_id,
        "card.add",
        json!({ "seq": seq, "title": args.title, "lane": lane.name }),
    )];
    let mut link = None;
    if let Some(from) = from {
        let link_id: i64 = sqlx::query_scalar(
            "INSERT INTO link (board_id, from_card_id, kind, to_card_id) VALUES ($1, $2, 'spawned', $3)
             RETURNING id",
        )
        .bind(board.id)
        .bind(from.id)
        .bind(card_id)
        .fetch_one(&mut *tx)
        .await?;
        link = Some(link_id);
        changes.push(Change::card(
            from.id,
            "link.add",
            json!({ "seq": from.seq, "kind": LinkKind::Spawned, "to_seq": seq }),
        ));
    }
    let mut out = commit(
        tx,
        &board,
        changes,
        format!("card {seq} added to {}", lane.name),
    )
    .await?;
    out.card = Some(seq);
    out.link = link;
    Ok(out)
}

/// `kb card move`: [`card_move_to`] to the end of the target lane.
pub async fn card_move(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    to: &str,
    note: Option<String>,
) -> Result<WriteOutcome> {
    card_move_to(conn, session_key, seq, to, note, false).await
}

/// `kb card move [--next]`. Sets `closed_at` when entering a `done`/`dropped` lane and
/// clears it otherwise; `note` adds a `progress` note. Exceeding the target's WIP limit is
/// a warning, and so is a card under discussion entering a doing lane (R7). With `next` the
/// card enters at the front of the lane (docs/board-v2.md 10.2). A move from a todo lane
/// into a doing lane records the queue's head in `detail.queue_head` (section 17.2).
pub async fn card_move_to(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    to: &str,
    note: Option<String>,
    next: bool,
) -> Result<WriteOutcome> {
    if let Some(n) = &note {
        require_text("note", n)?;
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    let target = lane_by_name(&mut tx, board.id, to).await?;
    if target.id == card.lane_id {
        return Err(Error::invalid(format!(
            "card is already in {}",
            target.name
        )));
    }
    let source = lane_by_id(&mut tx, card.lane_id).await?;
    // R10 (docs/board-v2.md section 17.2): a move from a todo lane into a doing lane records
    // the head of the todo lane's queue at this moment, before the card leaves it. A card
    // under discussion is not part of the queue; R7 covers its move, so nothing is recorded.
    let head = if source.kind == LaneKind::Todo
        && target.kind == LaneKind::Doing
        && card.origin != Origin::Discussing
    {
        Some(queue_head(&mut tx, source.id).await?)
    } else {
        None
    };
    let position = if next {
        front_card_position(&mut tx, target.id).await?
    } else {
        next_card_position(&mut tx, target.id).await?
    };
    sqlx::query(
        "UPDATE card SET lane_id = $2, position = $3, updated_at = now(),
                         closed_at = CASE WHEN $4 THEN now() END
         WHERE id = $1",
    )
    .bind(card.id)
    .bind(target.id)
    .bind(position)
    .bind(target.kind.is_closed())
    .execute(&mut *tx)
    .await?;
    let mut detail = json!({ "seq": seq, "from": source.name, "to": target.name });
    if let Some(head) = head {
        detail["queue_head"] = json!(head);
    }
    let mut changes = vec![Change::card(card.id, "card.move", detail)];
    if let Some(n) = &note {
        insert_note(&mut tx, card.id, NoteKind::Progress, n).await?;
        changes.push(Change::card(
            card.id,
            "card.note",
            json!({ "seq": seq, "kind": NoteKind::Progress, "excerpt": excerpt(n) }),
        ));
    }
    let mut warnings = Vec::new();
    if card.origin == Origin::Discussing && target.kind == LaneKind::Doing {
        // R7's immediate echo (docs/board-v2.md section 17.5).
        warnings.push(format!(
            "R7: card {seq} is still under discussion; run \"kb card confirm {seq}\" once the \
             user agreed, or move it back"
        ));
    }
    if let Some(limit) = target.wip_limit {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM card WHERE lane_id = $1")
            .bind(target.id)
            .fetch_one(&mut *tx)
            .await?;
        if count > limit as i64 {
            warnings.push(format!(
                "lane {} is over its WIP limit ({count}/{limit})",
                target.name
            ));
        }
    }
    let mut out = commit(
        tx,
        &board,
        changes,
        format!("card {seq} moved to {}", target.name),
    )
    .await?;
    out.card = Some(seq);
    out.warnings = warnings;
    Ok(out)
}

/// Fields of `kb card edit`; `None` leaves a field unchanged.
#[derive(Debug, Clone, Default)]
pub struct CardEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    pub priority: Option<i16>,
    pub owner: Option<String>,
    /// `Some(Some(name))`: `--epic <name>`; `Some(None)`: `--no-epic`.
    pub epic: Option<Option<String>>,
}

/// `kb card edit`. At least one field must be given. Changing the epic writes `card.epic`
/// (docs/board-v2.md section 3.5); the other fields write `card.edit`. When nothing actually
/// changes (only `--epic` with the card's current epic), nothing is written.
pub async fn card_edit(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    edit: CardEdit,
) -> Result<WriteOutcome> {
    let mut fields = Vec::new();
    if let Some(t) = &edit.title {
        require_text("title", t)?;
        fields.push("title");
    }
    if edit.body.is_some() {
        fields.push("body");
    }
    if edit.priority.is_some() {
        fields.push("priority");
    }
    if let Some(o) = &edit.owner {
        require_text("owner", o)?;
        fields.push("owner");
    }
    if fields.is_empty() && edit.epic.is_none() {
        return Err(Error::invalid(
            "nothing to edit: pass --title, --body, --priority, --owner, --epic or --no-epic",
        ));
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    let mut changes = Vec::new();
    let mut parts: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
    if !fields.is_empty() {
        sqlx::query(
            "UPDATE card SET title = coalesce($2, title), body = coalesce($3, body),
                             priority = coalesce($4, priority), owner = coalesce($5, owner),
                             updated_at = now()
             WHERE id = $1",
        )
        .bind(card.id)
        .bind(&edit.title)
        .bind(&edit.body)
        .bind(edit.priority)
        .bind(&edit.owner)
        .execute(&mut *tx)
        .await?;
        changes.push(Change::card(
            card.id,
            "card.edit",
            json!({ "seq": seq, "fields": fields }),
        ));
    }
    if let Some(target) = &edit.epic {
        let epic_id = match target {
            Some(name) => Some(epic_by_name(&mut tx, board.id, name).await?.id),
            None => None,
        };
        if epic_id != card.epic_id {
            let from = epic_name(&mut tx, card.epic_id).await?;
            sqlx::query("UPDATE card SET epic_id = $2, updated_at = now() WHERE id = $1")
                .bind(card.id)
                .bind(epic_id)
                .execute(&mut *tx)
                .await?;
            changes.push(Change::card(
                card.id,
                "card.epic",
                json!({ "seq": seq, "from": from, "to": target }),
            ));
            parts.push(match target {
                Some(name) => format!("epic {name}"),
                None => "no epic".to_string(),
            });
        }
    }
    let mut out = if changes.is_empty() {
        // Only --epic/--no-epic, and the card already has that epic: nothing to write.
        tx.rollback().await?;
        unchanged(&board, format!("card {seq} unchanged"))
    } else {
        let message = format!("card {seq} edited ({})", parts.join(", "));
        commit(tx, &board, changes, message).await?
    };
    out.card = Some(seq);
    Ok(out)
}

/// The result of a write that changed nothing: no activity, no `rev` bump, no notification.
fn unchanged(board: &Board, message: String) -> WriteOutcome {
    WriteOutcome {
        board_id: board.id,
        session_key: board.session_key.clone(),
        rev: board.rev,
        message,
        card: None,
        link: None,
        question: None,
        warnings: Vec::new(),
        hint: None,
    }
}

/// `kb card confirm` (`origin` = `agreed`) and `kb card discuss` (`origin` = `discussing`)
/// for one or more cards, in one transaction. Each changed card gets a `card.origin` row;
/// cards that already have `origin` are left alone. When no card changes, nothing is
/// written (no activity, no notification).
pub async fn card_set_origin(
    conn: &mut PgConnection,
    session_key: &str,
    seqs: &[i32],
    origin: Origin,
) -> Result<WriteOutcome> {
    if seqs.is_empty() {
        return Err(Error::invalid("no card given"));
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let mut changes = Vec::new();
    let mut lines = Vec::new();
    let mut seen = Vec::new();
    for &seq in seqs {
        if seen.contains(&seq) {
            continue;
        }
        seen.push(seq);
        let card = card_by_seq(&mut tx, board.id, seq).await?;
        if card.origin == origin {
            lines.push(format!("card {seq} is already {origin}"));
            continue;
        }
        sqlx::query("UPDATE card SET origin = $2, updated_at = now() WHERE id = $1")
            .bind(card.id)
            .bind(origin)
            .execute(&mut *tx)
            .await?;
        changes.push(Change::card(
            card.id,
            "card.origin",
            json!({ "seq": seq, "from": card.origin, "to": origin }),
        ));
        lines.push(format!("card {seq}: {} -> {origin}", card.origin));
    }
    let message = lines.join("\n");
    let mut out = if changes.is_empty() {
        tx.rollback().await?;
        unchanged(&board, message)
    } else {
        commit(tx, &board, changes, message).await?
    };
    if let [seq] = seen[..] {
        out.card = Some(seq);
    }
    Ok(out)
}

/// `kb card note` without `--because`.
pub async fn card_note(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    kind: NoteKind,
    text: &str,
) -> Result<WriteOutcome> {
    card_note_because(conn, session_key, seq, kind, text, &[]).await
}

/// `kb card note [--because <ref>]...` (docs/board-v2.md section 18). A `decision` note
/// without causes carries the hint of section 18.5.
pub async fn card_note_because(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    kind: NoteKind,
    text: &str,
    because: &[CauseRef],
) -> Result<WriteOutcome> {
    if kind == NoteKind::Comment {
        // Comments come from the human through kb_add_comment (docs/board-v2.md 13.7).
        return Err(Error::invalid(
            "comment notes come from the user; reply with --kind reply",
        ));
    }
    require_text("note", text)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    let causes = resolve_causes(&mut tx, board.id, seq, because).await?;
    let note_id = insert_note(&mut tx, card.id, kind, text).await?;
    let changes = vec![Change::card(
        card.id,
        "card.note",
        with_because(
            json!({ "seq": seq, "kind": kind, "excerpt": excerpt(text) }),
            &causes,
        ),
    )];
    insert_decision(&mut tx, board.id, card.id, Some(note_id), changes, &causes).await?;
    let mut out = commit(
        tx,
        &board,
        Vec::new(),
        format!("note ({kind}) added to card {seq}"),
    )
    .await?;
    out.card = Some(seq);
    if kind == NoteKind::Decision && causes.is_empty() {
        out.hint = Some(BECAUSE_HINT.to_string());
    }
    Ok(out)
}

/// `kb card block`: sets `blocked_reason`; the lane does not change.
pub async fn card_block(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    reason: &str,
) -> Result<WriteOutcome> {
    require_text("reason", reason)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    sqlx::query("UPDATE card SET blocked_reason = $2, updated_at = now() WHERE id = $1")
        .bind(card.id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::card(
        card.id,
        "card.block",
        json!({ "seq": seq, "reason": reason }),
    )];
    let mut out = commit(tx, &board, changes, format!("card {seq} blocked")).await?;
    out.card = Some(seq);
    Ok(out)
}

/// `kb card unblock`: clears `blocked_reason`.
pub async fn card_unblock(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    if card.blocked_reason.is_none() {
        return Err(Error::invalid(format!("card is not blocked: {seq}")));
    }
    sqlx::query("UPDATE card SET blocked_reason = NULL, updated_at = now() WHERE id = $1")
        .bind(card.id)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::card(card.id, "card.unblock", json!({ "seq": seq }))];
    let mut out = commit(tx, &board, changes, format!("card {seq} unblocked")).await?;
    out.card = Some(seq);
    Ok(out)
}

/// Moves a card into `lane` (unless already there) and closes it with `result`.
/// `closed_at` is the time the card entered its current done/dropped lane: it stays when
/// the card is already in `lane` (docs/board-v2.md section 16.3).
async fn close_card(tx: &mut PgConnection, card: &Card, lane: &Lane, result: &str) -> Result<()> {
    let position = if card.lane_id == lane.id {
        card.position
    } else {
        next_card_position(tx, lane.id).await?
    };
    sqlx::query(
        "UPDATE card SET lane_id = $2, position = $3, result = $4,
                         closed_at = CASE WHEN lane_id = $2 AND closed_at IS NOT NULL
                                          THEN closed_at ELSE now() END,
                         blocked_reason = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(card.id)
    .bind(lane.id)
    .bind(position)
    .bind(result)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// `kb card done` without `--because`.
pub async fn card_done(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    result: &str,
) -> Result<WriteOutcome> {
    card_done_because(conn, session_key, seq, result, &[]).await
}

/// `kb card done [--because <ref>]...`: moves the card to the leftmost `done` lane and
/// records `result`. The activity keeps the result (docs/board-v2.md section 18.3).
pub async fn card_done_because(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    result: &str,
    because: &[CauseRef],
) -> Result<WriteOutcome> {
    require_text("result", result)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    let causes = resolve_causes(&mut tx, board.id, seq, because).await?;
    let lane = first_lane_of_kind(&mut tx, board.id, LaneKind::Done)
        .await?
        .ok_or_else(|| {
            Error::invalid(
                "board has no done lane: add one with \"kb lane add <name> --kind done\"",
            )
        })?;
    close_card(&mut tx, &card, &lane, result).await?;
    let changes = vec![Change::card(
        card.id,
        "card.done",
        with_because(
            json!({ "seq": seq, "lane": lane.name, "result": result }),
            &causes,
        ),
    )];
    insert_decision(&mut tx, board.id, card.id, None, changes, &causes).await?;
    let echo = crate::rules::r11_echo(&mut tx, card.id, seq).await?;
    let mut out = commit(
        tx,
        &board,
        Vec::new(),
        format!("card {seq} done in {}", lane.name),
    )
    .await?;
    out.card = Some(seq);
    out.warnings.extend(echo);
    Ok(out)
}

/// `kb card drop` without `--because`.
pub async fn card_drop(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    reason: &str,
) -> Result<WriteOutcome> {
    card_drop_because(conn, session_key, seq, reason, &[]).await
}

/// `kb card drop [--because <ref>]...`: moves the card to the leftmost `dropped` lane,
/// creating lane 「捨棄」 at the far right when the board has none, and stores the reason in
/// `result`. Without causes the result carries the hint of docs/board-v2.md section 18.5.
pub async fn card_drop_because(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    reason: &str,
    because: &[CauseRef],
) -> Result<WriteOutcome> {
    require_text("reason", reason)?;
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    let causes = resolve_causes(&mut tx, board.id, seq, because).await?;
    let mut changes = Vec::new();
    let lane = match first_lane_of_kind(&mut tx, board.id, LaneKind::Dropped).await? {
        Some(lane) => lane,
        None => {
            if lanes_of(&mut tx, board.id)
                .await?
                .iter()
                .any(|l| l.name == DROPPED_LANE_NAME)
            {
                return Err(Error::invalid(format!(
                    "board has no dropped lane and lane {DROPPED_LANE_NAME} has another kind: \
                     add one with \"kb lane add <name> --kind dropped\""
                )));
            }
            let position = next_lane_position(&mut tx, board.id).await?;
            let lane = insert_lane(
                &mut tx,
                board.id,
                DROPPED_LANE_NAME,
                position,
                LaneKind::Dropped,
                None,
            )
            .await?;
            changes.push(Change::board(
                "lane.add",
                json!({ "lane": DROPPED_LANE_NAME, "kind": LaneKind::Dropped }),
            ));
            lane
        }
    };
    close_card(&mut tx, &card, &lane, reason).await?;
    changes.push(Change::card(
        card.id,
        "card.drop",
        with_because(json!({ "seq": seq, "reason": reason }), &causes),
    ));
    insert_decision(&mut tx, board.id, card.id, None, changes, &causes).await?;
    let echo = crate::rules::r11_echo(&mut tx, card.id, seq).await?;
    let mut out = commit(
        tx,
        &board,
        Vec::new(),
        format!("card {seq} dropped to {}", lane.name),
    )
    .await?;
    out.card = Some(seq);
    out.warnings.extend(echo);
    if causes.is_empty() {
        out.hint = Some(BECAUSE_HINT.to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Links

/// What a new link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Another card on the same board, by number.
    Card(i32),
    /// An external target (`ref` links), e.g. `src/auth/token.rs:120` or a URL.
    External(String),
}

/// `kb link add`. `kind` is `blocks`, `relates` or `continues` with a card target, or `ref`
/// with an external target. `spawned` links are created only by `card add --from`.
pub async fn link_add(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    kind: LinkKind,
    target: LinkTarget,
    label: Option<String>,
) -> Result<WriteOutcome> {
    match (&kind, &target) {
        (LinkKind::Spawned, _) => {
            return Err(Error::invalid(
                "spawned links are created by \"kb card add --from\"",
            ));
        }
        (LinkKind::Ref, LinkTarget::Card(_)) => {
            return Err(Error::invalid(
                "ref links point at an external target, not a card",
            ));
        }
        (LinkKind::Ref, LinkTarget::External(t)) => require_text("ref target", t)?,
        (_, LinkTarget::External(_)) => {
            return Err(Error::invalid(format!("{kind} links point at a card")));
        }
        (_, LinkTarget::Card(to)) if *to == seq => {
            return Err(Error::invalid("cannot link a card to itself"));
        }
        _ => {}
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let from = card_by_seq(&mut tx, board.id, seq).await?;
    let (to_card, external) = match &target {
        LinkTarget::Card(to) => (Some(card_by_seq(&mut tx, board.id, *to).await?), None),
        LinkTarget::External(t) => (None, Some(t.clone())),
    };
    let to_card_id = to_card.as_ref().map(|c| c.id);
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM link WHERE from_card_id = $1 AND kind = $2
                          AND to_card_id IS NOT DISTINCT FROM $3
                          AND target IS NOT DISTINCT FROM $4)",
    )
    .bind(from.id)
    .bind(kind)
    .bind(to_card_id)
    .bind(&external)
    .fetch_one(&mut *tx)
    .await?;
    if exists {
        return Err(Error::invalid("link already exists"));
    }
    let link_id: i64 = sqlx::query_scalar(
        "INSERT INTO link (board_id, from_card_id, kind, to_card_id, target, label)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(board.id)
    .bind(from.id)
    .bind(kind)
    .bind(to_card_id)
    .bind(&external)
    .bind(&label)
    .fetch_one(&mut *tx)
    .await?;
    let (detail, describe) = match (&to_card, &external) {
        (Some(to), _) => (
            json!({ "seq": seq, "kind": kind, "to_seq": to.seq }),
            format!("card {seq} {kind} card {}", to.seq),
        ),
        (None, t) => {
            let t = t.clone().unwrap_or_default();
            (
                json!({ "seq": seq, "kind": kind, "target": t }),
                format!("card {seq} ref {t}"),
            )
        }
    };
    let changes = vec![Change::card(from.id, "link.add", detail)];
    let mut out = commit(
        tx,
        &board,
        changes,
        format!("link {link_id} added: {describe}"),
    )
    .await?;
    out.card = Some(seq);
    out.link = Some(link_id);
    Ok(out)
}

/// `kb link remove <id>`: the link must belong to the session's board.
pub async fn link_remove(
    conn: &mut PgConnection,
    session_key: &str,
    link_id: i64,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let row: Option<(i64, i32, LinkKind)> = sqlx::query_as(
        "SELECT c.id, c.seq, l.kind FROM link l JOIN card c ON c.id = l.from_card_id
         WHERE l.id = $1 AND l.board_id = $2",
    )
    .bind(link_id)
    .bind(board.id)
    .fetch_optional(&mut *tx)
    .await?;
    let (card_id, seq, kind) = row.ok_or(Error::LinkNotFound(link_id))?;
    sqlx::query("DELETE FROM link WHERE id = $1")
        .bind(link_id)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::card(
        card_id,
        "link.remove",
        json!({ "seq": seq, "kind": kind }),
    )];
    let mut out = commit(tx, &board, changes, format!("link {link_id} removed")).await?;
    out.card = Some(seq);
    out.link = Some(link_id);
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Questions (docs/questions.md section 5)

/// Number of options a question must have.
pub const QUESTION_OPTIONS: std::ops::RangeInclusive<usize> = 2..=6;

/// One `--option` of `kb ask`: `label` or `label::description`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionSpec {
    pub label: String,
    pub description: Option<String>,
}

/// Parses `label` or `label::description`.
pub fn parse_option(s: &str) -> Result<OptionSpec> {
    let (label, description) = match s.split_once("::") {
        Some((l, d)) => (l.trim(), Some(d.trim()).filter(|d| !d.is_empty())),
        None => (s.trim(), None),
    };
    if label.is_empty() {
        return Err(Error::invalid(format!(
            "invalid option {s:?}: the label must not be empty"
        )));
    }
    Ok(OptionSpec {
        label: label.to_string(),
        description: description.map(str::to_string),
    })
}

/// `kb ask`: writes a question with 2 to 6 options on card `seq` and returns immediately.
pub async fn question_ask(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
    text: &str,
    options: Vec<OptionSpec>,
    multi: bool,
) -> Result<WriteOutcome> {
    require_text("question", text)?;
    if !QUESTION_OPTIONS.contains(&options.len()) {
        return Err(Error::invalid(format!(
            "a question needs 2 to 6 options (got {})",
            options.len()
        )));
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let card = card_by_seq(&mut tx, board.id, seq).await?;
    let lane = lane_by_id(&mut tx, card.lane_id).await?;
    if lane.kind.is_closed() {
        return Err(Error::invalid("card is closed"));
    }
    let question_id: i64 = sqlx::query_scalar(
        "INSERT INTO question (board_id, card_id, text, multi) VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(board.id)
    .bind(card.id)
    .bind(text)
    .bind(multi)
    .fetch_one(&mut *tx)
    .await?;
    for (i, o) in options.iter().enumerate() {
        sqlx::query(
            "INSERT INTO question_option (question_id, position, label, description)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(question_id)
        .bind(i as i32 + 1)
        .bind(&o.label)
        .bind(&o.description)
        .execute(&mut *tx)
        .await?;
    }
    let changes = vec![Change::card(
        card.id,
        "question.ask",
        json!({ "question": question_id, "seq": seq, "excerpt": excerpt(text) }),
    )];
    let mut out = commit(
        tx,
        &board,
        changes,
        format!(
            "question {question_id} asked on card {seq} ({} options)",
            options.len()
        ),
    )
    .await?;
    out.card = Some(seq);
    out.question = Some(question_id);
    Ok(out)
}

/// `kb question cancel`: only an `open` question can be cancelled.
pub async fn question_cancel(
    conn: &mut PgConnection,
    session_key: &str,
    question_id: i64,
    reason: Option<String>,
) -> Result<WriteOutcome> {
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let row: Option<(i64, i32, QuestionStatus)> = sqlx::query_as(
        "SELECT c.id, c.seq, q.status FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.id = $1 AND q.board_id = $2 FOR UPDATE OF q",
    )
    .bind(question_id)
    .bind(board.id)
    .fetch_optional(&mut *tx)
    .await?;
    let (card_id, seq, status) = row.ok_or(Error::QuestionNotFound(question_id))?;
    if status != QuestionStatus::Open {
        return Err(Error::invalid(format!(
            "question is not open: {question_id} is {status}"
        )));
    }
    sqlx::query("UPDATE question SET status = 'cancelled' WHERE id = $1")
        .bind(question_id)
        .execute(&mut *tx)
        .await?;
    let changes = vec![Change::card(
        card_id,
        "question.cancel",
        json!({ "question": question_id, "seq": seq, "reason": reason }),
    )];
    let mut out = commit(
        tx,
        &board,
        changes,
        format!("question {question_id} cancelled"),
    )
    .await?;
    out.card = Some(seq);
    out.question = Some(question_id);
    Ok(out)
}

/// Result of `kb inbox`.
#[derive(Debug, Clone, Serialize)]
pub struct Inbox {
    /// Answered questions not delivered before this call, in answer order.
    pub answers: Vec<QuestionView>,
    /// Comments from the human not delivered before this call, oldest first
    /// (docs/board-v2.md section 13.8).
    pub comments: Vec<CommentView>,
    /// Questions still waiting for an answer.
    pub open_questions: i64,
    /// Comments without a reply (docs/board-v2.md section 13.9), including the ones above.
    pub unreplied_comments: Vec<CommentView>,
    /// Whether `answers` and `comments` were marked as delivered.
    pub delivered: bool,
    /// When `answers` and `comments` are empty: what was already delivered in this turn
    /// (docs/board-v2.md section 19.2). Empty otherwise.
    pub delivered_this_turn: DeliveredThisTurn,
}

/// Answers and comments delivered since the board's `turn_started_at` (docs/board-v2.md
/// section 19.2).
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeliveredThisTurn {
    pub answers: Vec<QuestionView>,
    pub comments: Vec<CommentView>,
}

impl DeliveredThisTurn {
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.comments.is_empty()
    }
}

/// Marks `comments` as delivered (inside a write transaction) and returns their
/// `comment.deliver` activity.
pub(crate) async fn mark_comments_delivered(
    tx: &mut PgConnection,
    comments: &[CommentView],
    actor: &'static str,
) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for c in comments {
        sqlx::query("UPDATE card_note SET delivered_at = now() WHERE id = $1")
            .bind(c.note.id)
            .execute(&mut *tx)
            .await?;
        changes.push(
            Change::card(
                c.note.card_id,
                "comment.deliver",
                json!({ "note": c.note.id, "seq": c.card_seq }),
            )
            .by(actor),
        );
    }
    Ok(changes)
}

/// Marks `answers` as delivered (inside a write transaction) and returns their
/// `question.deliver` activity.
pub(crate) async fn mark_delivered(
    tx: &mut PgConnection,
    answers: &[QuestionView],
    actor: &'static str,
) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for q in answers {
        sqlx::query("UPDATE question SET delivered_at = now() WHERE id = $1")
            .bind(q.question.id)
            .execute(&mut *tx)
            .await?;
        changes.push(
            Change::card(
                q.question.card_id,
                "question.deliver",
                json!({ "question": q.question.id, "seq": q.card_seq }),
            )
            .by(actor),
        );
    }
    Ok(changes)
}

/// `kb inbox [--peek]`: answered questions and comments not yet delivered to the agent.
/// Unless `peek`, they are marked as delivered (`question.deliver`, `comment.deliver`) in
/// one transaction. An archived board is only read, never marked.
pub async fn inbox(conn: &mut PgConnection, session_key: &str, peek: bool) -> Result<Inbox> {
    let (mut tx, board) = begin(conn, session_key, true).await?;
    let answers = crate::read::undelivered_answers(&mut tx, board.id).await?;
    let comments = crate::read::undelivered_comments(&mut tx, board.id).await?;
    let open_questions = crate::read::open_question_count(&mut tx, board.id).await?;
    let unreplied_comments = crate::read::unreplied_comments(&mut tx, board.id).await?;
    let mark = !peek
        && (!answers.is_empty() || !comments.is_empty())
        && board.status == BoardStatus::Active;
    // Nothing new: show what the hooks (or an earlier kb inbox) already delivered in this
    // turn, so an agent that runs kb inbox after a wake still gets the content.
    let delivered_this_turn = match board.turn_started_at {
        Some(since) if answers.is_empty() && comments.is_empty() => DeliveredThisTurn {
            answers: crate::read::answers_delivered_since(&mut tx, board.id, since).await?,
            comments: crate::read::comments_delivered_since(&mut tx, board.id, since).await?,
        },
        _ => DeliveredThisTurn::default(),
    };
    if mark {
        let mut changes = mark_delivered(&mut tx, &answers, "agent").await?;
        changes.extend(mark_comments_delivered(&mut tx, &comments, "agent").await?);
        commit(tx, &board, changes, String::new()).await?;
    } else {
        // Release the board lock now, not on the connection's next use.
        tx.rollback().await?;
    }
    Ok(Inbox {
        answers,
        comments,
        open_questions,
        unreplied_comments,
        delivered: mark,
        delivered_this_turn,
    })
}

// ---------------------------------------------------------------------------------------
// Order of a lane (docs/board-v2.md section 10.2)

/// `kb card next <n>`: the card goes to the front of its lane.
pub async fn card_next(
    conn: &mut PgConnection,
    session_key: &str,
    seq: i32,
) -> Result<WriteOutcome> {
    card_order(conn, session_key, &[seq]).await
}

/// `kb card order <n> <n>...`: the listed cards go to the front of their lane in the given
/// order; the other cards keep their relative order after them. All cards must be in the
/// same lane. When the order does not change, nothing is written (no activity, no `rev`).
pub async fn card_order(
    conn: &mut PgConnection,
    session_key: &str,
    seqs: &[i32],
) -> Result<WriteOutcome> {
    if seqs.is_empty() {
        return Err(Error::invalid("no card given"));
    }
    let (mut tx, board) = begin(conn, session_key, false).await?;
    let mut listed: Vec<Card> = Vec::new();
    for &seq in seqs {
        let card = card_by_seq(&mut tx, board.id, seq).await?;
        if !listed.iter().any(|c| c.id == card.id) {
            listed.push(card);
        }
    }
    let lane_id = listed[0].lane_id;
    if listed.iter().any(|c| c.lane_id != lane_id) {
        return Err(Error::invalid("cards are not in the same lane"));
    }
    let lane = lane_by_id(&mut tx, lane_id).await?;
    // The lane's current order: position, then number (what every read shows).
    let current: Vec<(i64, i32)> = sqlx::query_as(
        "SELECT id, seq FROM card WHERE lane_id = $1 ORDER BY position, seq FOR UPDATE",
    )
    .bind(lane_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut wanted: Vec<i64> = listed.iter().map(|c| c.id).collect();
    wanted.extend(
        current
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !listed.iter().any(|c| c.id == *id)),
    );
    let listed_seqs: Vec<i32> = listed.iter().map(|c| c.seq).collect();
    let numbers = listed_seqs
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if wanted.iter().eq(current.iter().map(|(id, _)| id)) {
        // No change: success without a write.
        tx.rollback().await?;
        let rev = board.rev;
        return Ok(WriteOutcome {
            board_id: board.id,
            session_key: board.session_key.clone(),
            rev,
            message: format!("order of {} unchanged", lane.name),
            card: None,
            link: None,
            question: None,
            warnings: Vec::new(),
            hint: None,
        });
    }
    for (i, id) in wanted.iter().enumerate() {
        sqlx::query("UPDATE card SET position = $2 WHERE id = $1")
            .bind(id)
            .bind(i as i32 + 1)
            .execute(&mut *tx)
            .await?;
    }
    let changes = vec![Change::board(
        "card.order",
        json!({ "lane": lane.name, "seqs": listed_seqs }),
    )];
    commit(
        tx,
        &board,
        changes,
        if listed_seqs.len() == 1 {
            format!("card {numbers} moved to the front of {}", lane.name)
        } else {
            format!("cards {numbers} moved to the front of {}", lane.name)
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs/board-v2.md section 26.2.
    #[test]
    fn board_terminal_values_are_trimmed_and_blank_ones_dropped() {
        let t = BoardTerminal::new(" t2b ", Some(" spc1 "), Some(""), Some(" Acme ")).unwrap();
        assert_eq!(
            t,
            BoardTerminal {
                id: "t2b".into(),
                space: Some("spc1".into()),
                tab: None,
                workspace_name: Some("Acme".into()),
            }
        );
        // A name without a space is dropped.
        let t = BoardTerminal::new("t2b", Some("  "), None, Some("Acme")).unwrap();
        assert_eq!((t.space, t.workspace_name), (None, None));
        let e = BoardTerminal::new("  ", Some("spc1"), None, None).unwrap_err();
        assert_eq!(e.to_string(), TERMINAL_ID_BLANK);
    }
}
