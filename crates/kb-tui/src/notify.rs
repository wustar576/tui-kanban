//! After an answer (docs/board-v2.md section 13.19).
//!
//! The TUI no longer tells the agent anything: the background waiter `kb hook wait`
//! wakes the session (section 13.3). One optional step is left, and it runs as a child
//! process with a short timeout, off the UI path:
//!
//! - `chat` (「在 session 裡說明」): the command in `KB_FOCUS_CMD`, with `{terminal}`
//!   replaced by the board's terminal id, so the keyboard goes to the agent's pane
//!   (docs/board-v2.md section 24). Only when the board has a terminal and the variable is
//!   set; when the command fails, nothing is shown.
//!
//! `KB_NOTIFY=off` skips it (tests use it).

use std::time::Duration;

/// How long the focus command may take.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// Shown when the agent's pane got the keyboard after 「在 session 裡說明」.
pub const FOCUSED: &str = "已切換到 agent 的 pane，請直接輸入說明";

/// True when `KB_NOTIFY` is `off` (also `0`, `false`, `no`).
pub fn disabled() -> bool {
    std::env::var("KB_NOTIFY").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

/// The variable that holds the focus command.
pub const FOCUS_CMD: &str = "KB_FOCUS_CMD";

/// Splits a `KB_FOCUS_CMD` value into program and arguments at whitespace (no quoting)
/// and replaces `{terminal}` in every part with `terminal`. `None` when the value is blank.
pub fn focus_argv(focus_cmd: &str, terminal: &str) -> Option<Vec<String>> {
    let argv: Vec<String> = focus_cmd
        .split_whitespace()
        .map(|part| part.replace("{terminal}", terminal))
        .collect();
    (!argv.is_empty()).then_some(argv)
}

/// The command to run after an answer, or `None` when there is nothing to run: only
/// 「在 session 裡說明」 on a board with a terminal, with `focus_cmd` set, focuses the
/// agent's pane.
pub fn command_with(
    focus_cmd: Option<&str>,
    terminal: Option<&str>,
    chat: bool,
) -> Option<Vec<String>> {
    if !chat {
        return None;
    }
    let terminal = terminal.map(str::trim).filter(|t| !t.is_empty())?;
    focus_argv(focus_cmd?, terminal)
}

/// [`command_with`] for the process environment: `KB_FOCUS_CMD`, and nothing when
/// `KB_NOTIFY` is off.
pub fn command(terminal: Option<&str>, chat: bool) -> Option<Vec<String>> {
    if disabled() {
        return None;
    }
    let focus_cmd = std::env::var(FOCUS_CMD).ok();
    command_with(focus_cmd.as_deref(), terminal, chat)
}

/// Runs the step after an answer and returns the line to show at the bottom, or `None`
/// when there is nothing to say (no command, or a failure: silent).
pub async fn after_answer(terminal: Option<&str>, chat: bool) -> Option<String> {
    let argv = command(terminal, chat)?;
    run(&argv).await.then(|| FOCUSED.to_string())
}

/// Runs `argv` with no input and its output discarded; true on exit code 0 within
/// [`TIMEOUT`]. A command that is not installed counts as a failure.
async fn run(argv: &[String]) -> bool {
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    match tokio::time::timeout(TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOCUS: &str = "focus-pane do pane.focus --terminal {terminal}";

    #[test]
    fn only_chat_with_a_terminal_and_a_focus_command_runs_a_command() {
        // A plain answer runs nothing, with or without a terminal.
        assert_eq!(command_with(Some(FOCUS), None, false), None);
        assert_eq!(command_with(Some(FOCUS), Some("t2b"), false), None);
        assert_eq!(command_with(Some(FOCUS), None, true), None);
        assert_eq!(command_with(Some(FOCUS), Some("  "), true), None);
        // Without KB_FOCUS_CMD (or with a blank one) nothing runs.
        assert_eq!(command_with(None, Some("t2b"), true), None);
        assert_eq!(command_with(Some("   "), Some("t2b"), true), None);
        assert_eq!(
            command_with(Some(FOCUS), Some(" t2b "), true).unwrap(),
            vec!["focus-pane", "do", "pane.focus", "--terminal", "t2b"]
        );
    }

    #[test]
    fn focus_command_splits_at_whitespace_and_fills_the_terminal() {
        assert_eq!(
            focus_argv("  wezterm  cli activate-pane --pane-id={terminal} ", "7").unwrap(),
            vec!["wezterm", "cli", "activate-pane", "--pane-id=7"]
        );
        assert_eq!(focus_argv("focus-me", "t1").unwrap(), vec!["focus-me"]);
        assert_eq!(focus_argv("", "t1"), None);
    }

    #[tokio::test]
    async fn nothing_to_run_means_nothing_to_show() {
        assert_eq!(after_answer(None, false).await, None);
        assert_eq!(after_answer(None, true).await, None);
        assert_eq!(after_answer(Some("t2b"), false).await, None);
    }

    #[tokio::test]
    async fn a_missing_program_is_silent() {
        // A program that does not exist fails to start: no line, no error.
        assert!(!run(&["kb-no-such-program-for-tests".to_string()]).await);
    }
}
