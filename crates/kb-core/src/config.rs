//! Connection settings (docs/architecture.md section 3.2, docs/board-v2.md section 15.4).
//!
//! The writer and reader URLs are looked up independently: environment variable first
//! (`KB_WRITER_URL`, `KB_READER_URL`), then `~/.config/kb/config.toml`. The admin URL comes
//! only from the environment variable `KB_ADMIN_URL`: agents read the config file, so the
//! admin connection must not live there. An `admin_url` key in the file is ignored with a
//! warning.

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};

/// The warning printed when the config file still has an `admin_url` key
/// (docs/board-v2.md section 15.4).
pub const ADMIN_URL_IGNORED: &str =
    "admin_url in config.toml is ignored; set KB_ADMIN_URL for kb db commands";

/// The user's home directory: `USERPROFILE`, else `HOME` (empty values are missing).
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

/// Database role a connection is made as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Writer,
    Reader,
    Admin,
}

impl Role {
    pub fn env_var(self) -> &'static str {
        match self {
            Role::Writer => "KB_WRITER_URL",
            Role::Reader => "KB_READER_URL",
            Role::Admin => "KB_ADMIN_URL",
        }
    }

    fn file_key(self) -> &'static str {
        match self {
            Role::Writer => "writer_url",
            Role::Reader => "reader_url",
            Role::Admin => "admin_url",
        }
    }
}

/// Resolved connection URLs. A missing URL is reported only when it is actually needed.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub writer_url: Option<String>,
    pub reader_url: Option<String>,
    pub admin_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    database: FileDatabase,
}

#[derive(Debug, Default, Deserialize)]
struct FileDatabase {
    writer_url: Option<String>,
    reader_url: Option<String>,
    /// Read only to warn that it is ignored.
    admin_url: Option<String>,
}

impl Config {
    /// Path of the config file: `~/.config/kb/config.toml`.
    pub fn file_path() -> Option<PathBuf> {
        Some(home_dir()?.join(".config").join("kb").join("config.toml"))
    }

    /// Loads the configuration from the environment and the config file, without its
    /// warnings: only `kb db` prints them (docs/board-v2.md section 15.4).
    pub fn load() -> Result<Config> {
        Self::load_with_warnings().map(|(config, _)| config)
    }

    /// Loads the configuration and returns the warnings instead of printing them (`kb db`
    /// prints them; every other command, `kb tui` and the hooks drop them).
    /// The file is read only when the writer or reader variable is unset; a missing file is
    /// not an error. The admin URL comes only from `KB_ADMIN_URL`.
    pub fn load_with_warnings() -> Result<(Config, Vec<String>)> {
        let env = |role: Role| std::env::var(role.env_var()).ok().filter(|v| !v.is_empty());
        let mut config = Config {
            writer_url: env(Role::Writer),
            reader_url: env(Role::Reader),
            admin_url: env(Role::Admin),
        };
        let mut warnings = Vec::new();
        if config.writer_url.is_some() && config.reader_url.is_some() {
            return Ok((config, warnings));
        }
        let Some(path) = Self::file_path() else {
            return Ok((config, warnings));
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((config, warnings)),
            Err(e) => {
                return Err(Error::NoConfig(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };
        let file: FileConfig = toml::from_str(&text)
            .map_err(|e| Error::NoConfig(format!("invalid {}: {e}", path.display())))?;
        let db = file.database;
        config.writer_url = config.writer_url.or(db.writer_url);
        config.reader_url = config.reader_url.or(db.reader_url);
        if db.admin_url.is_some() {
            warnings.push(ADMIN_URL_IGNORED.to_string());
        }
        Ok((config, warnings))
    }

    /// URL for `role`. A missing writer or reader URL is a `no database config:` error
    /// naming where to put it; a missing admin URL is [`Error::NoAdminUrl`] (exit 1).
    pub fn url(&self, role: Role) -> Result<&str> {
        let value = match role {
            Role::Writer => &self.writer_url,
            Role::Reader => &self.reader_url,
            Role::Admin => &self.admin_url,
        };
        value.as_deref().ok_or_else(|| {
            if role == Role::Admin {
                return Error::NoAdminUrl;
            }
            let file = Self::file_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.config/kb/config.toml".to_string());
            Error::NoConfig(format!(
                "set {} or put [database] {} in {file} (run scripts/setup.ps1 or scripts/setup.sh)",
                role.env_var(),
                role.file_key()
            ))
        })
    }
}
