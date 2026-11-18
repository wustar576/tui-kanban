//! Database side of the Claude Code hook entry points (docs/hooks.md section 3).
//!
//! The `kb` binary reads the hook's JSON input, calls these functions and prints the
//! output; fail-open handling (exit 0 on every unexpected error) is also done there.
//! Each function is one transaction; all except [`tool_used`] bump the board's `rev`.

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use crate::error::{Error, Result};
use crate::model::{Board, BoardStatus, Outcome};
use crate::read::{self, CommentView, QuestionView};
use crate::resolve::TerminalInfo;
use crate::rules::{self, Level, Levels, RuleId, RuleResult};
use crate::write::{self, Change};

/// The fields of a hook's standard input that kb uses. Every field is optional; values of
/// an unexpected type count as missing (docs/hooks.md section 2.1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookInput {
    pub session_id: String,
    pub prompt_id: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
    /// `SessionStart`.
    pub source: Option<String>,
    /// `UserPromptSubmit`.
    pub prompt: Option<String>,
    /// `PreToolUse`, `PostToolUse`.
    pub tool_name: Option<String>,
    pub tool_input: Value,
    pub duration_ms: Option<i64>,
    /// `Stop`.
    pub stop_hook_active: bool,
    /// `Stop`: the agent's final message of the turn (the model judge, docs/hooks.md
    /// section 3.7).
    pub last_assistant_message: Option<String>,
    /// `SessionEnd`.
    pub reason: Option<String>,
    /// Path of the session's transcript (every event); used only for automatic adoption
    /// (docs/board-v2.md section 12.8).
    pub transcript_path: Option<String>,
}

impl HookInput {
    /// Parses the JSON text of a hook's standard input. `None` when it is not a JSON object.
    pub fn parse(text: &str) -> Option<HookInput> {
        let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
        let obj = value.as_object()?;
        let text = |k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        };
        Some(HookInput {
            session_id: text("session_id").unwrap_or_default().trim().to_string(),
            prompt_id: text("prompt_id"),
            cwd: text("cwd"),
            hook_event_name: text("hook_event_name"),
            source: text("source"),
            prompt: obj
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_name: text("tool_name"),
            tool_input: obj.get("tool_input").cloned().unwrap_or(Value::Null),
            duration_ms: obj
                .get("duration_ms")
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))),
            stop_hook_active: obj
                .get("stop_hook_active")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            last_assistant_message: text("last_assistant_message"),
            reason: text("reason"),
            transcript_path: text("transcript_path"),
        })
    }
}

/// `SessionStart` sources; anything else is treated as `startup`.
pub const SESSION_SOURCES: [&str; 5] = ["startup", "resume", "clear", "compact", "fork"];

pub fn normalize_source(source: Option<&str>) -> &'static str {
    let s = source.unwrap_or("");
    SESSION_SOURCES
        .into_iter()
        .find(|k| *k == s)
        .unwrap_or("startup")
}

/// Undelivered answers and comments that a hook just delivered, plus the open question
/// count and the comments still without a reply (docs/board-v2.md sections 13.8, 13.9).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Delivery {
    pub answers: Vec<QuestionView>,
    pub comments: Vec<CommentView>,
    pub open_questions: i64,
    pub unreplied_comments: Vec<CommentView>,
}

impl Delivery {
    /// Something was delivered.
    pub fn is_empty(&self) -> bool {
        self.answers.is_empty() && self.comments.is_empty()
    }
}

async fn deliver(
    tx: &mut PgConnection,
    board_id: i64,
    changes: &mut Vec<Change>,
) -> Result<Delivery> {
    let answers = read::undelivered_answers(tx, board_id).await?;
    changes.extend(write::mark_delivered(tx, &answers, "hook").await?);
    let comments = read::undelivered_comments(tx, board_id).await?;
    changes.extend(write::mark_comments_delivered(tx, &comments, "hook").await?);
    let open_questions = read::open_question_count(tx, board_id).await?;
    let unreplied_comments = read::unreplied_comments(tx, board_id).await?;
    Ok(Delivery {
        answers,
        comments,
        open_questions,
        unreplied_comments,
    })
}

/// Whether a `UserPromptSubmit` prompt is a task notification (a waiter's wake-up or a
/// finished background task) rather than a message from the human (docs/board-v2.md
/// section 13.6).
pub fn is_task_notification(prompt: &str) -> bool {
    prompt.trim_start().starts_with("<task-notification>")
}

/// Commits a hook transaction: activity, `rev + 1`, `updated_at`.
async fn finish(tx: write::Tx<'_>, board: &Board, changes: Vec<Change>) -> Result<i64> {
    Ok(write::commit(tx, board, changes, String::new()).await?.rev)
}

/// Locks the session's active board; `None` when there is none or it is archived.
async fn begin_active<'c>(
    conn: &'c mut PgConnection,
    session_key: &str,
) -> Result<Option<(write::Tx<'c>, Board)>> {
    match write::begin(conn, session_key, false).await {
        Ok(v) => Ok(Some(v)),
        Err(Error::NoBoard(_) | Error::BoardArchived) => Ok(None),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------------------
// session-start

#[derive(Debug, Clone, Serialize)]
pub struct SessionStart {
    /// The board after the hook.
    pub board: Board,
    pub reopened: bool,
    pub delivery: Delivery,
}

/// `kb hook session-start` (docs/board-v2.md section 12.2). `None` when the session has no
/// board: nothing is created or written. Otherwise reopens an archived board, refreshes
/// its terminal fields (`terminal` runs only then), records `session.start` and delivers
/// undelivered answers.
pub async fn session_start(
    conn: &mut PgConnection,
    session_key: &str,
    source: &str,
    terminal: impl FnOnce() -> Option<TerminalInfo>,
) -> Result<Option<SessionStart>> {
    let source = normalize_source(Some(source));
    let mut tx = conn.begin().await?;
    let Some(board) =
        sqlx::query_as::<_, Board>("SELECT * FROM board WHERE session_key = $1 FOR UPDATE")
            .bind(session_key)
            .fetch_optional(&mut *tx)
            .await?
    else {
        tx.rollback().await?;
        return Ok(None);
    };
    let mut changes = Vec::new();
    let reopened = board.status == BoardStatus::Archived;
    if reopened {
        write::reopen_in_tx(&mut tx, board.id).await?;
        changes.push(Change::board("board.reopen", json!({})).by("hook"));
    }
    write::refresh_terminal(&mut tx, board.id, terminal().as_ref()).await?;
    changes.push(Change::board("session.start", json!({ "source": source })).by("hook"));
    let delivery = deliver(&mut tx, board.id, &mut changes).await?;
    finish(tx, &board, changes).await?;
    let board = read::board_by_id(conn, board.id)
        .await?
        .ok_or_else(|| Error::NoBoard(session_key.to_string()))?;
    Ok(Some(SessionStart {
        board,
        reopened,
        delivery,
    }))
}

/// Automatic adoption (docs/board-v2.md section 12.8): when the session has no board, the
/// first of `predecessors` (nearest first) that has a board is adopted with
/// `detail.auto = true`. `predecessors` and `terminal` run only when needed. `None` when
/// the session already has a board or no predecessor has one.
pub async fn auto_adopt(
    conn: &mut PgConnection,
    session_key: &str,
    predecessors: impl FnOnce() -> Vec<String>,
    terminal: impl FnOnce() -> Option<TerminalInfo>,
) -> Result<Option<write::Adoption>> {
    if read::board_by_session(conn, session_key).await?.is_some() {
        return Ok(None);
    }
    for prev in predecessors() {
        if let Some(board) = read::board_by_session(conn, &prev).await? {
            let adoption =
                write::adopt_board_id(conn, session_key, board.id, terminal().as_ref(), true)
                    .await?;
            return Ok(Some(adoption));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------------------
// prompt

#[derive(Debug, Clone, Serialize)]
pub struct Prompt {
    pub turn_key: String,
    pub lanes: i64,
    pub delivery: Delivery,
    /// The warn-level failures of the previous turn (docs/board-v2.md section 14.2).
    pub reminders: Vec<Reminder>,
}

/// One `check.warn` row shown to the agent by `kb hook prompt` (docs/board-v2.md
/// section 14.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Reminder {
    pub rule: String,
    /// The rule's English sentence for the agent (`detail.text`).
    pub text: String,
    /// The command that fixes it (`detail.fix`); may be empty.
    pub fix: String,
    /// Recorded after a block in the same turn for a rule at level `block`: it would have
    /// blocked (docs/board-v2.md section 16.1).
    pub not_enforced: bool,
}

/// Added after the sentence of a reminder whose rule would have blocked
/// (docs/board-v2.md section 16.1).
pub const NOT_ENFORCED: &str = " (was not enforced: one block per turn)";

impl Reminder {
    /// One line: the sentence (line breaks become spaces), [`NOT_ENFORCED`] when the rule
    /// would have blocked, then ` Fix: <command>` when there is one.
    pub fn line(&self) -> String {
        let mut text = self
            .text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if self.not_enforced {
            text.push_str(NOT_ENFORCED);
        }
        if self.fix.trim().is_empty() {
            text
        } else {
            format!("{text} Fix: {}", self.fix.trim())
        }
    }
}

/// The `check.warn` rows written since the board's latest `user.prompt` of `kb hook prompt`
/// (all of them when there is none; the `user.prompt` of `kb board start` is not a
/// boundary, docs/board-v2.md section 14.2); for each rule only the rows of its latest check (one check is one
/// transaction, so its rows share `at`). Rows without `detail.text` (written before
/// section 14.2) are left out. Rows recorded after a block (`after_block`) at level
/// `block` are marked `not_enforced` (section 16.1). Ordered by rule number (R9 before
/// R10, section 17.4), then by writing order. Must run before the new `user.prompt` is
/// inserted.
async fn last_turn_warnings(conn: &mut PgConnection, board_id: i64) -> Result<Vec<Reminder>> {
    Ok(sqlx::query_as::<_, Reminder>(
        "WITH warns AS (
           SELECT id, at, coalesce(detail->>'rule', '') AS rule, detail->>'text' AS text,
                  coalesce(detail->>'fix', '') AS fix,
                  (detail->>'after_block' = 'true' AND detail->>'level' = 'block') IS TRUE
                    AS not_enforced
           FROM activity
           WHERE board_id = $1 AND verb = 'check.warn' AND detail->>'text' IS NOT NULL
             AND id > coalesce((SELECT max(id) FROM activity
                                WHERE board_id = $1 AND verb = 'user.prompt'
                                  AND (detail->>'source') IS DISTINCT FROM $2), 0)
         )
         SELECT w.rule, w.text, w.fix, w.not_enforced FROM warns w
         WHERE w.at = (SELECT max(x.at) FROM warns x WHERE x.rule = w.rule)
         ORDER BY length(w.rule), w.rule, w.id",
    )
    .bind(board_id)
    .bind(rules::BOARD_START_SOURCE)
    .fetch_all(&mut *conn)
    .await?)
}

/// Whether a prompt invokes the skill `kb` (`/kb`, `/kb <text>`, `/kb:kb ...`), which runs
/// `kb board start` (docs/board-v2.md section 12.12).
pub fn is_kb_skill_prompt(prompt: &str) -> bool {
    prompt
        .trim_start()
        .strip_prefix("/kb")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', ' ', '\t', '\r', '\n']))
}

/// Whether the board's latest activity row is the `user.prompt` of `kb board start`: the
/// turn has already started and nothing happened since (docs/hooks.md section 3.2).
async fn turn_started_by_board_start(conn: &mut PgConnection, board_id: i64) -> Result<bool> {
    let latest: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT verb, detail->>'source' FROM activity WHERE board_id = $1
         ORDER BY id DESC LIMIT 1",
    )
    .bind(board_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(matches!(
        latest,
        Some((verb, Some(source))) if verb == "user.prompt" && source == rules::BOARD_START_SOURCE
    ))
}

/// `kb hook prompt` (docs/hooks.md section 3.2): starts a new turn, records `user.prompt`,
/// delivers undelivered answers and returns the previous turn's warnings (docs/board-v2.md
/// section 14.2). `None` when the session has no active board. When the prompt is `/kb` and
/// `kb board start` has already started the turn for it (its `user.prompt` is the board's
/// latest row), the turn is kept: no new turn, no `user.prompt`, no warnings; only the
/// delivery.
///
/// The warnings are read and the new `user.prompt` is written in one transaction that
/// holds the board's lock, so each `check.warn` row is returned by exactly one call.
pub async fn prompt(
    conn: &mut PgConnection,
    session_key: &str,
    prompt_id: Option<&str>,
    prompt: &str,
) -> Result<Option<Prompt>> {
    let Some((mut tx, board)) = begin_active(conn, session_key).await? else {
        return Ok(None);
    };
    let mut changes = Vec::new();
    let same_prompt =
        is_kb_skill_prompt(prompt) && turn_started_by_board_start(&mut tx, board.id).await?;
    let (turn_key, reminders) = if same_prompt {
        // `kb board start` already started the turn of this same prompt: keep it (no
        // second turn start), and leave the reminders for the next prompt.
        (board.turn_key.clone().unwrap_or_default(), Vec::new())
    } else {
        let reminders = last_turn_warnings(&mut tx, board.id).await?;
        let turn_key = prompt_id
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(write::generated_turn_key);
        sqlx::query("UPDATE board SET turn_key = $2, turn_started_at = now() WHERE id = $1")
            .bind(board.id)
            .bind(&turn_key)
            .execute(&mut *tx)
            .await?;
        // `text` is for the model judge (docs/hooks.md section 3.7).
        changes.push(
            Change::board(
                "user.prompt",
                json!({
                    "excerpt": write::excerpt(prompt),
                    "text": crate::judge::prompt_text(prompt),
                }),
            )
            .by("user"),
        );
        (turn_key, reminders)
    };
    let delivery = deliver(&mut tx, board.id, &mut changes).await?;
    let lanes: i64 = sqlx::query_scalar("SELECT count(*) FROM lane WHERE board_id = $1")
        .bind(board.id)
        .fetch_one(&mut *tx)
        .await?;
    if changes.is_empty() {
        // The turn was kept and nothing was delivered: nothing changed.
        tx.rollback().await?;
    } else {
        finish(tx, &board, changes).await?;
    }
    Ok(Some(Prompt {
        turn_key,
        lanes,
        delivery,
        reminders,
    }))
}

/// `kb hook prompt` for a task notification (docs/board-v2.md section 13.6): no new turn and
/// no `user.prompt`; only delivers undelivered answers and comments. `None` when the
/// session has no active board. Writes (and bumps `rev`) only when something is delivered.
pub async fn notification(conn: &mut PgConnection, session_key: &str) -> Result<Option<Delivery>> {
    let Some((mut tx, board)) = begin_active(conn, session_key).await? else {
        return Ok(None);
    };
    let mut changes = Vec::new();
    let delivery = deliver(&mut tx, board.id, &mut changes).await?;
    if changes.is_empty() {
        tx.rollback().await?;
    } else {
        finish(tx, &board, changes).await?;
    }
    Ok(Some(delivery))
}

// ---------------------------------------------------------------------------------------
// pre-tool

/// Outcome of `kb hook pre-tool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreTool {
    /// No active board: do nothing.
    NoBoard,
    /// The tool is not checked, the rule passed, or its level is `off`.
    Pass,
    /// The rule failed at level `warn`; `check.warn` was recorded.
    Warn(RuleResult),
    /// The rule failed at level `block`; `check.block` was recorded. The text is the
    /// reason of the denial (`permissionDecisionReason`, docs/hooks.md section 3.3).
    Block(RuleResult, String),
}

/// `kb hook pre-tool` (docs/hooks.md section 3.3): R1 for file edits, R6 for
/// `AskUserQuestion`.
pub async fn pre_tool(
    conn: &mut PgConnection,
    session_key: &str,
    tool_name: &str,
    levels: &Levels,
) -> Result<PreTool> {
    let rule = if rules::is_file_edit_tool(tool_name) {
        RuleId::R1
    } else if tool_name == rules::ASK_TOOL {
        RuleId::R6
    } else {
        return Ok(PreTool::Pass);
    };
    let Some(board) = read::board_by_session(conn, session_key).await? else {
        return Ok(PreTool::NoBoard);
    };
    if board.status != BoardStatus::Active {
        return Ok(PreTool::NoBoard);
    }
    if levels.get(rule) == Level::Off {
        return Ok(PreTool::Pass);
    }
    let result = match rule {
        RuleId::R1 => rules::check_r1(conn, &board, levels).await?,
        _ => rules::r6_violation(levels),
    };
    if result.passed {
        return Ok(PreTool::Pass);
    }
    let verb = if result.blocks() {
        "check.block"
    } else {
        "check.warn"
    };
    record_checks(conn, session_key, std::slice::from_ref(&result), verb).await?;
    if result.blocks() {
        let text = if rule == RuleId::R6 {
            rules::R6_TEXT.to_string()
        } else {
            rules::block_text(std::slice::from_ref(&result), rules::PRE_TOOL_HEADER)
        };
        Ok(PreTool::Block(result, text))
    } else {
        Ok(PreTool::Warn(result))
    }
}

/// `check.warn` / `check.block` rows (`detail`: `rule`, `message`, `text`, `fix`), one per
/// violation. They carry no `card_id` and their actor is `hook`, so they never count as
/// card activity for R3 or R5.
fn check_changes<'a>(
    results: impl IntoIterator<Item = &'a RuleResult>,
    verb: &'static str,
) -> Vec<Change> {
    results
        .into_iter()
        .flat_map(|r| r.violations.iter())
        .map(|v| {
            Change::board(
                verb,
                json!({ "rule": v.rule, "message": v.message, "text": v.text, "fix": v.fix }),
            )
            .by("hook")
        })
        .collect()
}

async fn record_checks(
    conn: &mut PgConnection,
    session_key: &str,
    results: &[RuleResult],
    verb: &'static str,
) -> Result<()> {
    let Some((tx, board)) = begin_active(conn, session_key).await? else {
        return Ok(());
    };
    finish(tx, &board, check_changes(results, verb)).await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// tool-used

/// `detail.target` of `tool.use` (docs/hooks.md section 3.4): the file path of file
/// tools, the first 80 characters of a `Bash`/`PowerShell` command, else empty.
pub fn tool_target(tool_name: &str, tool_input: &Value) -> String {
    let field = |k: &str| tool_input.get(k).and_then(Value::as_str);
    if let Some(path) = field("file_path").or_else(|| field("notebook_path")) {
        return path.to_string();
    }
    if matches!(tool_name, "Bash" | "PowerShell")
        && let Some(cmd) = field("command")
    {
        return write::excerpt(cmd);
    }
    String::new()
}

/// Whether a shell command runs `kb` itself (those already write their own activity).
pub fn is_kb_command(command: &str) -> bool {
    let mut s = command.trim_start();
    // PowerShell call operator: & kb ... / & 'C:\path\kb.exe' ...
    if let Some(rest) = s.strip_prefix('&') {
        s = rest.trim_start();
    }
    let first = if let Some(rest) = s.strip_prefix(['"', '\'']) {
        let quote = s.as_bytes()[0] as char;
        rest.split(quote).next().unwrap_or("")
    } else {
        s.split_whitespace().next().unwrap_or("")
    };
    let name = first.rsplit(['/', '\\']).next().unwrap_or(first);
    name.eq_ignore_ascii_case("kb") || name.eq_ignore_ascii_case("kb.exe")
}

/// `kb hook tool-used`: one `tool.use` row without bumping `rev` (no notification).
/// Returns false when nothing was written (no active board, or a `kb` command).
///
/// For a file edit tool whose file is scratch (the temporary folder or a hidden top-level
/// folder of home, unless the board's project folder is there too), the row records
/// `detail.scratch = true`; a relative path resolves against `cwd`, the hook input's folder
/// ([`crate::project::is_scratch`]). Rule R5 does not count those edits (docs/hooks.md
/// sections 3.4 and 4).
pub async fn tool_used(
    conn: &mut PgConnection,
    session_key: &str,
    tool_name: &str,
    tool_input: &Value,
    duration_ms: Option<i64>,
    cwd: Option<&str>,
) -> Result<bool> {
    if matches!(tool_name, "Bash" | "PowerShell")
        && tool_input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(is_kb_command)
    {
        return Ok(false);
    }
    let Some(project) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT cwd FROM board WHERE session_key = $1 AND status = 'active'",
    )
    .bind(session_key)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(false);
    };
    let target = tool_target(tool_name, tool_input);
    let mut detail = json!({
        "tool": tool_name,
        "target": target,
        "duration_ms": duration_ms,
    });
    if rules::is_file_edit_tool(tool_name)
        && crate::project::is_scratch(project.as_deref(), cwd, &target)
    {
        detail["scratch"] = json!(true);
    }
    let written = sqlx::query(
        "INSERT INTO activity (board_id, actor, verb, detail, turn_key)
         SELECT id, 'agent', 'tool.use', $2, turn_key FROM board
         WHERE session_key = $1 AND status = 'active'",
    )
    .bind(session_key)
    .bind(sqlx::types::Json(&detail))
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(written > 0)
}

// ---------------------------------------------------------------------------------------
// stop

#[derive(Debug, Clone)]
pub enum Stop {
    /// No active board: do nothing.
    NoBoard,
    /// Undelivered answers or comments: block with them; they are now marked delivered.
    Deliver(Delivery),
    /// Rules at level `block` failed; `check.block` was recorded.
    Block(Vec<RuleResult>),
    /// The agent may stop; `check.warn` (if any) and `turn.summary` were recorded.
    Pass {
        results: Vec<RuleResult>,
        summary: Value,
    },
    /// `stop_hook_active`: never blocks. Every failing rule not at level `off` was
    /// recorded as `check.warn` with `after_block`, and `turn.summary` was recorded
    /// (docs/board-v2.md section 16.1).
    AfterBlock {
        results: Vec<RuleResult>,
        summary: Value,
    },
}

/// `kb hook stop` when `stop_hook_active` is true (condition 1, docs/board-v2.md section
/// 16.1): a turn blocks at most once. Checks R2-R5 and R7-R11, records every violation of
/// a rule not at level `off` as `check.warn` with `detail.level` and
/// `detail.after_block = true`, and records `turn.summary`. Delivers nothing: undelivered answers and comments stay for the
/// waiter and the `prompt` hook.
pub async fn stop_after_block(
    conn: &mut PgConnection,
    session_key: &str,
    levels: &Levels,
) -> Result<Stop> {
    let Some((mut tx, board)) = begin_active(conn, session_key).await? else {
        return Ok(Stop::NoBoard);
    };
    let results = rules::check_turn(&mut tx, &board, levels).await?;
    // Rules at level `off` are not part of the outcome, as in the other conditions.
    let mut changes: Vec<Change> = results
        .iter()
        .filter(|r| r.level != Level::Off && !r.passed)
        .flat_map(|r| {
            r.violations.iter().map(move |v| {
                Change::board(
                    "check.warn",
                    json!({
                        "rule": v.rule, "message": v.message, "text": v.text, "fix": v.fix,
                        "level": r.level, "after_block": true,
                    }),
                )
                .by("hook")
            })
        })
        .collect();
    let summary = write::turn_summary(&mut tx, &board).await?;
    changes.push(Change::board("turn.summary", summary.clone()).by("hook"));
    finish(tx, &board, changes).await?;
    Ok(Stop::AfterBlock { results, summary })
}

/// `kb hook stop` after condition 1 (`stop_hook_active`, see [`stop_after_block`]):
/// 2 undelivered answers or comments, 3 blocking rules, 4 pass (docs/hooks.md section 3.5,
/// docs/board-v2.md section 13.8).
pub async fn stop(conn: &mut PgConnection, session_key: &str, levels: &Levels) -> Result<Stop> {
    let Some((mut tx, board)) = begin_active(conn, session_key).await? else {
        return Ok(Stop::NoBoard);
    };
    let mut changes = Vec::new();
    let delivery = deliver(&mut tx, board.id, &mut changes).await?;
    if !delivery.is_empty() {
        finish(tx, &board, changes).await?;
        return Ok(Stop::Deliver(delivery));
    }
    let results = rules::check_turn(&mut tx, &board, levels).await?;
    if results.iter().any(RuleResult::blocks) {
        let changes = check_changes(results.iter().filter(|r| r.blocks()), "check.block");
        finish(tx, &board, changes).await?;
        return Ok(Stop::Block(results));
    }
    let mut changes = check_changes(results.iter().filter(|r| r.warns()), "check.warn");
    let summary = write::turn_summary(&mut tx, &board).await?;
    changes.push(Change::board("turn.summary", summary.clone()).by("hook"));
    finish(tx, &board, changes).await?;
    Ok(Stop::Pass { results, summary })
}

// ---------------------------------------------------------------------------------------
// session-end

/// `kb hook session-end` (docs/hooks.md section 3.6): archives an active board
/// (`abandoned` without cards, else completed/incomplete) with summary
/// `session ended: <reason>`, and records `session.end`. Deletes the session's `kb_waiter`
/// row in any case (docs/board-v2.md section 20.4). Returns the outcome when the board
/// was archived; `Ok(None)` also when the session has no board.
pub async fn session_end(
    conn: &mut PgConnection,
    session_key: &str,
    reason: &str,
) -> Result<Option<Outcome>> {
    // First, and whether or not the session has a board: the session is over, so its waiter
    // row is stale (a waiter Claude Code killed could not delete it).
    crate::wait::forget_session(conn, session_key).await?;
    let (mut tx, board) = match write::begin(conn, session_key, true).await {
        Ok(v) => v,
        Err(Error::NoBoard(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut changes = vec![Change::board("session.end", json!({ "reason": reason })).by("hook")];
    let mut archived = None;
    if board.status == BoardStatus::Active {
        let cards: i64 = sqlx::query_scalar("SELECT count(*) FROM card WHERE board_id = $1")
            .bind(board.id)
            .fetch_one(&mut *tx)
            .await?;
        let outcome = if cards == 0 {
            Outcome::Abandoned
        } else {
            write::automatic_outcome(&mut tx, board.id).await?
        };
        let summary = format!("session ended: {reason}");
        write::archive_in_tx(&mut tx, board.id, outcome, Some(&summary)).await?;
        changes.push(Change::board("board.archive", json!({ "outcome": outcome })).by("hook"));
        archived = Some(outcome);
    }
    finish(tx, &board, changes).await?;
    Ok(archived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kb_skill_prompts() {
        for p in ["/kb", "/kb 修正登入逾時", "  /kb:kb", "/kb\ntitle"] {
            assert!(is_kb_skill_prompt(p), "{p}");
        }
        for p in ["/kbx", "kb", "please run /kb", "/k", ""] {
            assert!(!is_kb_skill_prompt(p), "{p}");
        }
    }

    #[test]
    fn reminder_is_one_line() {
        let r = Reminder {
            rule: "R2".into(),
            text: "R2: lane \"驗證\" has 2 cards, over its WIP limit of 1.".into(),
            fix: "kb card move <n> --to <lane>".into(),
            not_enforced: false,
        };
        assert_eq!(
            r.line(),
            "R2: lane \"驗證\" has 2 cards, over its WIP limit of 1. Fix: kb card move <n> --to <lane>"
        );
        // A rule that would have blocked after the turn's block (docs/board-v2.md 16.1).
        let r = Reminder {
            rule: "R4".into(),
            text: "R4: card 3 \"重現問題\" is done but has no result.".into(),
            fix: "kb card done 3 --result \"<result>\"".into(),
            not_enforced: true,
        };
        assert_eq!(
            r.line(),
            "R4: card 3 \"重現問題\" is done but has no result. (was not enforced: one block per turn) \
             Fix: kb card done 3 --result \"<result>\""
        );
        // R6 has a multi-line text and no fix.
        let r = Reminder {
            rule: "R6".into(),
            text: rules::R6_TEXT.into(),
            fix: String::new(),
            not_enforced: false,
        };
        assert_eq!(
            r.line(),
            "AskUserQuestion is disabled here. Ask through the board instead: \
             kb ask --card <n> \"<question>\" --option \"<label>::<description>\" --option \"<label>\" \
             Then continue with work that does not depend on the answer."
        );
    }
}
