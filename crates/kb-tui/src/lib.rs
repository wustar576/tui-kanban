//! `kb tui`: the terminal UI of tui-kanban (docs/tui.md, docs/board-v2.md sections 5-7,
//! docs/questions.md section 7).
//!
//! - [`app`]: screen state and the pure `update` function (keys, loaded data, ticks).
//! - [`view`]: drawing from `&App`; never changes state.
//! - [`layout`]: geometry shared by both, so selections stay visible.
//! - [`markdown`]: Markdown of card bodies and notes, wrapped by display width.
//! - [`text`]: display-width helpers and the Chinese labels.
//! - [`notify`]: after 「在 session 裡說明」, running `KB_FOCUS_CMD` to focus the agent's pane.
//! - [`run`]: the only IO: database reads (as `kb_reader`), answering a question, adding a
//!   comment, the `LISTEN` connection, the terminal, and `--snapshot`.
//!
//! The TUI reads with `kb_core::read` and `kb_core::events`. Its two writes are answering
//! a question through `kb_core::answer::answer_question` and adding a comment through
//! `kb_core::comment::add_comment`, both on the same `kb_reader` connection
//! (docs/board-v2.md section 13.20). It never uses `kb_core::write`.

pub mod app;
pub mod layout;
pub mod markdown;
pub mod notify;
pub mod run;
pub mod text;
pub mod view;

pub use run::{Options, render_text, run, send_answer, send_comment, snapshot};

#[cfg(test)]
mod tests;
