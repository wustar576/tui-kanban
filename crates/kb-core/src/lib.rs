//! tui-kanban core library: configuration, database access, data model, write operations,
//! read queries, workspace resolution and change notifications.
//!
//! - Writers (the `kb` CLI) use [`write`]; each operation is one transaction that also
//!   records activity and bumps the board revision.
//! - Readers (the TUI) use [`read`] and [`events`]; both work as the `kb_reader` role.
//!   The TUI's two writes are [`answer::answer_question`] and [`comment::add_comment`]; they
//!   only execute the `SECURITY DEFINER` functions `kb_answer_question` and `kb_add_comment`.
//! - Claude Code hooks use [`hook`]; rule levels and checks are in [`rules`]; the background
//!   waiter of `kb hook wait` is [`wait`], with process checks in [`process`]; the model judge of
//!   `kb hook judge` is [`judge`].

pub mod answer;
pub mod comment;
pub mod config;
pub mod db;
pub mod error;
pub mod events;
pub mod hook;
pub mod judge;
pub mod model;
pub mod process;
pub mod project;
pub mod read;
pub mod resolve;
pub mod rules;
pub mod transcript;
pub mod wait;
pub mod write;

pub use config::{Config, Role};
pub use error::{Error, Result};
pub use sqlx::postgres::PgConnection;
