//! `kb hook <event>`: Claude Code hook entry points (docs/hooks.md, docs/board-v2.md
//! section 12).
//!
//! Reads one JSON object from standard input, uses its `session_id` as the session key,
//! and never fails the session: every unexpected problem is exit code 0 with a
//! `kb hook:` message on standard error (section 2.3). Only `pre-tool` exits with 2, when a
//! rule at level `block` fails, and `wait` and `judge`, to wake the session (docs/board-v2.md
//! section 13.3, docs/hooks.md section 3.7). `judge` prints nothing on failure. All JSON
//! output is ASCII-only (section 2.2).
//!
//! A session without a board hears nothing about the board (docs/board-v2.md section
//! 12.2): no hook prints anything for it, except after an automatic adoption (12.8).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kb_core::hook::{self, Delivery, HookInput, PreTool, Stop};
use kb_core::resolve::{self, TerminalInfo};
use kb_core::rules::{self, Level, RuleId};
use kb_core::wait::{self, WaitEnd, WaitOptions};
use kb_core::{Config, Error, PgConnection, Role, db, transcript};
use serde_json::json;

use crate::cli::HookCmd;
use crate::{guide, output};

pub const NO_LANES: &str = "The board has no lanes. Run \"kb board init\" before starting work.";
pub const ACT_ON_ANSWERS: &str = "Act on these answers before stopping.";
/// First line of the previous turn's warnings in the `prompt` context (docs/board-v2.md
/// section 14.2).
pub const REMINDERS_HEADER: &str = "Board reminders from the last turn:";
/// The `stop` block line when comments were delivered (docs/board-v2.md section 13.8).
pub const REPLY_TO_COMMENTS: &str = "Reply to each comment with kb card note <n> --kind reply \"<text>\", then act on it before stopping.";

/// Connection time limit of `session-start` (docs/board-v2.md section 12.9).
pub const SESSION_START_TIMEOUT: Duration = Duration::from_millis(3000);
/// Connection time limit of every other event.
pub const HOOK_TIMEOUT: Duration = Duration::from_millis(500);

/// What the process prints and its exit code.
#[derive(Debug, Default)]
struct Reply {
    stdout: Option<String>,
    stderr: Vec<String>,
    code: u8,
}

impl Reply {
    fn context(event: &str, text: String) -> Reply {
        Reply {
            stdout: Some(output::json_ascii(&json!({
                "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
            }))),
            ..Reply::default()
        }
    }

    fn block(reason: String) -> Reply {
        Reply {
            stdout: Some(output::json_ascii(
                &json!({ "decision": "block", "reason": reason }),
            )),
            ..Reply::default()
        }
    }
}

/// Runs a hook and returns the process exit code.
pub async fn run(event: HookCmd) -> u8 {
    let mut bytes = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    if event == HookCmd::Judge {
        // The judge never prints anything except the text that wakes the agent
        // (docs/hooks.md section 3.7): every failure is silent.
        let reply = match HookInput::parse(&text) {
            Some(input) => judge(&input).await,
            None => Reply::default(),
        };
        if reply.code == 2 {
            let mut err = std::io::stderr().lock();
            for line in &reply.stderr {
                let _ = writeln!(err, "{line}");
            }
            let _ = err.flush();
        }
        return reply.code;
    }
    let reply = match HookInput::parse(&text) {
        None => Reply {
            stderr: vec!["kb hook: invalid input".to_string()],
            ..Reply::default()
        },
        Some(input) => match handle(event, &input).await {
            Ok(reply) => reply,
            Err(e) => failure(e),
        },
    };
    let mut out = std::io::stdout().lock();
    if let Some(s) = &reply.stdout {
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
    let mut err = std::io::stderr().lock();
    for line in &reply.stderr {
        let _ = writeln!(err, "{line}");
    }
    reply.code
}

/// Fail open (docs/hooks.md section 2.3). No event prints context on failure: the agent
/// must not hear about a board it may not have (docs/board-v2.md section 12.2).
fn failure(e: Error) -> Reply {
    let message = match &e {
        Error::Unreachable(_) => "kb hook: database unreachable".to_string(),
        other => format!("kb hook: {other}"),
    };
    Reply {
        stderr: vec![message],
        ..Reply::default()
    }
}

/// The connection settings without their warnings: a hook's standard error is not for the
/// human, and `wait`'s is the text that wakes the agent (docs/board-v2.md section 15.4).
fn load_config() -> Result<Config, Error> {
    Config::load()
}

/// Connects with the event's time limit (docs/board-v2.md section 12.9).
async fn connect(event: HookCmd) -> Result<PgConnection, Error> {
    let config = load_config()?;
    let limit = if event == HookCmd::SessionStart {
        SESSION_START_TIMEOUT
    } else {
        HOOK_TIMEOUT
    };
    db::connect_timeout(config.url(Role::Writer)?, limit).await
}

fn levels(reply: &mut Reply) -> rules::Levels {
    let (levels, warnings) = rules::Levels::load();
    reply.stderr.extend(
        warnings
            .into_iter()
            .map(|w| format!("kb hook: warning: {w}")),
    );
    levels
}

async fn handle(event: HookCmd, input: &HookInput) -> Result<Reply, Error> {
    let session = input.session_id.as_str();
    if session.is_empty() {
        // No session key: there is no board to work with. Not an error for the session.
        return Ok(Reply {
            stderr: if event == HookCmd::SessionStart {
                vec!["kb hook: no session_id in input".to_string()]
            } else {
                Vec::new()
            },
            ..Reply::default()
        });
    }
    match event {
        HookCmd::SessionStart => session_start(input).await,
        HookCmd::Prompt => prompt(input).await,
        HookCmd::PreTool => pre_tool(input).await,
        HookCmd::ToolUsed => {
            let mut conn = connect(event).await?;
            let tool = input.tool_name.as_deref().unwrap_or("");
            hook::tool_used(
                &mut conn,
                session,
                tool,
                &input.tool_input,
                input.duration_ms,
                input.cwd.as_deref(),
            )
            .await?;
            Ok(Reply::default())
        }
        HookCmd::Stop => stop(input).await,
        HookCmd::Wait => wait(input).await,
        HookCmd::Judge => Ok(judge(input).await),
        HookCmd::SessionEnd => {
            let mut conn = connect(event).await?;
            let reason = input.reason.as_deref().unwrap_or("other");
            hook::session_end(&mut conn, session, reason).await?;
            Ok(Reply::default())
        }
    }
}

/// Persists `KB_SESSION` for the session's later Bash commands through Claude Code's
/// `CLAUDE_ENV_FILE` (docs/cli.md section 2.3), when that variable is set.
fn persist_session_env(session: &str) -> Option<String> {
    let path = std::env::var_os("CLAUDE_ENV_FILE").filter(|v| !v.is_empty())?;
    // Unquoted when possible: Claude Code copies only plain `export NAME=value` lines (value
    // used as written) into PowerShell commands. Session ids are UUIDs, so this is the norm.
    let plain = session
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let line = if plain {
        format!("export KB_SESSION={session}\n")
    } else {
        format!("export KB_SESSION='{}'\n", session.replace('\'', "'\\''"))
    };
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(&path))
        .and_then(|mut f| f.write_all(line.as_bytes()));
    result
        .err()
        .map(|e| format!("kb hook: warning: cannot write CLAUDE_ENV_FILE: {e}"))
}

/// The context receives the board without a `next:` line: the next write prints it
/// (docs/board-v2.md section 15.2). Never fails the hook.
async fn forget_next(conn: &mut PgConnection, board_id: i64) {
    let _ = kb_core::write::forget_next(conn, board_id).await;
}

/// The current terminal's fields (docs/board-v2.md section 12.7); runs `hc get`.
fn current_terminal() -> Option<TerminalInfo> {
    resolve::current_hc().map(|hc| hc.terminal)
}

/// Automatic adoption (docs/board-v2.md section 12.8). True when a predecessor's board
/// now belongs to this session. Reading the transcripts never fails the hook.
async fn try_auto_adopt(conn: &mut PgConnection, input: &HookInput) -> Result<bool, Error> {
    let Some(path) = input.transcript_path.as_deref() else {
        return Ok(false);
    };
    let session = input.session_id.as_str();
    let adopted = hook::auto_adopt(
        conn,
        session,
        || transcript::predecessors(Path::new(path), session),
        current_terminal,
    )
    .await?;
    Ok(adopted.is_some())
}

async fn session_start(input: &HookInput) -> Result<Reply, Error> {
    let session = input.session_id.as_str();
    let source = input.source.as_deref().unwrap_or("");
    let mut conn = connect(HookCmd::SessionStart).await?;
    let mut start = hook::session_start(&mut conn, session, source, current_terminal).await?;
    if start.is_none() && try_auto_adopt(&mut conn, input).await? {
        // The adoption already refreshed the terminal fields.
        start = hook::session_start(&mut conn, session, source, || None).await?;
    }
    let Some(start) = start else {
        // No board: the session knows nothing about kb (docs/board-v2.md section 12.2).
        return Ok(Reply::default());
    };
    let snapshot = kb_core::read::board_snapshot(&mut conn, start.board.id)
        .await?
        .ok_or_else(|| Error::NoBoard(session.to_string()))?;
    forget_next(&mut conn, start.board.id).await;
    let mut parts = vec![guide::board_context(&snapshot)];
    parts.extend(delivery_text(&start.delivery));
    let mut reply = Reply::context("SessionStart", parts.join("\n\n"));
    reply.stderr.extend(persist_session_env(session));
    Ok(reply)
}

async fn start_turn(
    conn: &mut PgConnection,
    input: &HookInput,
) -> Result<Option<hook::Prompt>, Error> {
    hook::prompt(
        conn,
        &input.session_id,
        input.prompt_id.as_deref(),
        input.prompt.as_deref().unwrap_or(""),
    )
    .await
}

/// What a delivering hook puts in the context: the delivered answers and comments in the
/// `kb inbox` format (with the reminder line), or only the reminder about comments without
/// a reply; `None` when there is neither (docs/board-v2.md sections 13.8 and 13.9).
fn delivery_text(d: &Delivery) -> Option<String> {
    if !d.is_empty() {
        Some(output::inbox_text(
            &d.answers,
            &d.comments,
            d.open_questions,
            &d.unreplied_comments,
        ))
    } else {
        output::unreplied_line(&d.unreplied_comments)
    }
}

async fn prompt(input: &HookInput) -> Result<Reply, Error> {
    if hook::is_task_notification(input.prompt.as_deref().unwrap_or("")) {
        return notification(input).await;
    }
    let mut conn = connect(HookCmd::Prompt).await?;
    let mut reply = Reply::default();
    let levels = levels(&mut reply);
    let mut p = start_turn(&mut conn, input).await?;
    let mut adopted = false;
    if p.is_none() && try_auto_adopt(&mut conn, input).await? {
        adopted = true;
        p = start_turn(&mut conn, input).await?;
    }
    let Some(p) = p else {
        return Ok(reply);
    };
    let mut parts = Vec::new();
    if adopted {
        // The conversation moved to this session: tell the agent about its board again.
        let board = kb_core::read::board_by_session(&mut conn, &input.session_id)
            .await?
            .ok_or_else(|| Error::NoBoard(input.session_id.clone()))?;
        let snapshot = kb_core::read::board_snapshot(&mut conn, board.id)
            .await?
            .ok_or_else(|| Error::NoBoard(input.session_id.clone()))?;
        forget_next(&mut conn, board.id).await;
        parts.push(guide::board_context(&snapshot));
        reply.stderr.extend(persist_session_env(&input.session_id));
    }
    parts.extend(delivery_text(&p.delivery));
    if !adopted && p.lanes == 0 && levels.get(RuleId::R1) != Level::Off {
        parts.push(NO_LANES.to_string());
    }
    parts.extend(reminder_text(&p.reminders));
    if !parts.is_empty() {
        let stderr = std::mem::take(&mut reply.stderr);
        reply = Reply::context("UserPromptSubmit", parts.join("\n\n"));
        reply.stderr = stderr;
    }
    Ok(reply)
}

/// The previous turn's warnings for the `prompt` context (docs/board-v2.md sections 14.2
/// and 16.1): the header, then one line per `check.warn` row; `None` when there are none.
fn reminder_text(reminders: &[hook::Reminder]) -> Option<String> {
    if reminders.is_empty() {
        return None;
    }
    let mut s = REMINDERS_HEADER.to_string();
    for r in reminders {
        s.push('\n');
        s.push_str(&r.line());
    }
    Some(s)
}

/// `kb hook prompt` for a task notification, e.g. the turn a waiter woke
/// (docs/board-v2.md section 13.6): no new turn, no `user.prompt`, no hints, no adoption,
/// no warnings of the previous turn (they wait for the next real prompt, section 14.2);
/// only the delivery and the reminder about comments without a reply.
async fn notification(input: &HookInput) -> Result<Reply, Error> {
    let mut conn = connect(HookCmd::Prompt).await?;
    let Some(delivery) = hook::notification(&mut conn, &input.session_id).await? else {
        return Ok(Reply::default());
    };
    Ok(match delivery_text(&delivery) {
        Some(text) => Reply::context("UserPromptSubmit", text),
        None => Reply::default(),
    })
}

/// `kb hook wait` (docs/board-v2.md section 13.3): exit 2 with one line on standard error
/// to wake the session; every other end is exit 0 without output.
async fn wait(input: &HookInput) -> Result<Reply, Error> {
    let config = load_config()?;
    let claude_pid = wait::parse_pid(std::env::var("CLAUDE_PID").ok().as_deref());
    let opts = WaitOptions::new(input.session_id.clone(), claude_pid);
    Ok(match wait::wait(config.url(Role::Writer)?, &opts).await? {
        WaitEnd::Wake(items) => Reply {
            stderr: vec![items.message()],
            code: 2,
            ..Reply::default()
        },
        _ => Reply::default(),
    })
}

/// Connection time limit of `kb hook judge`: it runs in the background.
pub const JUDGE_CONNECT_TIMEOUT: Duration = Duration::from_millis(3000);

/// The judge key from the plugin option `judge_api_key` (`userConfig` in
/// plugin/.claude-plugin/plugin.json): Claude Code passes it to the plugin's hooks under this name.
const PLUGIN_JUDGE_KEY_ENV: &str = "CLAUDE_PLUGIN_OPTION_JUDGE_API_KEY";

/// `kb hook judge` (docs/hooks.md section 3.7): exit 2 with the wake text on standard error
/// when a finding in the wake band wakes the agent; every other end, including every
/// failure, is exit 0 without output.
async fn judge(input: &HookInput) -> Reply {
    use kb_core::judge::{self, GeminiDecisions, JudgeConfig, JudgeEnd};
    let session = input.session_id.as_str();
    let Some(message) = input
        .last_assistant_message
        .as_deref()
        .filter(|m| !m.trim().is_empty())
    else {
        return Reply::default();
    };
    if session.is_empty() {
        return Reply::default();
    }
    let (config, _) = JudgeConfig::load();
    if !config.enabled {
        return Reply::default();
    }
    // The plugin option first, then the variable of `[judge] api_key_env`; empty counts as unset.
    let Some(api_key) = [PLUGIN_JUDGE_KEY_ENV, config.api_key_env.as_str()]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
        })
    else {
        return Reply::default();
    };
    let provider = GeminiDecisions {
        endpoint: config.endpoint.clone(),
        model: config.model.clone(),
        api_key,
        timeout: Duration::from_millis(config.timeout_ms),
    };
    let run = async {
        let config_db = load_config()?;
        let mut conn =
            db::connect_timeout(config_db.url(Role::Writer)?, JUDGE_CONNECT_TIMEOUT).await?;
        judge::judge_turn(&mut conn, session, message, &config, &provider).await
    };
    match run.await {
        Ok(JudgeEnd::Judged {
            wake: Some(text), ..
        }) => Reply {
            stderr: vec![text],
            code: 2,
            ..Reply::default()
        },
        _ => Reply::default(),
    }
}

async fn pre_tool(input: &HookInput) -> Result<Reply, Error> {
    let tool = input.tool_name.as_deref().unwrap_or("");
    // Only R1 (file edits) and R6 (AskUserQuestion) are checked; skip the database otherwise.
    if !rules::is_file_edit_tool(tool) && tool != rules::ASK_TOOL {
        return Ok(Reply::default());
    }
    let mut reply = Reply::default();
    let levels = levels(&mut reply);
    let mut conn = connect(HookCmd::PreTool).await?;
    match hook::pre_tool(&mut conn, &input.session_id, tool, &levels).await? {
        PreTool::NoBoard | PreTool::Pass => {}
        PreTool::Warn(result) => {
            for v in &result.violations {
                reply.stderr.push(format!("kb hook: warning: {}", v.text));
            }
        }
        PreTool::Block(_, text) => {
            reply.stderr.push(text);
            reply.code = 2;
        }
    }
    Ok(reply)
}

async fn stop(input: &HookInput) -> Result<Reply, Error> {
    let mut reply = Reply::default();
    let levels = levels(&mut reply);
    let mut conn = connect(HookCmd::Stop).await?;
    let stderr = std::mem::take(&mut reply.stderr);
    // Condition 1: Claude Code is already continuing because of a Stop hook. A turn blocks
    // at most once: the rules are checked and recorded, never blocking (docs/board-v2.md
    // section 16.1).
    let outcome = if input.stop_hook_active {
        hook::stop_after_block(&mut conn, &input.session_id, &levels).await?
    } else {
        hook::stop(&mut conn, &input.session_id, &levels).await?
    };
    let mut reply = match outcome {
        Stop::NoBoard | Stop::Pass { .. } | Stop::AfterBlock { .. } => Reply::default(),
        Stop::Deliver(d) => {
            let mut reason = output::inbox_text(
                &d.answers,
                &d.comments,
                d.open_questions,
                &d.unreplied_comments,
            );
            if !d.answers.is_empty() {
                reason.push('\n');
                reason.push_str(ACT_ON_ANSWERS);
            }
            if !d.comments.is_empty() {
                reason.push('\n');
                reason.push_str(REPLY_TO_COMMENTS);
            }
            Reply::block(reason)
        }
        Stop::Block(results) => Reply::block(rules::block_text(&results, rules::STOP_HEADER)),
    };
    reply.stderr = stderr;
    Ok(reply)
}
