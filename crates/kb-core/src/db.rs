//! Connections, embedded migrations, roles and grants (docs/cli.md sections 4.1 and 4.2).

use std::str::FromStr;
use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::{AssertSqlSafe, Connection, Row};

use crate::config::{Config, Role};
use crate::error::{Error, Result};

/// One embedded schema version.
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// All schema versions, oldest first. Files live in `migrations/` at the repository root.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "hooks_questions",
        sql: include_str!("../../../migrations/0002_hooks_questions.sql"),
    },
    Migration {
        version: 3,
        name: "epics_origin",
        sql: include_str!("../../../migrations/0003_epics_origin.sql"),
    },
    Migration {
        version: 4,
        name: "answer_text",
        sql: include_str!("../../../migrations/0004_answer_text.sql"),
    },
    Migration {
        version: 5,
        name: "wake_comments",
        sql: include_str!("../../../migrations/0005_wake_comments.sql"),
    },
    Migration {
        version: 6,
        name: "last_next",
        sql: include_str!("../../../migrations/0006_last_next.sql"),
    },
    Migration {
        version: 7,
        name: "because",
        sql: include_str!("../../../migrations/0007_because.sql"),
    },
    Migration {
        version: 8,
        name: "terminal_columns",
        sql: include_str!("../../../migrations/0008_terminal_columns.sql"),
    },
];

/// Schema version this binary expects.
pub fn latest_version() -> i32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens one connection. Every failure, including bad credentials, is
/// [`Error::Unreachable`] (exit code 3).
pub async fn connect(url: &str) -> Result<PgConnection> {
    connect_timeout(url, CONNECT_TIMEOUT).await
}

/// [`connect`] with another time limit; running out of time is [`Error::Unreachable`].
/// `kb hook` uses short limits (docs/board-v2.md section 12.9).
pub async fn connect_timeout(url: &str, limit: Duration) -> Result<PgConnection> {
    let options = PgConnectOptions::from_str(url)
        .map_err(|e| Error::NoConfig(format!("invalid database url: {e}")))?
        .application_name("kb");
    connect_with_limit(&options, limit).await
}

pub(crate) async fn connect_with(options: &PgConnectOptions) -> Result<PgConnection> {
    connect_with_limit(options, CONNECT_TIMEOUT).await
}

async fn connect_with_limit(options: &PgConnectOptions, limit: Duration) -> Result<PgConnection> {
    match tokio::time::timeout(limit, PgConnection::connect_with(options)).await {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(e)) => Err(Error::Unreachable(e.to_string())),
        Err(_) => Err(Error::Unreachable(format!(
            "timed out after {} ms connecting to {}:{}",
            limit.as_millis(),
            options.get_host(),
            options.get_port()
        ))),
    }
}

/// Connects as `role` using the URL from `config`.
pub async fn connect_role(config: &Config, role: Role) -> Result<PgConnection> {
    connect(config.url(role)?).await
}

/// Result of [`migrate`].
#[derive(Debug, Clone, Serialize)]
pub struct MigrateReport {
    pub database: String,
    pub created_database: bool,
    pub applied: Vec<i32>,
    pub version: i32,
}

/// Arbitrary constant for the advisory lock that serializes concurrent migrations.
const MIGRATE_LOCK: i64 = 0x6b62_6d69_6772; // "kbmigr"

/// Creates or updates database `database` (default: the database of `KB_ADMIN_URL`):
/// creates it if missing, creates the writer and reader roles named in `writer_url` and
/// `reader_url` (setting their passwords from those URLs), applies pending migrations and
/// grants privileges (docs/architecture.md section 3.1). Idempotent.
///
/// This is the confirmed form: it never asks. `kb db migrate` uses [`migrate_checked`].
pub async fn migrate(config: &Config, database: Option<&str>) -> Result<MigrateReport> {
    migrate_checked(config, database, true).await
}

/// What [`migrate_checked`] found in an existing target database before changing anything.
struct Existing {
    current: Option<i32>,
    pending: Vec<i32>,
    has_boards: bool,
}

/// Reads the schema version and whether the database has any board.
async fn inspect(conn: &mut PgConnection) -> Result<Existing> {
    let row = sqlx::query(
        "SELECT to_regclass('kb_schema_migrations') IS NOT NULL AS has_versions,
                to_regclass('board') IS NOT NULL AS has_board_table",
    )
    .fetch_one(&mut *conn)
    .await?;
    let has_versions: bool = row.try_get("has_versions")?;
    let has_board_table: bool = row.try_get("has_board_table")?;
    let done: Vec<i32> = if has_versions {
        sqlx::query_scalar("SELECT version FROM kb_schema_migrations")
            .fetch_all(&mut *conn)
            .await?
    } else {
        Vec::new()
    };
    let has_boards = if has_board_table {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM board)")
            .fetch_one(&mut *conn)
            .await?
    } else {
        false
    };
    Ok(Existing {
        current: done.iter().copied().max(),
        pending: pending_versions(&done),
        has_boards,
    })
}

/// Versions of [`MIGRATIONS`] not in `done`, oldest first.
pub fn pending_versions(done: &[i32]) -> Vec<i32> {
    MIGRATIONS
        .iter()
        .map(|m| m.version)
        .filter(|v| !done.contains(v))
        .collect()
}

/// [`migrate`], with the confirmation of docs/board-v2.md section 15.5: when the target
/// database already has a board and migrations are pending, nothing is changed unless
/// `confirmed`, and the result is [`Error::MigrationNeedsConfirm`] carrying the plan. The
/// check runs under the migration lock, before any change (database, roles, schema,
/// grants).
pub async fn migrate_checked(
    config: &Config,
    database: Option<&str>,
    confirmed: bool,
) -> Result<MigrateReport> {
    let admin = PgConnectOptions::from_str(config.url(Role::Admin)?)
        .map_err(|e| Error::NoConfig(format!("invalid KB_ADMIN_URL: {e}")))?
        .application_name("kb");
    let writer = RoleSpec::from_url(config.url(Role::Writer)?)?;
    let reader = RoleSpec::from_url(config.url(Role::Reader)?)?;
    let target = database
        .map(str::to_string)
        .or_else(|| admin.get_database().map(str::to_string))
        .unwrap_or_else(|| "kanban".to_string());

    // Maintenance connection to the `postgres` database: holds the global lock and creates
    // the target database (CREATE DATABASE cannot run while connected to it).
    let mut maint = connect_with(&admin.clone().database("postgres")).await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATE_LOCK)
        .execute(&mut maint)
        .await?;

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&target)
            .fetch_one(&mut maint)
            .await?;
    let mut existing_conn = None;
    if exists {
        let mut conn = connect_with(&admin.clone().database(&target)).await?;
        let found = inspect(&mut conn).await?;
        if found.has_boards && !found.pending.is_empty() && !confirmed {
            conn.close().await.ok();
            sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(MIGRATE_LOCK)
                .execute(&mut maint)
                .await?;
            maint.close().await.ok();
            return Err(Error::MigrationNeedsConfirm {
                database: target,
                current: found.current,
                pending: found.pending,
            });
        }
        existing_conn = Some(conn);
    } else {
        sqlx::query(AssertSqlSafe(format!(
            "CREATE DATABASE {}",
            quote_ident(&target)
        )))
        .execute(&mut maint)
        .await?;
    }

    for role in [&writer, &reader] {
        let role_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(&role.name)
                .fetch_one(&mut maint)
                .await?;
        let verb = if role_exists { "ALTER" } else { "CREATE" };
        let sql = format!(
            "{verb} ROLE {} WITH LOGIN PASSWORD {}",
            quote_ident(&role.name),
            quote_literal(&role.password)
        );
        sqlx::query(AssertSqlSafe(sql)).execute(&mut maint).await?;
    }

    let mut conn = match existing_conn {
        Some(conn) => conn,
        None => connect_with(&admin.clone().database(&target)).await?,
    };
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS kb_schema_migrations (
            version    integer PRIMARY KEY,
            name       text NOT NULL,
            applied_at timestamptz NOT NULL DEFAULT now()
        )",
    )
    .execute(&mut conn)
    .await?;
    let done: Vec<i32> = sqlx::query_scalar("SELECT version FROM kb_schema_migrations")
        .fetch_all(&mut conn)
        .await?;
    let mut applied = Vec::new();
    for m in MIGRATIONS.iter().filter(|m| !done.contains(&m.version)) {
        let mut tx = conn.begin().await?;
        sqlx::raw_sql(m.sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO kb_schema_migrations (version, name) VALUES ($1, $2)")
            .bind(m.version)
            .bind(m.name)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        applied.push(m.version);
    }

    let w = quote_ident(&writer.name);
    let r = quote_ident(&reader.name);
    let grants = format!(
        "GRANT CONNECT ON DATABASE {db} TO {w}, {r};
         GRANT USAGE ON SCHEMA public TO {w}, {r};
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {w};
         GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {w};
         REVOKE INSERT, UPDATE, DELETE ON kb_schema_migrations FROM {w};
         REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {r};
         GRANT SELECT ON ALL TABLES IN SCHEMA public TO {r};
         REVOKE ALL ON FUNCTION kb_answer_question(bigint, bigint[], boolean, text) FROM PUBLIC;
         GRANT EXECUTE ON FUNCTION kb_answer_question(bigint, bigint[], boolean, text) TO {w}, {r};
         REVOKE ALL ON FUNCTION kb_add_comment(bigint, text) FROM PUBLIC;
         GRANT EXECUTE ON FUNCTION kb_add_comment(bigint, text) TO {w}, {r};",
        db = quote_ident(&target)
    );
    sqlx::raw_sql(AssertSqlSafe(grants))
        .execute(&mut conn)
        .await?;
    conn.close().await.ok();

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATE_LOCK)
        .execute(&mut maint)
        .await?;
    maint.close().await.ok();

    Ok(MigrateReport {
        database: target,
        created_database: !exists,
        applied,
        version: latest_version(),
    })
}

/// Output of `kb db status`.
#[derive(Debug, Clone, Serialize)]
pub struct DbStatus {
    pub connected: bool,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Highest applied schema version; `None` when the schema was never migrated.
    pub schema_version: Option<i32>,
    pub latest_version: i32,
    pub workspaces: i64,
    pub active_boards: i64,
}

/// Connects with `url` and reports schema version and counts.
pub async fn status(url: &str) -> Result<DbStatus> {
    let options = PgConnectOptions::from_str(url)
        .map_err(|e| Error::NoConfig(format!("invalid database url: {e}")))?
        .application_name("kb");
    let mut conn = connect_with(&options).await?;
    let migrated: bool =
        sqlx::query_scalar("SELECT to_regclass('kb_schema_migrations') IS NOT NULL")
            .fetch_one(&mut conn)
            .await?;
    let (schema_version, workspaces, active_boards) = if migrated {
        let row = sqlx::query(
            "SELECT (SELECT max(version) FROM kb_schema_migrations) AS version,
                    (SELECT count(*) FROM workspace) AS workspaces,
                    (SELECT count(*) FROM board WHERE status = 'active') AS active_boards",
        )
        .fetch_one(&mut conn)
        .await?;
        (
            row.try_get("version")?,
            row.try_get("workspaces")?,
            row.try_get("active_boards")?,
        )
    } else {
        (None, 0, 0)
    };
    Ok(DbStatus {
        connected: true,
        host: options.get_host().to_string(),
        port: options.get_port(),
        database: options.get_database().unwrap_or_default().to_string(),
        user: options.get_username().to_string(),
        schema_version,
        latest_version: latest_version(),
        workspaces,
        active_boards,
    })
}

struct RoleSpec {
    name: String,
    password: String,
}

impl RoleSpec {
    fn from_url(raw: &str) -> Result<RoleSpec> {
        let url = url::Url::parse(raw)
            .map_err(|e| Error::NoConfig(format!("invalid database url: {e}")))?;
        let decode = |s: &str| percent_decode(s);
        let name = decode(url.username());
        let password = url.password().map(decode).unwrap_or_default();
        if name.is_empty() || password.is_empty() {
            return Err(Error::NoConfig(
                "writer_url and reader_url must contain a user name and password".to_string(),
            ));
        }
        Ok(RoleSpec { name, password })
    }
}

fn percent_decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Quotes an SQL identifier: `a"b` → `"a""b"`.
pub(crate) fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Quotes an SQL string literal: `a'b` → `'a''b'`.
pub(crate) fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
