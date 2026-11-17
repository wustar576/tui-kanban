//! Rows and enumerations of docs/data-model.md.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::Error;

/// Declares a fieldless enum stored as `text` in PostgreSQL, with serde, `FromStr`,
/// `Display` and sqlx `Type`/`Encode`/`Decode` all using the same lowercase spelling.
macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident ($label:literal) { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self, Error> {
                match s {
                    $($text => Ok($name::$variant),)+
                    _ => {
                        let allowed: Vec<&str> = vec![$($text),+];
                        Err(Error::invalid(format!(
                            "invalid {}: {s} (expected one of: {})",
                            $label,
                            allowed.join(", ")
                        )))
                    }
                }
            }
        }

        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <String as sqlx::Type<sqlx::Postgres>>::type_info()
            }
            fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
            }
        }

        impl<'q> sqlx::Encode<'q, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
                <&str as sqlx::Encode<'q, sqlx::Postgres>>::encode(self.as_str(), buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
                let text = <&str as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)?;
                Ok(text.parse::<$name>()?)
            }
        }
    };
}

text_enum!(
    /// Where a workspace comes from (`workspace.source`).
    WorkspaceSource ("workspace source") { Herdr => "herdr", Dir => "dir", Manual => "manual" }
);

text_enum!(
    /// `board.status`.
    BoardStatus ("board status") { Active => "active", Archived => "archived" }
);

text_enum!(
    /// `board.outcome`, set when a board is archived.
    Outcome ("outcome") { Completed => "completed", Incomplete => "incomplete", Abandoned => "abandoned" }
);

text_enum!(
    /// `lane.kind`: the fixed category behind a freely named lane.
    LaneKind ("lane kind") { Todo => "todo", Doing => "doing", Done => "done", Dropped => "dropped" }
);

text_enum!(
    /// `card_note.kind`. `comment` is written only by the human, through the database
    /// function `kb_add_comment` (docs/board-v2.md section 13.7); the agent answers it with
    /// a `reply`.
    NoteKind ("note kind") {
        Progress => "progress",
        Decision => "decision",
        Blocker => "blocker",
        Result => "result",
        Reply => "reply",
        Comment => "comment",
    }
);

text_enum!(
    /// `link.kind`. `Ref` points at an external target; all others at a card.
    LinkKind ("link kind") {
        Spawned => "spawned",
        Blocks => "blocks",
        Relates => "relates",
        Continues => "continues",
        Ref => "ref",
    }
);

text_enum!(
    /// `question.status` (docs/questions.md section 4.1).
    QuestionStatus ("question status") {
        Open => "open",
        Answered => "answered",
        Chat => "chat",
        Cancelled => "cancelled",
    }
);

text_enum!(
    /// `card.origin` (docs/board-v2.md section 3.3): where a card comes from.
    /// `discussing`: a topic the user has not confirmed yet; `agreed`: the user confirmed it;
    /// `derived`: the agent added it during execution (default for new cards).
    Origin ("origin") {
        Discussing => "discussing",
        Agreed => "agreed",
        Derived => "derived",
    }
);

text_enum!(
    /// `cause.kind` (docs/board-v2.md section 18.3): what a decision follows from.
    /// `user`: the user's instruction in the conversation, not an object on the board.
    CauseKind ("cause kind") {
        Card => "card",
        Question => "question",
        Comment => "comment",
        User => "user",
    }
);

text_enum!(
    /// The write that made a decision (docs/board-v2.md section 18.1).
    DecisionKind ("decision kind") {
        Note => "note",
        Drop => "drop",
        Done => "done",
    }
);

/// A `--because` reference as written (docs/board-v2.md section 18.2): `30` or `#30` (a
/// card), `q5` (a question), `c75` (a comment), `user` (the user's instruction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CauseRef {
    Card(i32),
    Question(i64),
    Comment(i64),
    User,
}

impl CauseRef {
    /// Parses one `--because` value. `q`, `c` and `user` ignore case.
    pub fn parse(s: &str) -> Result<CauseRef, Error> {
        let t = s.trim();
        let bad = || {
            Error::invalid(format!(
                "invalid --because: {s} (expected a card number like 30 or #30, \
                 q<question id>, c<comment id> or user)"
            ))
        };
        if t.eq_ignore_ascii_case("user") {
            return Ok(CauseRef::User);
        }
        let id = |rest: &str| match rest.parse::<i64>() {
            Ok(n) if n >= 1 && rest.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
            _ => Err(bad()),
        };
        match t.as_bytes().first() {
            Some(b'q' | b'Q') => Ok(CauseRef::Question(id(&t[1..])?)),
            Some(b'c' | b'C') => Ok(CauseRef::Comment(id(&t[1..])?)),
            _ => {
                let digits = t.strip_prefix('#').unwrap_or(t);
                match digits.parse::<i32>() {
                    Ok(n) if n >= 1 && digits.bytes().all(|b| b.is_ascii_digit()) => {
                        Ok(CauseRef::Card(n))
                    }
                    _ => Err(bad()),
                }
            }
        }
    }

    pub fn kind(self) -> CauseKind {
        match self {
            CauseRef::Card(_) => CauseKind::Card,
            CauseRef::Question(_) => CauseKind::Question,
            CauseRef::Comment(_) => CauseKind::Comment,
            CauseRef::User => CauseKind::User,
        }
    }
}

/// The canonical spelling used in messages: `30`, `q5`, `c75`, `user`.
impl std::fmt::Display for CauseRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CauseRef::Card(n) => write!(f, "{n}"),
            CauseRef::Question(id) => write!(f, "q{id}"),
            CauseRef::Comment(id) => write!(f, "c{id}"),
            CauseRef::User => f.write_str("user"),
        }
    }
}

/// One cause of a decision as read back (docs/board-v2.md section 18.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cause {
    pub kind: CauseKind,
    /// The question or comment id; `None` for a card and for `user`.
    pub id: Option<i64>,
    /// The cited card, or the card that holds the question or comment; `None` for `user`.
    pub card_seq: Option<i32>,
    pub card_title: Option<String>,
}

impl LaneKind {
    /// `done` and `dropped` lanes close a card (`card.closed_at` is set).
    pub fn is_closed(self) -> bool {
        matches!(self, LaneKind::Done | LaneKind::Dropped)
    }
}

/// Card priority, 0 (`P0`, highest) to 3 (`P3`). Default `P2`.
pub const DEFAULT_PRIORITY: i16 = 2;

/// Parses `P0`..`P3` (case-insensitive) or a bare `0`..`3`.
pub fn parse_priority(s: &str) -> Result<i16, Error> {
    let digits = s.strip_prefix(['P', 'p']).unwrap_or(s);
    match digits.parse::<i16>() {
        Ok(p @ 0..=3) => Ok(p),
        _ => Err(Error::invalid(format!(
            "invalid priority: {s} (expected P0, P1, P2 or P3)"
        ))),
    }
}

/// Formats a priority as `P0`..`P3`.
pub fn format_priority(p: i16) -> String {
    format!("P{p}")
}

/// Parses a card number: `5` or `#5`.
pub fn parse_card_seq(s: &str) -> Result<i32, Error> {
    let digits = s.trim().strip_prefix('#').unwrap_or(s.trim());
    match digits.parse::<i32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(Error::invalid(format!(
            "invalid card number: {s} (expected e.g. 5 or #5)"
        ))),
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Workspace {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub source: WorkspaceSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Board {
    pub id: i64,
    pub workspace_id: i64,
    pub session_key: String,
    pub title: String,
    pub goal: String,
    pub status: BoardStatus,
    pub outcome: Option<Outcome>,
    pub summary: Option<String>,
    pub herdr_space: Option<String>,
    pub herdr_tab: Option<String>,
    pub herdr_terminal: Option<String>,
    pub cwd: Option<String>,
    pub prev_board_id: Option<i64>,
    pub next_card_seq: i32,
    pub rev: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Key of the current turn (docs/hooks.md section 2.4); set by `kb hook prompt`.
    pub turn_key: Option<String>,
    /// When the latest user prompt (or `kb board start`) started a turn. `kb inbox` uses it;
    /// the rules use [`crate::rules::TurnStart`] instead (docs/hooks.md section 2.4).
    pub turn_started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Lane {
    pub id: i64,
    pub board_id: i64,
    pub name: String,
    pub position: i32,
    pub kind: LaneKind,
    pub wip_limit: Option<i32>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Card {
    pub id: i64,
    pub board_id: i64,
    pub seq: i32,
    pub lane_id: i64,
    pub title: String,
    pub body: String,
    pub priority: i16,
    pub position: i32,
    pub owner: String,
    pub blocked_reason: Option<String>,
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    /// The epic the card belongs to, if any (docs/board-v2.md section 3.2).
    pub epic_id: Option<i64>,
    pub origin: Origin,
}

/// A stage of the work on one board (docs/board-v2.md section 3.1).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Epic {
    pub id: i64,
    pub board_id: i64,
    /// Unique within the board.
    pub name: String,
    /// 1-based order.
    pub position: i32,
    // The column `epic.summary` (retrospective) stays in the schema but is no longer used
    // (docs/board-v2.md section 10.1).
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Note {
    pub id: i64,
    pub card_id: i64,
    pub kind: NoteKind,
    pub body: String,
    /// `agent`, or `user` for comments.
    pub author: String,
    pub created_at: DateTime<Utc>,
    /// When the agent received this comment (`kb inbox` or a hook). Only comments are
    /// delivered; `None` for other kinds and on a schema older than version 5.
    #[sqlx(default)]
    pub delivered_at: Option<DateTime<Utc>>,
    /// What this note's decision follows from (docs/board-v2.md section 18.4). Filled by
    /// `read::card_detail` only; empty elsewhere and on a schema older than version 7.
    #[sqlx(skip)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub because: Vec<Cause>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Link {
    pub id: i64,
    pub board_id: i64,
    pub from_card_id: i64,
    pub kind: LinkKind,
    pub to_card_id: Option<i64>,
    pub target: Option<String>,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A question an agent asked on a card (docs/questions.md section 4.1).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Question {
    pub id: i64,
    pub board_id: i64,
    pub card_id: i64,
    pub text: String,
    /// Several options may be selected.
    pub multi: bool,
    pub status: QuestionStatus,
    pub asked_by: String,
    pub asked_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    /// When the agent received the answer.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Text the human typed with the answer (docs/board-v2.md section 7.1); alone, it is
    /// the answer. `None` without text, and on a schema older than version 4.
    #[sqlx(default)]
    pub answer_text: Option<String>,
}

/// One candidate answer of a question (docs/questions.md section 4.2). The fixed
/// "reply in the session" choice is not an option row; it is `status = chat`.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct QuestionOption {
    pub id: i64,
    pub question_id: i64,
    /// 1-based display order.
    pub position: i32,
    pub label: String,
    pub description: Option<String>,
    /// The human selected this option.
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Activity {
    pub id: i64,
    pub board_id: i64,
    pub card_id: Option<i64>,
    pub at: DateTime<Utc>,
    pub actor: String,
    /// e.g. `card.move`; see docs/data-model.md section 2.7.
    pub verb: String,
    /// Verb-specific fields, enough to render one line without other queries.
    pub detail: serde_json::Value,
    pub turn_key: Option<String>,
}
