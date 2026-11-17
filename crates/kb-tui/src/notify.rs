//! After an answer (docs/board-v2.md section 13.19).
//!
//! The TUI no longer tells the agent anything: the background waiter `kb hook wait`
//! wakes the session (section 13.3). One optional step is left, and it runs as a child
//! process with a short timeout, off the UI path:
//!
//! - `chat` (「在 session 裡說明」): `herdr-app do pane.focus --terminal <terminal>`, so the
//!   keyboard goes to the agent's pane. Only when the board has a terminal; when herdr is
//!   not there (or the command fails), nothing is shown.
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

/// The command to run after an answer, or `None` when there is nothing to run: only
/// 「在 session 裡說明」 on a board with a terminal focuses the agent's pane.
pub fn command(terminal: Option<&str>, chat: bool) -> Option<Vec<String>> {
    if !chat {
        return None;
    }
    let terminal = terminal.map(str::trim).filter(|t| !t.is_empty())?;
    if disabled() {
        return None;
    }
    Some(vec![
        "herdr-app".into(),
        "do".into(),
        "pane.focus".into(),
        "--terminal".into(),
        terminal.into(),
    ])
}

/// Runs the step after an answer and returns the line to show at the bottom, or `None`
/// when there is nothing to say (no command, herdr missing, or a failure: silent).
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

    #[test]
    fn only_chat_with_a_terminal_runs_a_command() {
        // No `hc send` any more: a plain answer runs nothing, with or without a terminal.
        assert_eq!(command(None, false), None);
        assert_eq!(command(Some("t2b"), false), None);
        assert_eq!(command(None, true), None);
        assert_eq!(command(Some("  "), true), None);
        // The test environment sets KB_NOTIFY=off only when asked; check the shape
        // without depending on it.
        if !disabled() {
            assert_eq!(
                command(Some("t2b"), true).unwrap(),
                vec!["herdr-app", "do", "pane.focus", "--terminal", "t2b"]
            );
        }
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
