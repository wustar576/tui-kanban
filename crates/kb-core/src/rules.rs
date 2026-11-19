//! Rules R1-R11 (docs/hooks.md section 4, docs/board-v2.md sections 17 and 19.4): levels from the
//! rules file and the checks.
//!
//! Checks only read, so they work on any connection (also inside a write transaction).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::postgres::PgConnection;

use crate::error::Result;
use crate::model::Board;

/// Environment variable that overrides the rules file path (used by tests).
pub const RULES_FILE_ENV: &str = "KB_RULES_FILE";

/// Tools that modify files (R1 and R5).
pub const FILE_EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The tool R6 replaces with `kb ask`.
pub const ASK_TOOL: &str = "AskUserQuestion";

pub fn is_file_edit_tool(tool: &str) -> bool {
    FILE_EDIT_TOOLS.contains(&tool)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum RuleId {
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    R8,
    R9,
    R10,
    R11,
}

impl RuleId {
    pub const ALL: [RuleId; 11] = [
        RuleId::R1,
        RuleId::R2,
        RuleId::R3,
        RuleId::R4,
        RuleId::R5,
        RuleId::R6,
        RuleId::R7,
        RuleId::R8,
        RuleId::R9,
        RuleId::R10,
        RuleId::R11,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RuleId::R1 => "R1",
            RuleId::R2 => "R2",
            RuleId::R3 => "R3",
            RuleId::R4 => "R4",
            RuleId::R5 => "R5",
            RuleId::R6 => "R6",
            RuleId::R7 => "R7",
            RuleId::R8 => "R8",
            RuleId::R9 => "R9",
            RuleId::R10 => "R10",
            RuleId::R11 => "R11",
        }
    }

    /// Default level (docs/hooks.md section 4; R3 is `off` per docs/board-v2.md section 9;
    /// R7-R10 per section 17; R11 per section 19.4).
    pub fn default_level(self) -> Level {
        match self {
            RuleId::R1
            | RuleId::R2
            | RuleId::R5
            | RuleId::R7
            | RuleId::R8
            | RuleId::R9
            | RuleId::R10
            | RuleId::R11 => Level::Warn,
            // R6: docs/board-v2.md section 23.2.
            RuleId::R4 | RuleId::R6 => Level::Block,
            RuleId::R3 => Level::Off,
        }
    }

    /// One-line English description, for `kb board check`.
    pub fn describe(self) -> &'static str {
        match self {
            RuleId::R1 => "the board has lanes and at least one card",
            RuleId::R2 => "no lane holds more cards than its WIP limit",
            RuleId::R3 => "every unblocked card in a doing lane has activity this turn",
            RuleId::R4 => "every card that entered a done lane this turn has a result",
            RuleId::R5 => "if files were edited this turn, some card has activity this turn",
            RuleId::R6 => "questions go through \"kb ask\", not AskUserQuestion",
            RuleId::R7 => "no card under discussion is in a doing lane",
            RuleId::R8 => "every card in a doing lane with an open question is blocked",
            RuleId::R9 => "every answer delivered this turn has a decision note on its card",
            RuleId::R10 => "cards moved from todo to doing this turn were the head of their queue",
            RuleId::R11 => "no finished card (done or dropped lane) has an open question",
        }
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Off,
    Warn,
    Block,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Warn => "warn",
            Level::Block => "block",
        }
    }

    fn parse(s: &str) -> Option<Level> {
        match s {
            "off" => Some(Level::Off),
            "warn" => Some(Level::Warn),
            "block" => Some(Level::Block),
            _ => None,
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The level of every rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels([Level; RuleId::ALL.len()]);

impl Default for Levels {
    fn default() -> Self {
        Levels(RuleId::ALL.map(RuleId::default_level))
    }
}

impl Levels {
    pub fn get(&self, rule: RuleId) -> Level {
        self.0[rule as usize]
    }

    pub fn set(&mut self, rule: RuleId, level: Level) {
        self.0[rule as usize] = level;
    }

    /// Path of the rules file: `$KB_RULES_FILE`, else `~/.config/kb/rules.toml`.
    pub fn file_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(RULES_FILE_ENV).filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(p));
        }
        crate::config::Config::file_path().map(|p| p.with_file_name("rules.toml"))
    }

    /// Loads the levels from the rules file. A missing file gives the defaults. Problems
    /// (unreadable file, invalid TOML, invalid values) fall back to defaults and are
    /// returned as warnings for standard error.
    pub fn load() -> (Levels, Vec<String>) {
        let Some(path) = Self::file_path() else {
            return (Levels::default(), Vec::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (levels, mut warnings) = Self::parse(&text);
                for w in &mut warnings {
                    *w = format!("{}: {w}", path.display());
                }
                (levels, warnings)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Levels::default(), Vec::new()),
            Err(e) => (
                Levels::default(),
                vec![format!(
                    "cannot read {}: {e}; using default rule levels",
                    path.display()
                )],
            ),
        }
    }

    /// Parses the rules file text (`[rules] R1 = "warn"` ...). Rules not in the file keep
    /// their default; unknown keys are ignored.
    pub fn parse(text: &str) -> (Levels, Vec<String>) {
        let mut levels = Levels::default();
        let mut warnings = Vec::new();
        let value: toml::Table = match toml::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!(
                    "invalid TOML ({}); using default rule levels",
                    e.message()
                ));
                return (levels, warnings);
            }
        };
        let Some(rules) = value.get("rules") else {
            return (levels, warnings);
        };
        let Some(rules) = rules.as_table() else {
            warnings.push("[rules] is not a table; using default rule levels".to_string());
            return (levels, warnings);
        };
        for rule in RuleId::ALL {
            let Some(v) = rules.get(rule.as_str()) else {
                continue;
            };
            match v.as_str().and_then(Level::parse) {
                Some(level) => levels.set(rule, level),
                None => warnings.push(format!(
                    "invalid level for {rule}: {v} (expected \"off\", \"warn\" or \"block\"); \
                     using \"{}\"",
                    rule.default_level()
                )),
            }
        }
        (levels, warnings)
    }
}

/// One failure of a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub rule: RuleId,
    /// The card concerned (R3, R4, R7-R11).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<i32>,
    /// English sentence for the agent, starting with the rule id.
    pub text: String,
    /// The command that fixes it.
    pub fix: String,
    /// Traditional Chinese explanation for humans (activity `detail.message`).
    pub message: String,
}

/// The outcome of one rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleResult {
    pub rule: RuleId,
    pub level: Level,
    /// False when the rule was not checked (R3, R5, R9 and R10 when the turn's range is
    /// unknown, docs/hooks.md section 4).
    pub checked: bool,
    pub passed: bool,
    pub description: &'static str,
    /// Why the rule was not checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<&'static str>,
    pub violations: Vec<Violation>,
}

impl RuleResult {
    fn new(rule: RuleId, levels: &Levels, violations: Vec<Violation>) -> Self {
        RuleResult {
            rule,
            level: levels.get(rule),
            checked: true,
            passed: violations.is_empty(),
            description: rule.describe(),
            skipped: None,
            violations,
        }
    }

    fn skipped(rule: RuleId, levels: &Levels, why: &'static str) -> Self {
        RuleResult {
            rule,
            level: levels.get(rule),
            checked: false,
            passed: true,
            description: rule.describe(),
            skipped: Some(why),
            violations: Vec::new(),
        }
    }

    /// Failed at level `block`.
    pub fn blocks(&self) -> bool {
        !self.passed && self.level == Level::Block
    }

    /// Failed at level `warn`.
    pub fn warns(&self) -> bool {
        !self.passed && self.level == Level::Warn
    }
}

const NO_PROMPT_YET: &str = "no user.prompt on this board yet (UserPromptSubmit hook not bound)";

/// `detail.source` of the `user.prompt` that `kb board start` writes when it starts the turn
/// (docs/board-v2.md section 12.4).
pub const BOARD_START_SOURCE: &str = "board.start";

/// `detail.source` of the `session.start` written after a compaction. Compaction can happen
/// in the middle of a turn, so that row does not start the rules' turn (docs/hooks.md
/// section 2.4).
pub const COMPACT_SOURCE: &str = "compact";

/// Where the rules' current turn starts (docs/hooks.md section 2.4): just after the board's
/// latest start marker, which is any of
/// - a `turn.summary` written by `kb hook stop` (actor `hook`; the one of `kb board tidy` is
///   written mid-turn by the agent and is not an end);
/// - a `session.start` whose `detail.source` is not `compact` (a session that starts, resumes,
///   is cleared or forked starts between turns; a compaction may come mid-turn);
/// - the `user.prompt` of `kb board start` (`detail.source` `board.start`; it is written only
///   when the board is created or reopened, so no row of the current turn precedes it).
///
/// Without any, at the board's start. Other `user.prompt` rows and `turn_started_at` do not
/// move it: a wake turn, a message sent while the agent is busy, and a turn interrupted with
/// Esc (no Stop) do not cut the range.
///
/// Activity rows compare by id: the stop hook writes its check rows and the summary in one
/// transaction, so they share `at`, and the summary is the transaction's last row; ids keep
/// that order, and a `tool.use` row a background hook writes after the stop belongs to the
/// next turn (as it would by time). Tables without an activity id (`card_note.created_at`,
/// the cards' `closed_at` and `updated_at`) compare with the marker's `at`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnStart {
    /// Activity rows with a larger id belong to the turn (0: every row of the board).
    pub after_id: i64,
    /// Notes and cards changed after this time belong to the turn (`None`: all of them).
    pub after_at: Option<DateTime<Utc>>,
}

/// The [`TurnStart`] of a board.
pub async fn turn_start(conn: &mut PgConnection, board_id: i64) -> Result<TurnStart> {
    let row: Option<(i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, at FROM activity
         WHERE board_id = $1
           AND ((verb = 'turn.summary' AND actor = 'hook')
                OR (verb = 'session.start' AND actor = 'hook'
                    AND (detail->>'source') IS DISTINCT FROM $2)
                OR (verb = 'user.prompt' AND detail->>'source' = $3))
         ORDER BY id DESC LIMIT 1",
    )
    .bind(board_id)
    .bind(COMPACT_SOURCE)
    .bind(BOARD_START_SOURCE)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map_or_else(TurnStart::default, |(id, at)| TurnStart {
        after_id: id,
        after_at: Some(at),
    }))
}

/// Whether the current turn's range is known (docs/hooks.md section 4), so R3, R5, R9 and R10
/// are checked: the board has a `user.prompt` written by `kb hook prompt` (the hook is
/// bound), or the current turn was started by `kb board start` and has no `turn.summary`
/// yet (that one turn is known even when the hook is not bound).
async fn turn_known(conn: &mut PgConnection, board: &Board) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM activity
                        WHERE board_id = $1 AND verb = 'user.prompt'
                          AND (detail->>'source') IS DISTINCT FROM $3)
             OR (EXISTS (SELECT 1 FROM activity
                         WHERE board_id = $1 AND verb = 'user.prompt'
                           AND detail->>'source' = $3 AND at >= $2)
                 AND NOT EXISTS (SELECT 1 FROM activity
                                 WHERE board_id = $1 AND verb = 'turn.summary' AND at >= $2))",
    )
    .bind(board.id)
    .bind(board.turn_started_at)
    .bind(BOARD_START_SOURCE)
    .fetch_one(&mut *conn)
    .await?)
}

/// R1: the board has lanes and at least one card.
pub async fn check_r1(
    conn: &mut PgConnection,
    board: &Board,
    levels: &Levels,
) -> Result<RuleResult> {
    let (lanes, cards): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM lane WHERE board_id = $1),
                (SELECT count(*) FROM card WHERE board_id = $1)",
    )
    .bind(board.id)
    .fetch_one(&mut *conn)
    .await?;
    let mut v = Vec::new();
    if lanes == 0 || cards == 0 {
        v.push(Violation {
            rule: RuleId::R1,
            card: None,
            text: "R1: the board needs lanes and at least one card before files are edited."
                .to_string(),
            fix: "kb board init --title \"<title>\" --lanes \"<name>:todo,<name>:doing,<name>:done\" ; \
                  kb card add --lane <lane> --title \"<title>\""
                .to_string(),
            message: if lanes == 0 {
                "看板沒有泳道就修改檔案".to_string()
            } else {
                "看板沒有卡片就修改檔案".to_string()
            },
        });
    }
    Ok(RuleResult::new(RuleId::R1, levels, v))
}

/// R6: the agent tried to use `AskUserQuestion`. Always a violation; the caller decides
/// whether the tool is `AskUserQuestion`.
pub fn r6_violation(levels: &Levels) -> RuleResult {
    RuleResult::new(
        RuleId::R6,
        levels,
        vec![Violation {
            rule: RuleId::R6,
            card: None,
            text: R6_TEXT.to_string(),
            fix: String::new(),
            message: "使用了 AskUserQuestion，應改用 kb ask".to_string(),
        }],
    )
}

/// Text given to the agent when R6 blocks (docs/hooks.md section 4.1; docs/board-v2.md
/// section 23.4).
pub const R6_TEXT: &str = "AskUserQuestion is disabled here. Ask through the board instead:\n  \
kb ask --card <n> \"<question>\" --option \"<label>::<description>\" --option \"<label>\"\n\
If no card covers the question, add one first: kb card add --lane <lane> --title \"<topic>\" --discuss\n\
Then continue with work that does not depend on the answer.";

/// R2-R5 and R7-R11 for the board's current turn (the `stop` hook and `kb board check`),
/// in rule order.
pub async fn check_turn(
    conn: &mut PgConnection,
    board: &Board,
    levels: &Levels,
) -> Result<Vec<RuleResult>> {
    let start = turn_start(conn, board.id).await?;
    let turn_known = board.turn_started_at.is_some() && turn_known(conn, board).await?;

    // R2: WIP limits.
    let over: Vec<(String, i64, i32)> = sqlx::query_as(
        "SELECT l.name, count(c.id), l.wip_limit
         FROM lane l LEFT JOIN card c ON c.lane_id = l.id
         WHERE l.board_id = $1 AND l.wip_limit IS NOT NULL
         GROUP BY l.id HAVING count(c.id) > l.wip_limit
         ORDER BY l.position",
    )
    .bind(board.id)
    .fetch_all(&mut *conn)
    .await?;
    let r2 = over
        .into_iter()
        .map(|(lane, count, limit)| Violation {
            rule: RuleId::R2,
            card: None,
            text: format!("R2: lane \"{lane}\" has {count} cards, over its WIP limit of {limit}."),
            fix: format!("kb card move <n> --to <lane>  (move cards out of \"{lane}\")"),
            message: format!("泳道「{lane}」超過上限（{count}/{limit}）"),
        })
        .collect();

    // R3: unblocked cards in doing lanes without card activity this turn. Card activity is a
    // row the agent wrote (actor `agent`), other than the deliveries an agent's `kb inbox`
    // writes; rows of hooks and of the user do not count.
    let r3 = if turn_known {
        let idle: Vec<(i32, String)> = sqlx::query_as(
            "SELECT c.seq, c.title FROM card c JOIN lane l ON l.id = c.lane_id
             WHERE c.board_id = $1 AND l.kind = 'doing' AND c.blocked_reason IS NULL
               AND NOT EXISTS (SELECT 1 FROM activity a
                               WHERE a.board_id = $1 AND a.card_id = c.id AND a.id > $2
                                 AND a.actor = 'agent'
                                 AND a.verb NOT IN ('question.deliver', 'comment.deliver'))
             ORDER BY l.position, c.position, c.seq",
        )
        .bind(board.id)
        .bind(start.after_id)
        .fetch_all(&mut *conn)
        .await?;
        let v = idle
            .into_iter()
            .map(|(seq, title)| Violation {
                rule: RuleId::R3,
                card: Some(seq),
                text: format!(
                    "R3: card {seq} \"{title}\" is in a doing lane but has no activity this turn."
                ),
                fix: format!("kb card note {seq} \"<what happened>\"  (or move/block it)"),
                message: format!("#{seq} 這一輪沒有紀錄"),
            })
            .collect();
        RuleResult::new(RuleId::R3, levels, v)
    } else {
        RuleResult::skipped(RuleId::R3, levels, NO_PROMPT_YET)
    };

    // R4: cards that entered a done lane this turn need a result. When the board has no start
    // marker of a turn yet, every card in a done lane is checked.
    let missing: Vec<(i32, String)> = sqlx::query_as(
        "SELECT c.seq, c.title FROM card c JOIN lane l ON l.id = c.lane_id
         WHERE c.board_id = $1 AND l.kind = 'done'
           AND coalesce(c.closed_at, c.updated_at) > coalesce($2, '-infinity')
           AND (c.result IS NULL OR btrim(c.result) = '')
         ORDER BY l.position, c.position, c.seq",
    )
    .bind(board.id)
    .bind(start.after_at)
    .fetch_all(&mut *conn)
    .await?;
    let r4 = missing
        .into_iter()
        .map(|(seq, title)| Violation {
            rule: RuleId::R4,
            card: Some(seq),
            text: format!("R4: card {seq} \"{title}\" is done but has no result."),
            fix: format!("kb card done {seq} --result \"<result>\""),
            message: format!("#{seq} 完成但沒有結果"),
        })
        .collect();

    // R5: files edited this turn, but no card activity (as for R3) this turn. A scratch edit
    // (`detail.scratch` true) does not count, nor a row written before that field with
    // `detail.in_project` false; rows with neither field count.
    let r5 = if turn_known {
        let (edits, card_rows): (i64, i64) = sqlx::query_as(
            "SELECT
               (SELECT count(*) FROM activity
                 WHERE board_id = $1 AND id > $2 AND verb = 'tool.use'
                   AND detail->>'tool' = ANY($3)
                   AND (detail->'scratch') IS DISTINCT FROM 'true'::jsonb
                   AND (detail->'in_project') IS DISTINCT FROM 'false'::jsonb),
               (SELECT count(*) FROM activity
                 WHERE board_id = $1 AND id > $2 AND card_id IS NOT NULL
                   AND actor = 'agent'
                   AND verb NOT IN ('question.deliver', 'comment.deliver'))",
        )
        .bind(board.id)
        .bind(start.after_id)
        .bind(&FILE_EDIT_TOOLS[..])
        .fetch_one(&mut *conn)
        .await?;
        let v = if edits > 0 && card_rows == 0 {
            vec![Violation {
                rule: RuleId::R5,
                card: None,
                text: format!(
                    "R5: files were edited this turn ({edits} edits) but no card has activity this turn."
                ),
                fix: "kb card note <n> \"<what changed>\"  (or add a card for this work)"
                    .to_string(),
                message: format!("這一輪修改了 {edits} 次檔案，但沒有任何卡片的紀錄"),
            }]
        } else {
            Vec::new()
        };
        RuleResult::new(RuleId::R5, levels, v)
    } else {
        RuleResult::skipped(RuleId::R5, levels, NO_PROMPT_YET)
    };

    let mut results = vec![
        RuleResult::new(RuleId::R2, levels, r2),
        r3,
        RuleResult::new(RuleId::R4, levels, r4),
        r5,
    ];
    results.extend(check_r7_to_r10(conn, board, levels, turn_known, start).await?);
    Ok(results)
}

/// `<id>` or `<id>, <id>` / `<id>、<id>`.
fn join_ids(ids: &[i64], sep: &str) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(sep)
}

/// R7-R11 (docs/board-v2.md sections 17 and 19.4). R7, R8 and R11 look at the board's
/// current state; R9 and R10 at the current turn and are skipped when it is unknown.
async fn check_r7_to_r10(
    conn: &mut PgConnection,
    board: &Board,
    levels: &Levels,
    turn_known: bool,
    start: TurnStart,
) -> Result<Vec<RuleResult>> {
    // R7: cards under discussion in a doing lane.
    let discussing: Vec<(i32, String)> = sqlx::query_as(
        "SELECT c.seq, c.title FROM card c JOIN lane l ON l.id = c.lane_id
         WHERE c.board_id = $1 AND l.kind = 'doing' AND c.origin = 'discussing'
         ORDER BY l.position, c.position, c.seq",
    )
    .bind(board.id)
    .fetch_all(&mut *conn)
    .await?;
    let r7 = discussing
        .into_iter()
        .map(|(seq, title)| Violation {
            rule: RuleId::R7,
            card: Some(seq),
            text: format!(
                "R7: card {seq} \"{title}\" is still under discussion but is in a doing lane."
            ),
            fix: format!(
                "kb card confirm {seq}  (once the user agreed; otherwise move it back: \
                 kb card move {seq} --to <lane>)"
            ),
            message: format!("#{seq} 還在討論中，卻在進行中的泳道"),
        })
        .collect();

    // R8: cards in a doing lane with an open question that are not blocked: no blocked
    // reason and no unfinished card blocking them through a `blocks` link (section 10.2).
    let waiting: Vec<(i32, String, Vec<i64>)> = sqlx::query_as(
        "SELECT c.seq, c.title, array_agg(q.id ORDER BY q.id)
         FROM card c
         JOIN lane l ON l.id = c.lane_id
         JOIN question q ON q.card_id = c.id AND q.status = 'open'
         WHERE c.board_id = $1 AND l.kind = 'doing' AND c.blocked_reason IS NULL
           AND NOT EXISTS (SELECT 1 FROM link k
                           JOIN card f ON f.id = k.from_card_id
                           JOIN lane fl ON fl.id = f.lane_id
                           WHERE k.to_card_id = c.id AND k.kind = 'blocks'
                             AND fl.kind NOT IN ('done', 'dropped'))
         GROUP BY c.id, l.position
         ORDER BY l.position, c.position, c.seq",
    )
    .bind(board.id)
    .fetch_all(&mut *conn)
    .await?;
    let r8 = waiting
        .into_iter()
        .map(|(seq, title, ids)| {
            let first = ids.first().copied().unwrap_or_default();
            let which = if ids.len() == 1 {
                format!("open question {first}")
            } else {
                format!("open questions {}", join_ids(&ids, ", "))
            };
            Violation {
                rule: RuleId::R8,
                card: Some(seq),
                text: format!(
                    "R8: card {seq} \"{title}\" is in a doing lane with {which} but is not blocked."
                ),
                fix: format!(
                    "kb card block {seq} --reason \"waiting for answer to question {first}\""
                ),
                message: format!(
                    "#{seq} 有未回答的提問 {}，卻沒有標記阻擋",
                    join_ids(&ids, "、")
                ),
            }
        })
        .collect();

    // R9: answers delivered this turn without a decision note on the card since. Answers
    // of the "explain in the session" kind (status chat) carry no content: skipped.
    let r9 = if turn_known {
        let undecided: Vec<(i64, i32, String)> = sqlx::query_as(
            "SELECT q.id, c.seq, c.title FROM question q JOIN card c ON c.id = q.card_id
             WHERE q.board_id = $1 AND q.status = 'answered'
               AND EXISTS (SELECT 1 FROM activity a
                           WHERE a.board_id = $1 AND a.verb = 'question.deliver'
                             AND a.id > $2 AND a.detail->>'question' = q.id::text
                             AND NOT EXISTS (SELECT 1 FROM card_note n
                                             WHERE n.card_id = q.card_id
                                               AND n.kind = 'decision'
                                               AND n.created_at >= a.at))
             ORDER BY q.id",
        )
        .bind(board.id)
        .bind(start.after_id)
        .fetch_all(&mut *conn)
        .await?;
        let v = undecided
            .into_iter()
            .map(|(id, seq, title)| Violation {
                rule: RuleId::R9,
                card: Some(seq),
                text: format!(
                    "R9: the answer to question {id} on card {seq} \"{title}\" was delivered \
                     this turn, but the card has no decision note since."
                ),
                fix: format!("kb card note {seq} --kind decision \"<what was decided>\""),
                message: format!("#{seq} 收到提問 {id} 的回答，但沒有記下決定"),
            })
            .collect();
        RuleResult::new(RuleId::R9, levels, v)
    } else {
        RuleResult::skipped(RuleId::R9, levels, NO_PROMPT_YET)
    };

    // R10: moves from a todo lane into a doing lane this turn that skipped the head of the
    // queue. The head (the first ready card: neither blocked nor under discussion) was
    // recorded by the move (`detail.queue_head`, section 17.2); moves without it (no head, or
    // recorded before) are not checked.
    let r10 = if turn_known {
        let skipped: Vec<(i32, String, String, String, i32)> = sqlx::query_as(
            "SELECT (a.detail->>'seq')::int, coalesce(c.title, ''),
                    coalesce(a.detail->>'from', ''), coalesce(a.detail->>'to', ''),
                    (a.detail->>'queue_head')::int
             FROM activity a LEFT JOIN card c ON c.id = a.card_id
             WHERE a.board_id = $1 AND a.verb = 'card.move' AND a.id > $2
               AND jsonb_typeof(a.detail->'queue_head') = 'number'
               AND (a.detail->>'queue_head')::int <> (a.detail->>'seq')::int
             ORDER BY a.id",
        )
        .bind(board.id)
        .bind(start.after_id)
        .fetch_all(&mut *conn)
        .await?;
        let v = skipped
            .into_iter()
            .map(|(seq, title, from, to, head)| Violation {
                rule: RuleId::R10,
                card: Some(seq),
                text: format!(
                    "R10: card {seq} \"{title}\" moved from \"{from}\" to \"{to}\" this turn, \
                     but the head of that queue was #{head}."
                ),
                fix: format!(
                    "kb card order <n> <n> ...  (the head was #{head}; \
                     fix the order if the queue was wrong)"
                ),
                message: format!("#{seq} 跳過「{from}」的第一張 #{head} 就開始"),
            })
            .collect();
        RuleResult::new(RuleId::R10, levels, v)
    } else {
        RuleResult::skipped(RuleId::R10, levels, NO_PROMPT_YET)
    };

    // R11: finished cards with an open question (section 19.4).
    let r11 = check_r11(conn, board.id).await?;

    Ok(vec![
        RuleResult::new(RuleId::R7, levels, r7),
        RuleResult::new(RuleId::R8, levels, r8),
        r9,
        r10,
        RuleResult::new(RuleId::R11, levels, r11),
    ])
}

/// R11 (docs/board-v2.md section 19.4): cards in a done or dropped lane with an open
/// question, one violation per card.
async fn check_r11(conn: &mut PgConnection, board_id: i64) -> Result<Vec<Violation>> {
    let finished: Vec<(i32, String, Vec<i64>)> = sqlx::query_as(
        "SELECT c.seq, c.title, array_agg(q.id ORDER BY q.id)
         FROM card c
         JOIN lane l ON l.id = c.lane_id
         JOIN question q ON q.card_id = c.id AND q.status = 'open'
         WHERE c.board_id = $1 AND l.kind IN ('done', 'dropped')
         GROUP BY c.id, l.position
         ORDER BY l.position, c.position, c.seq",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(finished
        .into_iter()
        .map(|(seq, title, ids)| {
            let first = ids.first().copied().unwrap_or_default();
            Violation {
                rule: RuleId::R11,
                card: Some(seq),
                text: format!(
                    "R11: card {seq} \"{title}\" is finished but {}.",
                    still_open(&ids)
                ),
                fix: format!(
                    "{}  (if it was answered in the session; otherwise move the card back: \
                     kb card move {seq} --to <lane>)",
                    r11_cancel(first)
                ),
                message: format!("#{seq} 已經結束，提問 {} 仍然待回答", join_ids(&ids, "、")),
            }
        })
        .collect())
}

/// The command R11's fix and its immediate echo both suggest (docs/board-v2.md 20.3).
fn r11_cancel(question: i64) -> String {
    format!("kb question cancel {question} --reason \"answered in the session\"")
}

/// `question 19 is still open` / `questions 19, 20 are still open`.
fn still_open(ids: &[i64]) -> String {
    if ids.len() == 1 {
        format!("question {} is still open", ids[0])
    } else {
        format!("questions {} are still open", join_ids(ids, ", "))
    }
}

/// R11's immediate echo for `kb card done` and `kb card drop` (docs/board-v2.md section
/// 19.4); `None` when the card has no open question. Printed whatever R11's level is. It
/// suggests the same command as R11's fix (section 20.3).
pub async fn r11_echo(conn: &mut PgConnection, card_id: i64, seq: i32) -> Result<Option<String>> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM question WHERE card_id = $1 AND status = 'open' ORDER BY id",
    )
    .bind(card_id)
    .fetch_all(&mut *conn)
    .await?;
    let Some(first) = ids.first() else {
        return Ok(None);
    };
    Ok(Some(format!(
        "R11: card {seq} is finished but {}; if it was answered in the session, run: {}",
        still_open(&ids),
        r11_cancel(*first)
    )))
}

/// R1-R5 and R7-R11 for `kb board check` (docs/hooks.md section 5.1).
pub async fn check_board(
    conn: &mut PgConnection,
    board: &Board,
    levels: &Levels,
) -> Result<Vec<RuleResult>> {
    let mut results = vec![check_r1(conn, board, levels).await?];
    results.extend(check_turn(conn, board, levels).await?);
    Ok(results)
}

/// The text given to the agent when rules block (docs/hooks.md section 4.1): a header,
/// then each violation of a blocking rule with its fix.
pub fn block_text(results: &[RuleResult], header: &str) -> String {
    let mut s = header.to_string();
    for r in results.iter().filter(|r| r.blocks()) {
        for v in &r.violations {
            s.push('\n');
            s.push_str(&v.text);
            if !v.fix.is_empty() {
                s.push_str("\n    Fix: ");
                s.push_str(&v.fix);
            }
        }
    }
    s
}

/// Header of the `stop` block text.
pub const STOP_HEADER: &str = "Kanban rules failed. Fix them, then stop again.";

/// Header of the `pre-tool` block text for R1.
pub const PRE_TOOL_HEADER: &str = "Kanban rules failed. Fix them, then try again.";
