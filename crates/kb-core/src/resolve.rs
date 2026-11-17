//! Session key and workspace resolution (docs/cli.md sections 2.3 and 2.4).

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::model::WorkspaceSource;

/// Resolves the session key: `--session`, then `KB_SESSION`, then `CLAUDE_CODE_SESSION_ID`.
///
/// `env` looks up an environment variable; pass `|k| std::env::var(k).ok()` in production.
/// Empty values are ignored.
pub fn resolve_session_key(
    flag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let non_empty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    non_empty(flag.map(str::to_string))
        .or_else(|| non_empty(env("KB_SESSION")))
        .or_else(|| non_empty(env("CLAUDE_CODE_SESSION_ID")))
        .ok_or(Error::NoSession)
}

/// Where the session key comes from (docs/cli.md section 2.3, docs/hooks.md section 5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSource {
    /// Sources 1-3: `--session`, `KB_SESSION`, `CLAUDE_CODE_SESSION_ID`.
    Key(String),
    /// Source 4: the most recently changed active board of this herdr terminal
    /// (`HC_TERMINAL_ID`); needs a database lookup ([`resolve_session`]).
    Terminal(String),
}

/// Finds the session key source without touching the database: sources 1-3 as in
/// [`resolve_session_key`], then `HC_TERMINAL_ID`. Fails with [`Error::NoSession`] when
/// none is set.
pub fn session_source(
    flag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<SessionSource> {
    match resolve_session_key(flag, &env) {
        Ok(key) => Ok(SessionSource::Key(key)),
        Err(Error::NoSession) => env("HC_TERMINAL_ID")
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .map(SessionSource::Terminal)
            .ok_or(Error::NoSession),
        Err(e) => Err(e),
    }
}

/// Resolves a [`SessionSource`] to a session key. For a terminal, the key of the most
/// recently changed active board whose `herdr_terminal` is that terminal; no such board is
/// [`Error::NoSession`].
pub async fn resolve_session(
    conn: &mut sqlx::postgres::PgConnection,
    source: SessionSource,
) -> Result<String> {
    match source {
        SessionSource::Key(key) => Ok(key),
        SessionSource::Terminal(terminal) => crate::read::active_board_by_terminal(conn, &terminal)
            .await?
            .map(|b| b.session_key)
            .ok_or(Error::NoSession),
    }
}

/// The herdr location of a terminal, from `hc get <terminal id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HerdrContext {
    /// `labels["herdr.space"]`.
    pub space: String,
    /// `labels["herdr.tab"]`, if present.
    pub tab: Option<String>,
    /// The terminal address, e.g. `t2b`.
    pub terminal: String,
    /// The part of `labels["where"]` before ` / `; the space id when the label is absent.
    pub name: String,
}

impl HerdrContext {
    /// Workspace key for this herdr space: `herdr:<space id>`.
    pub fn workspace_key(&self) -> String {
        format!("herdr:{}", self.space)
    }
}

/// Parses the JSON printed by `hc get <id>`. `terminal_id` is the id that was queried;
/// it is used as the address when the JSON has no `address`. Returns `None` when the
/// JSON has no `labels["herdr.space"]`.
pub fn parse_hc_get(json: &str, terminal_id: &str) -> Option<HerdrContext> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let labels = value.get("labels")?;
    let label = |k: &str| {
        labels
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let space = label("herdr.space")?;
    let name = label("where")
        .map(|w| w.split(" / ").next().unwrap_or("").trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| space.clone());
    let terminal = value
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(terminal_id)
        .to_string();
    Some(HerdrContext {
        space,
        tab: label("herdr.tab"),
        terminal,
        name,
    })
}

/// Runs `hc get $HC_TERMINAL_ID` and parses the result.
/// Returns `None` when `HC_TERMINAL_ID` is unset, `hc` cannot run or fails, or the
/// terminal is not in a herdr space.
pub fn current_herdr_context() -> Option<HerdrContext> {
    current_hc().and_then(|hc| hc.herdr)
}

/// Runs `hc get <terminal_id>` and parses the result. `None` on any failure.
pub fn herdr_context_for(terminal_id: &str) -> Option<HerdrContext> {
    parse_hc_get(&hc_get(terminal_id)?, terminal_id)
}

/// Standard output of `hc get <terminal_id>`; `None` when `hc` cannot run or fails.
fn hc_get(terminal_id: &str) -> Option<String> {
    let output = Command::new("hc")
        .args(["get", terminal_id])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The terminal fields a board stores (docs/board-v2.md section 12.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalInfo {
    /// The terminal address: `address` from `hc get`, else the `HC_TERMINAL_ID` value.
    pub terminal: String,
    /// `labels["herdr.space"]`, if present.
    pub space: Option<String>,
    /// `labels["herdr.tab"]`, if present.
    pub tab: Option<String>,
}

impl From<&HerdrContext> for TerminalInfo {
    fn from(ctx: &HerdrContext) -> Self {
        TerminalInfo {
            terminal: ctx.terminal.clone(),
            space: Some(ctx.space.clone()),
            tab: ctx.tab.clone(),
        }
    }
}

/// Parses the terminal fields from `hc get` output. Never fails: without usable JSON (or
/// without output, `None`) the address is `terminal_id` and the labels are absent.
pub fn parse_hc_terminal(json: Option<&str>, terminal_id: &str) -> TerminalInfo {
    let value: Option<serde_json::Value> = json.and_then(|j| serde_json::from_str(j).ok());
    let text = |v: Option<&serde_json::Value>| {
        v.and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let label = |k: &str| text(value.as_ref().and_then(|v| v.get("labels")?.get(k)));
    TerminalInfo {
        terminal: text(value.as_ref().and_then(|v| v.get("address")))
            .unwrap_or_else(|| terminal_id.trim().to_string()),
        space: label("herdr.space"),
        tab: label("herdr.tab"),
    }
}

/// What one `hc get $HC_TERMINAL_ID` tells about the current terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HcInfo {
    /// Always known when `HC_TERMINAL_ID` is set.
    pub terminal: TerminalInfo,
    /// Only when the terminal is in a herdr space (workspace resolution).
    pub herdr: Option<HerdrContext>,
}

/// The current terminal (docs/board-v2.md section 12.7): `None` only when
/// `HC_TERMINAL_ID` is unset or empty. Runs `hc get` once; when it fails, the terminal
/// address is the `HC_TERMINAL_ID` value.
pub fn current_hc() -> Option<HcInfo> {
    let id = std::env::var("HC_TERMINAL_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let out = hc_get(&id);
    Some(HcInfo {
        terminal: parse_hc_terminal(out.as_deref(), &id),
        herdr: out.as_deref().and_then(|o| parse_hc_get(o, &id)),
    })
}

/// Resolves the workspace of a new board from the current environment: `--workspace`,
/// else the herdr space of `hc`, else `cwd`; the board also gets the current terminal
/// whatever the workspace source (docs/board-v2.md section 12.7).
pub fn workspace_here(manual: Option<&str>, hc: Option<HcInfo>, cwd: &Path) -> WorkspaceTarget {
    let (terminal, herdr) = match hc {
        Some(hc) => (Some(hc.terminal), hc.herdr),
        None => (None, None),
    };
    let mut ws = resolve_workspace(manual, herdr, cwd);
    if terminal.is_some() {
        ws.terminal = terminal;
    }
    ws
}

/// The workspace a new board belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceTarget {
    pub key: String,
    pub name: String,
    pub source: WorkspaceSource,
    /// Set when the workspace came from herdr.
    pub herdr: Option<HerdrContext>,
    /// Terminal fields stored on the new board (docs/board-v2.md section 12.7). Set from
    /// `herdr` by [`resolve_workspace`]; [`workspace_here`] sets it for any source.
    pub terminal: Option<TerminalInfo>,
}

/// Resolves the workspace of a new board (docs/cli.md section 2.4):
/// `--workspace <name>` → `manual:<name>`; else the herdr space from `herdr`
/// (normally [`current_herdr_context`]); else `dir:<cwd>`.
pub fn resolve_workspace(
    manual: Option<&str>,
    herdr: Option<HerdrContext>,
    cwd: &Path,
) -> WorkspaceTarget {
    if let Some(name) = manual.map(str::trim).filter(|s| !s.is_empty()) {
        return WorkspaceTarget {
            key: format!("manual:{name}"),
            name: name.to_string(),
            source: WorkspaceSource::Manual,
            herdr: None,
            terminal: None,
        };
    }
    if let Some(ctx) = herdr {
        return WorkspaceTarget {
            key: ctx.workspace_key(),
            name: ctx.name.clone(),
            source: WorkspaceSource::Herdr,
            terminal: Some(TerminalInfo::from(&ctx)),
            herdr: Some(ctx),
        };
    }
    let path = cwd.to_string_lossy().to_string();
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.clone());
    WorkspaceTarget {
        key: format!("dir:{path}"),
        name,
        source: WorkspaceSource::Dir,
        herdr: None,
        terminal: None,
    }
}
