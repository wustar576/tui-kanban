//! Answering a question: the only write the TUI makes (docs/questions.md section 3).
//!
//! This module executes the database function `kb_answer_question` and nothing else, so it
//! works on a `kb_reader` connection, which may execute that function but cannot write any
//! table directly. It is deliberately separate from [`crate::write`]: `kb-tui` must never
//! use `write`.

use sqlx::postgres::PgConnection;

use crate::error::{Error, Result};

/// Why `kb_answer_question` refused an answer. `Display` is the database message
/// (English); [`AnswerError::zh_tw`] is a short Traditional Chinese text for the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    #[error("question not found")]
    QuestionNotFound,
    /// The question was already answered or the agent cancelled it.
    #[error("question is not open")]
    NotOpen,
    #[error("board is archived")]
    BoardArchived,
    /// `chat` is false and no option id was given.
    #[error("no option selected")]
    NoOptionSelected,
    #[error("option does not belong to question")]
    OptionNotInQuestion,
    /// A single-choice question got more than one option.
    #[error("question accepts one option")]
    AcceptsOneOption,
    /// The typed answer is longer than 500 characters (docs/board-v2.md section 7.1).
    #[error("answer text is too long")]
    TextTooLong,
}

impl AnswerError {
    pub const ALL: [AnswerError; 7] = [
        AnswerError::QuestionNotFound,
        AnswerError::NotOpen,
        AnswerError::BoardArchived,
        AnswerError::NoOptionSelected,
        AnswerError::OptionNotInQuestion,
        AnswerError::AcceptsOneOption,
        AnswerError::TextTooLong,
    ];

    /// The exact message raised by `kb_answer_question`.
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerError::QuestionNotFound => "question not found",
            AnswerError::NotOpen => "question is not open",
            AnswerError::BoardArchived => "board is archived",
            AnswerError::NoOptionSelected => "no option selected",
            AnswerError::OptionNotInQuestion => "option does not belong to question",
            AnswerError::AcceptsOneOption => "question accepts one option",
            AnswerError::TextTooLong => "answer text is too long",
        }
    }

    /// Traditional Chinese text for the TUI status line.
    pub fn zh_tw(self) -> &'static str {
        match self {
            AnswerError::QuestionNotFound => "提問不存在",
            AnswerError::NotOpen => "提問已經不是待回答（可能已被回答或取消）",
            AnswerError::BoardArchived => "看板已封存",
            AnswerError::NoOptionSelected => "沒有勾選任何選項",
            AnswerError::OptionNotInQuestion => "選項不屬於這個提問",
            AnswerError::AcceptsOneOption => "這個提問只能選一個選項",
            AnswerError::TextTooLong => "文字超過 500 個字",
        }
    }

    /// Maps a database error message to the typed error.
    pub fn from_message(message: &str) -> Option<AnswerError> {
        AnswerError::ALL.into_iter().find(|e| e.as_str() == message)
    }
}

/// Answers question `question_id` by executing `kb_answer_question` (one transaction):
///
/// - `chat = true`: the human will reply in the session (`status = chat`); `option_ids`
///   is ignored.
/// - `chat = false`: selects `option_ids` (ids of `question_option` rows), stores
///   `text` (trimmed; at most 500 characters) and sets `status = answered`. With text,
///   `option_ids` may be empty: the text alone is the answer (docs/board-v2.md 7.1).
///
/// On success the function also sets `answered_at`, records `question.answer` activity and
/// bumps the board's `rev` (one `kb_events` notification). Refusals come back as
/// [`Error::Answer`]; a lost connection as [`Error::Unreachable`]. Works as `kb_reader`.
pub async fn answer_question(
    conn: &mut PgConnection,
    question_id: i64,
    option_ids: &[i64],
    chat: bool,
    text: Option<&str>,
) -> Result<()> {
    let result = sqlx::query("SELECT kb_answer_question($1, $2, $3, $4)")
        .bind(question_id)
        .bind(option_ids)
        .bind(chat)
        .bind(text)
        .execute(&mut *conn)
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(err) => {
            if let Some(db) = err.as_database_error()
                && let Some(e) = AnswerError::from_message(db.message())
            {
                return Err(Error::Answer(e));
            }
            Err(err.into())
        }
    }
}
