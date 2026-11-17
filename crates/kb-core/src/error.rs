//! Error type shared by every kb-core operation, with the CLI exit-code mapping
//! from docs/cli.md section 2.8.

/// Errors returned by kb-core. `Display` gives the exact English message the CLI prints.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no session: set KB_SESSION or pass --session")]
    NoSession,

    #[error(
        "no board for session {0}: run \"kb board init\" first\n\
         hint: start a board with \"kb board start\", or take over an earlier session's board \
         with \"kb board adopt <board id>\""
    )]
    NoBoard(String),

    /// `kb board adopt` when the current session already has a board.
    #[error("session {session} already has board {board}")]
    AlreadyHasBoard { session: String, board: i64 },

    /// `kb board adopt` with a target that matches no board.
    #[error("board not found: {0}")]
    BoardNotFound(String),

    #[error("board is archived: run \"kb board reopen\" first")]
    BoardArchived,

    #[error("lane not found: {name} (lanes: {})", lane_list(.available))]
    LaneNotFound {
        name: String,
        available: Vec<String>,
    },

    #[error("epic not found: {name} (epics: {})", lane_list(.available))]
    EpicNotFound {
        name: String,
        available: Vec<String>,
    },

    #[error("card not found: {0}")]
    CardNotFound(i32),

    #[error("link not found: {0}")]
    LinkNotFound(i64),

    #[error("workspace not found: {0}")]
    WorkspaceNotFound(String),

    #[error("question not found: {0}")]
    QuestionNotFound(i64),

    /// `kb_answer_question` refused the answer (docs/questions.md section 4.3).
    #[error("{0}")]
    Answer(crate::answer::AnswerError),

    /// `kb_add_comment` refused the comment (docs/board-v2.md section 13.7).
    #[error("{0}")]
    Comment(crate::comment::CommentError),

    /// Any other data error: an invalid argument value or a rule of the CLI contract.
    #[error("{0}")]
    Invalid(String),

    #[error("no database config: {0}")]
    NoConfig(String),

    /// A command needs the admin connection and `KB_ADMIN_URL` is unset
    /// (docs/board-v2.md section 15.4). Exit 1: a usage error, not an unreachable database.
    #[error(
        "no admin connection: set KB_ADMIN_URL for kb db commands \
         (admin_url in config.toml is not read)"
    )]
    NoAdminUrl,

    /// `kb db migrate` without `--yes` on a database that has boards and pending
    /// migrations (docs/board-v2.md section 15.5). Nothing was changed.
    #[error(
        "migration plan for database {database}: schema version {}, pending {}\n\
         the database already has boards; nothing was changed. \
         To apply: kb db migrate --database {database} --yes",
        .current.map_or_else(|| "none".to_string(), |v| v.to_string()),
        version_list(.pending)
    )]
    MigrationNeedsConfirm {
        database: String,
        current: Option<i32>,
        pending: Vec<i32>,
    },

    #[error("database unreachable: {0}")]
    Unreachable(String),

    #[error("database error: {0}")]
    Db(sqlx::Error),
}

fn lane_list(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

fn version_list(versions: &[i32]) -> String {
    versions
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid(message.into())
    }

    /// Process exit code per docs/cli.md section 2.8: 1 data error, 3 database unreachable
    /// or not configured. (2, syntax error, is produced by the argument parser.)
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::NoConfig(_) | Error::Unreachable(_) => 3,
            _ => 1,
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed => Error::Unreachable(err.to_string()),
            other => Error::Db(other),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
