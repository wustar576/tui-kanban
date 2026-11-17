//! The model judge, `kb hook judge` (docs/hooks.md section 3.7, docs/behaviour-checks.md):
//! after a turn ends, a decision model answers questions about the agent's last message;
//! findings are recorded as `judge.finding` rows, and a finding in the wake band wakes the
//! agent once per turn.
//!
//! - [`config`]: the `[judge]` table of the rules file.
//! - [`questions`]: the wording, the turn description, the findings, the wake text.
//! - [`provider`]: the model behind a small trait; one implementation (Gemini Decisions).
//!
//! Everything is stored as `activity` rows; there is no table of its own.

pub mod config;
pub mod provider;
pub mod questions;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqlx::postgres::PgConnection;

use crate::error::{Error, Result};
use crate::model::{BoardStatus, Origin};
use crate::read;
use crate::write::{self, Change};

pub use config::{JudgeConfig, QuestionConfig};
pub use provider::{GeminiDecisions, ModelAnswer, ModelAnswers, ModelQuestion, Provider};
pub use questions::{Band, CardFacts, Finding, Topic};

pub const RUN_VERB: &str = "judge.run";
pub const FINDING_VERB: &str = "judge.finding";

/// Most characters of the prompt text `kb hook prompt` stores in `user.prompt`
/// `detail.text`.
pub const PROMPT_TEXT_LIMIT: usize = 4000;

/// The `detail.text` of `user.prompt`: the prompt, at most [`PROMPT_TEXT_LIMIT`]
/// characters.
pub fn prompt_text(prompt: &str) -> String {
    prompt.chars().take(PROMPT_TEXT_LIMIT).collect()
}

/// How often the judge looks for the turn's `turn.summary`.
const POLL: Duration = Duration::from_millis(200);

/// Agent rows that count as a write on a card for the `advice` question.
const CARD_WRITES: [&str; 10] = [
    "card.add",
    "card.note",
    "card.done",
    "card.drop",
    "card.move",
    "card.origin",
    "card.block",
    "card.unblock",
    "card.edit",
    "question.ask",
];

/// Agent rows that are not board commands in the turn description.
const NOT_COMMANDS: [&str; 4] = ["tool.use", "user.prompt", "question.deliver", "comment.deliver"];

/// How a run of the judge ended.
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeEnd {
    /// Nothing judged and nothing written: no active board, the turn did not end
    /// (no `turn.summary`), another judge has this turn, or no question to ask.
    Skipped,
    /// The `judge.run` row (and the findings) were written.
    Judged {
        findings: usize,
        /// The model call failed (`detail.error`).
        error: Option<String>,
        /// The text that wakes the agent (standard error, exit code 2).
        wake: Option<String>,
    },
}

/// The newest `turn.summary` of `kb hook stop` (actor `hook`; `kb board tidy` writes one as
/// the agent mid-turn) written after the board's latest `user.prompt` and its latest
/// `judge.run`: the end of a turn not judged yet.
async fn pending_summary(conn: &mut PgConnection, board_id: i64) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM activity
         WHERE board_id = $1 AND verb = 'turn.summary' AND actor = 'hook'
           AND id > coalesce((SELECT max(id) FROM activity
                              WHERE board_id = $1 AND verb IN ('user.prompt', 'judge.run')), 0)
         ORDER BY id DESC LIMIT 1",
    )
    .bind(board_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Judges the turn that just ended (docs/hooks.md section 3.7). The model call goes through
/// `provider` (blocking). Writes nothing when it returns [`JudgeEnd::Skipped`].
pub async fn judge_turn(
    conn: &mut PgConnection,
    session_key: &str,
    final_message: &str,
    config: &JudgeConfig,
    provider: &dyn Provider,
) -> Result<JudgeEnd> {
    let Some(board) = read::board_by_session(conn, session_key).await? else {
        return Ok(JudgeEnd::Skipped);
    };
    if board.status != BoardStatus::Active {
        return Ok(JudgeEnd::Skipped);
    }
    // Hooks of one event run in parallel: wait for `kb hook stop` to end the turn.
    let deadline = Instant::now() + Duration::from_millis(config.summary_wait_ms);
    let summary = loop {
        if let Some(id) = pending_summary(conn, board.id).await? {
            break id;
        }
        if Instant::now() >= deadline {
            return Ok(JudgeEnd::Skipped);
        }
        tokio::time::sleep(POLL).await;
    };
    // One judge per summary: a lock in its own key space (the waiter's is `kb_waiter:`).
    let lock: i64 = sqlx::query_scalar("SELECT hashtextextended($1, 0)")
        .bind(format!("kb_judge:{summary}"))
        .fetch_one(&mut *conn)
        .await?;
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lock)
        .fetch_one(&mut *conn)
        .await?;
    if !got {
        return Ok(JudgeEnd::Skipped);
    }
    let end = judge_locked(conn, session_key, board.id, summary, final_message, config, provider)
        .await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(lock)
        .execute(&mut *conn)
        .await;
    end
}

#[derive(sqlx::FromRow)]
struct SegmentRow {
    actor: String,
    verb: String,
    detail: Value,
    card_id: Option<i64>,
    seq: Option<i32>,
}

async fn judge_locked(
    conn: &mut PgConnection,
    session_key: &str,
    board_id: i64,
    summary: i64,
    final_message: &str,
    config: &JudgeConfig,
    provider: &dyn Provider,
) -> Result<JudgeEnd> {
    let judged: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM activity
                        WHERE board_id = $1 AND verb = 'judge.run' AND id > $2)",
    )
    .bind(board_id)
    .bind(summary)
    .fetch_one(&mut *conn)
    .await?;
    if judged {
        return Ok(JudgeEnd::Skipped);
    }
    let Some(snapshot) = read::board_snapshot(conn, board_id).await? else {
        return Ok(JudgeEnd::Skipped);
    };

    // The segment: rows after the later of the latest user.prompt and judge.run.
    let start: Option<(i64, String, Value)> = sqlx::query_as(
        "SELECT id, verb, detail FROM activity
         WHERE board_id = $1 AND verb IN ('user.prompt', 'judge.run') AND id < $2
         ORDER BY id DESC LIMIT 1",
    )
    .bind(board_id)
    .bind(summary)
    .fetch_optional(&mut *conn)
    .await?;
    let start_id = start.as_ref().map_or(0, |s| s.0);
    let user_message = match &start {
        Some((_, verb, _)) if verb == RUN_VERB => questions::UserMessage::Notification,
        Some((_, _, d)) if d["source"].as_str() == Some(crate::rules::BOARD_START_SOURCE) => {
            questions::UserMessage::BoardStart
        }
        Some((_, _, d)) => match d["text"].as_str().filter(|t| !t.trim().is_empty()) {
            Some(t) => questions::UserMessage::Text(t.to_string()),
            None => questions::UserMessage::NotRecorded,
        },
        None => questions::UserMessage::NotRecorded,
    };
    let rows: Vec<SegmentRow> = sqlx::query_as(
        "SELECT a.actor, a.verb, a.detail, a.card_id, c.seq
         FROM activity a LEFT JOIN card c ON c.id = a.card_id
         WHERE a.board_id = $1 AND a.id > $2 AND a.id <= $3
         ORDER BY a.id",
    )
    .bind(board_id)
    .bind(start_id)
    .bind(summary)
    .fetch_all(&mut *conn)
    .await?;

    let mut delivered = Vec::new();
    let mut tools = Vec::new();
    let mut commands = Vec::new();
    let mut written = BTreeSet::new();
    let mut left_discussing = BTreeSet::new();
    for r in &rows {
        match r.verb.as_str() {
            "question.deliver" => {
                if let Some(qid) = r.detail["question"].as_i64()
                    && let Some(q) = read::question(conn, board_id, qid).await?
                {
                    delivered.push(answer_line(&q));
                }
            }
            "comment.deliver" => {
                if let Some(nid) = r.detail["note"].as_i64()
                    && let Some((body, seq)) = sqlx::query_as::<_, (String, i32)>(
                        "SELECT n.body, c.seq FROM card_note n JOIN card c ON c.id = n.card_id
                         WHERE n.id = $1 AND c.board_id = $2",
                    )
                    .bind(nid)
                    .bind(board_id)
                    .fetch_optional(&mut *conn)
                    .await?
                {
                    delivered.push(questions::one_line(
                        &format!("comment {nid} on card #{seq}: {body}"),
                        600,
                    ));
                }
            }
            "tool.use" => tools.push((
                r.detail["tool"].as_str().unwrap_or("").to_string(),
                r.detail["target"].as_str().unwrap_or("").to_string(),
            )),
            _ => {}
        }
        if r.actor != "agent" || NOT_COMMANDS.contains(&r.verb.as_str()) {
            continue;
        }
        commands.push(questions::board_command_line(&r.verb, r.seq, &r.detail));
        if let Some(id) = r.card_id
            && CARD_WRITES.contains(&r.verb.as_str())
        {
            written.insert(id);
        }
        if r.verb == "card.origin"
            && r.detail["from"].as_str() == Some(Origin::Discussing.as_str())
            && let Some(id) = r.card_id
        {
            left_discussing.insert(id);
        }
    }

    let last: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT card_id, max(id) FROM activity
         WHERE board_id = $1 AND card_id IS NOT NULL GROUP BY card_id",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?;
    let cards: Vec<CardFacts> = snapshot
        .lanes
        .iter()
        .flat_map(|l| l.cards.iter().map(move |c| (l.lane.kind, c)))
        .map(|(kind, c)| {
            let finished = kind.is_closed();
            CardFacts {
                id: c.card.id,
                seq: c.card.seq,
                title: c.card.title.clone(),
                finished,
                discussing: (c.card.origin == Origin::Discussing && !finished)
                    || left_discussing.contains(&c.card.id),
                blocked: c.is_blocked(),
                open_questions: c.open_question_count,
                written: written.contains(&c.card.id),
                last_activity: last
                    .iter()
                    .find(|(id, _)| *id == c.card.id)
                    .map_or(0, |(_, a)| *a),
            }
        })
        .collect();

    let plan = questions::plan(&cards, final_message, config);
    if plan.questions.is_empty() {
        return Ok(JudgeEnd::Skipped);
    }
    let input = questions::input_text(&questions::TurnText {
        user_message,
        delivered,
        tools,
        board_commands: commands,
        final_message: final_message.to_string(),
    });
    let started = Instant::now();
    let (findings, ms, error) = match provider.decide(&input, &plan.questions) {
        Ok(answers) => (
            questions::findings(&plan, &answers, &cards, config),
            answers.ms,
            None,
        ),
        Err(e) => (Vec::new(), started.elapsed().as_millis() as u64, Some(e)),
    };

    // Record: one transaction under the board's lock, `rev + 1` (docs/hooks.md 3.7).
    let (mut tx, board) = match write::begin(conn, session_key, false).await {
        Ok(v) => v,
        Err(Error::NoBoard(_) | Error::BoardArchived) => return Ok(JudgeEnd::Skipped),
        Err(e) => return Err(e),
    };
    let woke_before: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM activity
                        WHERE board_id = $1 AND verb = 'judge.finding'
                          AND turn_key IS NOT DISTINCT FROM $2
                          AND detail->>'woke' = 'true')",
    )
    .bind(board.id)
    .bind(&board.turn_key)
    .fetch_one(&mut *tx)
    .await?;
    let wake_now = !woke_before && findings.iter().any(|f| f.band == Band::Wake);
    let woke = |f: &Finding| wake_now && f.band == Band::Wake;
    let mut changes: Vec<Change> = findings
        .iter()
        .map(|f| {
            let detail = json!({
                "question": f.topic.as_str(),
                "p": f.p,
                "band": f.band.as_str(),
                "woke": woke(f),
                "text": f.text,
                "message": f.message,
            });
            match f.card {
                Some((id, _)) => Change::card(id, FINDING_VERB, detail),
                None => Change::board(FINDING_VERB, detail),
            }
            .by("hook")
        })
        .collect();
    let mut run = json!({
        "questions": plan.questions.len(),
        "findings": findings.len(),
        "ms": ms,
        "input_chars": input.chars().count(),
        "model": config.model,
    });
    if plan.left_out > 0 {
        run["left_out"] = json!(plan.left_out);
    }
    if let Some(e) = &error {
        run["error"] = json!(e);
    }
    changes.push(Change::board(RUN_VERB, run).by("hook"));
    write::commit(tx, &board, changes, String::new()).await?;

    let woken: Vec<&Finding> = findings.iter().filter(|f| woke(f)).collect();
    Ok(JudgeEnd::Judged {
        findings: findings.len(),
        error,
        wake: (!woken.is_empty()).then(|| questions::wake_text(&woken)),
    })
}

/// One line for a delivered answer in the turn description.
fn answer_line(q: &read::QuestionView) -> String {
    use crate::model::QuestionStatus;
    let answer = match q.question.status {
        QuestionStatus::Chat => "(the user will reply in the session)".to_string(),
        _ => {
            let labels = q.selected_labels().join(", ");
            match q.question.answer_text.as_deref().map(str::trim) {
                Some(t) if !t.is_empty() && labels.is_empty() => t.to_string(),
                Some(t) if !t.is_empty() => format!("{labels}; text: {t}"),
                _ => labels,
            }
        }
    };
    questions::one_line(
        &format!(
            "answer to question {} on card #{} (\"{}\"): {answer}",
            q.question.id, q.card_seq, q.question.text
        ),
        600,
    )
}
