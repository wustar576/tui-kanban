//! Session key and workspace resolution (docs/cli.md sections 2.3 and 2.4).

use std::path::Path;

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
    /// Source 4: the most recently changed active board of this terminal
    /// (`KB_TERMINAL_ID`); needs a database lookup ([`resolve_session`]).
    Terminal(String),
}

/// Finds the session key source without touching the database: sources 1-3 as in
/// [`resolve_session_key`], then `KB_TERMINAL_ID`. Fails with [`Error::NoSession`] when
/// none is set.
pub fn session_source(
    flag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<SessionSource> {
    match resolve_session_key(flag, &env) {
        Ok(key) => Ok(SessionSource::Key(key)),
        Err(Error::NoSession) => env_text(&env, TERMINAL_ID)
            .map(SessionSource::Terminal)
            .ok_or(Error::NoSession),
        Err(e) => Err(e),
    }
}

/// Resolves a [`SessionSource`] to a session key. For a terminal, the key of the most
/// recently changed active board whose `terminal_id` is that terminal; no such board is
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

/// Identity of the current terminal (docs/cli.md section 2.3).
pub const TERMINAL_ID: &str = "KB_TERMINAL_ID";
/// Id of the workspace area the terminal is in (docs/cli.md section 2.4).
pub const TERMINAL_SPACE: &str = "KB_TERMINAL_SPACE";
/// Tab id of the terminal.
pub const TERMINAL_TAB: &str = "KB_TERMINAL_TAB";
/// Workspace name when `KB_TERMINAL_SPACE` is set.
pub const WORKSPACE_NAME: &str = "KB_WORKSPACE_NAME";

/// The variable's value, trimmed; `None` when unset or blank.
fn env_text(env: impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The workspace area of the current terminal, from `KB_TERMINAL_SPACE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpaceContext {
    /// `KB_TERMINAL_SPACE`.
    pub space: String,
    /// `KB_WORKSPACE_NAME`; the space id when unset.
    pub name: String,
}

impl SpaceContext {
    /// Workspace key for this space: `space:<space id>`.
    pub fn workspace_key(&self) -> String {
        format!("space:{}", self.space)
    }
}

/// The terminal fields a board stores (docs/board-v2.md sections 12.7 and 24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalInfo {
    /// `KB_TERMINAL_ID`.
    pub terminal: String,
    /// `KB_TERMINAL_SPACE`, if set.
    pub space: Option<String>,
    /// `KB_TERMINAL_TAB`, if set.
    pub tab: Option<String>,
}

/// What the `KB_TERMINAL_*` variables tell about the current terminal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalEnv {
    /// Set when `KB_TERMINAL_ID` is set.
    pub terminal: Option<TerminalInfo>,
    /// Set when `KB_TERMINAL_SPACE` is set (workspace resolution).
    pub space: Option<SpaceContext>,
}

/// Reads the `KB_TERMINAL_*` variables and `KB_WORKSPACE_NAME` through `env`. Blank
/// values count as unset. Never fails.
pub fn terminal_env(env: impl Fn(&str) -> Option<String>) -> TerminalEnv {
    let space = env_text(&env, TERMINAL_SPACE);
    let tab = env_text(&env, TERMINAL_TAB);
    TerminalEnv {
        terminal: env_text(&env, TERMINAL_ID).map(|terminal| TerminalInfo {
            terminal,
            space: space.clone(),
            tab,
        }),
        space: space.map(|space| SpaceContext {
            name: env_text(&env, WORKSPACE_NAME).unwrap_or_else(|| space.clone()),
            space,
        }),
    }
}

/// [`terminal_env`] of the process environment.
pub fn current_terminal_env() -> TerminalEnv {
    terminal_env(|k| std::env::var(k).ok())
}

/// The current terminal's fields; `None` when `KB_TERMINAL_ID` is unset.
pub fn current_terminal() -> Option<TerminalInfo> {
    current_terminal_env().terminal
}

/// The current workspace area; `None` when `KB_TERMINAL_SPACE` is unset.
pub fn current_space() -> Option<SpaceContext> {
    current_terminal_env().space
}

/// Resolves the workspace of a new board from the current environment: `--workspace`,
/// else `KB_TERMINAL_SPACE`, else `cwd`; the board also gets the current terminal
/// whatever the workspace source (docs/board-v2.md section 12.7).
pub fn workspace_here(manual: Option<&str>, here: TerminalEnv, cwd: &Path) -> WorkspaceTarget {
    let mut ws = resolve_workspace(manual, here.space, cwd);
    ws.terminal = here.terminal;
    ws
}

/// The workspace a new board belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceTarget {
    pub key: String,
    pub name: String,
    pub source: WorkspaceSource,
    /// Set when the workspace came from `KB_TERMINAL_SPACE`.
    pub space: Option<SpaceContext>,
    /// Terminal fields stored on the new board (docs/board-v2.md section 12.7).
    /// [`workspace_here`] sets it for any source; [`resolve_workspace`] leaves it unset.
    pub terminal: Option<TerminalInfo>,
}

/// Resolves the workspace of a new board (docs/cli.md section 2.4):
/// `--workspace <name>` → `manual:<name>`; else the space from `space`
/// (normally [`current_space`]) → `space:<id>`; else `dir:<cwd>`.
pub fn resolve_workspace(
    manual: Option<&str>,
    space: Option<SpaceContext>,
    cwd: &Path,
) -> WorkspaceTarget {
    if let Some(name) = manual.map(str::trim).filter(|s| !s.is_empty()) {
        return WorkspaceTarget {
            key: format!("manual:{name}"),
            name: name.to_string(),
            source: WorkspaceSource::Manual,
            space: None,
            terminal: None,
        };
    }
    if let Some(ctx) = space {
        return WorkspaceTarget {
            key: ctx.workspace_key(),
            name: ctx.name.clone(),
            source: WorkspaceSource::Space,
            space: Some(ctx),
            terminal: None,
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
        space: None,
        terminal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn nothing_set_means_no_terminal_and_no_space() {
        assert_eq!(terminal_env(env_of(&[])), TerminalEnv::default());
        let blank = env_of(&[
            (TERMINAL_ID, "  "),
            (TERMINAL_SPACE, ""),
            (TERMINAL_TAB, " "),
        ]);
        assert_eq!(terminal_env(blank), TerminalEnv::default());
    }

    #[test]
    fn all_variables_set() {
        let here = terminal_env(env_of(&[
            (TERMINAL_ID, " t2b "),
            (TERMINAL_SPACE, "spc1"),
            (TERMINAL_TAB, "tb1"),
            (WORKSPACE_NAME, "Acme_DevOps"),
        ]));
        assert_eq!(
            here.terminal,
            Some(TerminalInfo {
                terminal: "t2b".into(),
                space: Some("spc1".into()),
                tab: Some("tb1".into()),
            })
        );
        assert_eq!(
            here.space,
            Some(SpaceContext {
                space: "spc1".into(),
                name: "Acme_DevOps".into(),
            })
        );
        assert_eq!(here.space.unwrap().workspace_key(), "space:spc1");
    }

    #[test]
    fn space_without_name_uses_the_id_and_needs_no_terminal() {
        let here = terminal_env(env_of(&[(TERMINAL_SPACE, "spc2")]));
        assert_eq!(here.terminal, None);
        assert_eq!(here.space.unwrap().name, "spc2");
    }

    #[test]
    fn terminal_without_space() {
        let here = terminal_env(env_of(&[(TERMINAL_ID, "t7")]));
        assert_eq!(here.space, None);
        let t = here.terminal.clone().unwrap();
        assert_eq!(
            (t.terminal.as_str(), &t.space, &t.tab),
            ("t7", &None, &None)
        );
        // No space: the workspace comes from the folder; the terminal is still stored.
        let cwd = Path::new("proj");
        let ws = workspace_here(None, here.clone(), cwd);
        assert_eq!(ws.source, WorkspaceSource::Dir);
        assert_eq!(ws.terminal, Some(t));
        let manual = workspace_here(Some("手動"), here, cwd);
        assert_eq!(manual.key, "manual:手動");
        assert!(manual.terminal.is_some());
        assert_eq!(
            workspace_here(None, TerminalEnv::default(), cwd).terminal,
            None
        );
    }

    #[test]
    fn workspace_order_manual_then_space_then_folder() {
        let ctx = SpaceContext {
            space: "spc1".into(),
            name: "Acme".into(),
        };
        let cwd = Path::new("proj");
        let manual = resolve_workspace(Some("手動"), Some(ctx.clone()), cwd);
        assert_eq!(
            (manual.key.as_str(), manual.source),
            ("manual:手動", WorkspaceSource::Manual)
        );
        let space = resolve_workspace(None, Some(ctx), cwd);
        assert_eq!(
            (space.key.as_str(), space.name.as_str(), space.source),
            ("space:spc1", "Acme", WorkspaceSource::Space)
        );
        assert!(space.space.is_some());
        let dir = resolve_workspace(None, None, cwd);
        assert_eq!(
            (dir.name.as_str(), dir.source),
            ("proj", WorkspaceSource::Dir)
        );
    }

    #[test]
    fn session_source_falls_back_to_the_terminal() {
        let env = env_of(&[(TERMINAL_ID, "t9")]);
        assert_eq!(
            session_source(None, &env).unwrap(),
            SessionSource::Terminal("t9".into())
        );
        assert_eq!(
            session_source(Some("flag"), &env).unwrap(),
            SessionSource::Key("flag".into())
        );
        // The old herdr variable is not read any more.
        let old = env_of(&[("HC_TERMINAL_ID", "t9")]);
        assert!(matches!(session_source(None, old), Err(Error::NoSession)));
    }
}
