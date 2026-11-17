//! Adding a comment to a card: the TUI's second write (docs/board-v2.md section 13.7).
//!
//! Like [`crate::answer`], this module executes one `SECURITY DEFINER` database function,
//! `kb_add_comment`, and nothing else, so it works on a `kb_reader` connection. It is
//! separate from [`crate::write`]: `kb-tui` must never use `write`.

use sqlx::postgres::PgConnection;

use crate::error::{Error, Result};

/// Longest comment, in characters, after line breaks became spaces and the text was trimmed.
pub const MAX_COMMENT_CHARS: usize = 500;

/// Why `kb_add_comment` refused a comment. `Display` is the database message (English);
/// [`CommentError::zh_tw`] is a short Traditional Chinese text for the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommentError {
    #[error("card not found")]
    CardNotFound,
    /// The board is archived; the human may ask the agent to reopen it.
    #[error("board is archived")]
    BoardArchived,
    /// Nothing left after trimming white space.
    #[error("comment text is empty")]
    Empty,
    /// More than [`MAX_COMMENT_CHARS`] characters.
    #[error("comment text is too long")]
    TooLong,
}

impl CommentError {
    pub const ALL: [CommentError; 4] = [
        CommentError::CardNotFound,
        CommentError::BoardArchived,
        CommentError::Empty,
        CommentError::TooLong,
    ];

    /// The exact message raised by `kb_add_comment`.
    pub fn as_str(self) -> &'static str {
        match self {
            CommentError::CardNotFound => "card not found",
            CommentError::BoardArchived => "board is archived",
            CommentError::Empty => "comment text is empty",
            CommentError::TooLong => "comment text is too long",
        }
    }

    /// Traditional Chinese text for the TUI status line.
    pub fn zh_tw(self) -> &'static str {
        match self {
            CommentError::CardNotFound => "卡片不存在",
            CommentError::BoardArchived => "看板已封存：請 agent 重新開啟看板",
            CommentError::Empty => "留言沒有內容",
            CommentError::TooLong => "留言超過 500 個字",
        }
    }

    /// Maps a database error message to the typed error.
    pub fn from_message(message: &str) -> Option<CommentError> {
        CommentError::ALL
            .into_iter()
            .find(|e| e.as_str() == message)
    }
}

/// The text `kb_add_comment` stores: line breaks become spaces, then leading and trailing
/// white space (including the ideographic space) is removed. The TUI can use it to count
/// characters the way the database does.
pub fn normalize_text(text: &str) -> String {
    text.replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .trim_matches([' ', '\t', '\u{3000}'])
        .to_string()
}

/// Adds a comment from the human to card `card_id` (the card's `id`, not its number) by
/// executing `kb_add_comment` (one transaction). Returns the new note's id.
///
/// On success the function also records `comment.add` activity (actor `user`) and bumps
/// the board's `rev` (one `kb_events` notification), which wakes the session's waiter.
/// Refusals come back as [`Error::Comment`]; a lost connection as [`Error::Unreachable`].
/// Works as `kb_reader`.
pub async fn add_comment(conn: &mut PgConnection, card_id: i64, text: &str) -> Result<i64> {
    let result = sqlx::query_scalar::<_, i64>("SELECT kb_add_comment($1, $2)")
        .bind(card_id)
        .bind(text)
        .fetch_one(&mut *conn)
        .await;
    match result {
        Ok(id) => Ok(id),
        Err(err) => {
            if let Some(db) = err.as_database_error()
                && let Some(e) = CommentError::from_message(db.message())
            {
                return Err(Error::Comment(e));
            }
            Err(err.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_matches_the_database_rules() {
        assert_eq!(normalize_text("  a\r\nb\nc\rd\u{3000} "), "a b c d");
        assert_eq!(normalize_text("\u{3000}\t"), "");
    }

    #[test]
    fn messages_round_trip() {
        for e in CommentError::ALL {
            assert_eq!(CommentError::from_message(e.as_str()), Some(e));
            assert_eq!(e.to_string(), e.as_str());
        }
    }
}
