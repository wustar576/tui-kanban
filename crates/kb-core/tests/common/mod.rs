//! Test harness: every test talks to database `kanban_test`, never `kanban`.
//!
//! Connection URLs come from the normal configuration (writer and reader: environment or
//! `~/.config/kb/config.toml`; admin: `KB_ADMIN_URL` only, docs/board-v2.md section 15.4)
//! with the database name replaced by `kanban_test`. The schema is migrated once per test
//! process.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};

use kb_core::{Config, PgConnection, Role, db};
use tokio::sync::OnceCell;

pub const TEST_DATABASE: &str = "kanban_test";

static MIGRATED: OnceCell<()> = OnceCell::const_new();

fn with_test_database(raw: &str) -> String {
    let mut url = url::Url::parse(raw).expect("valid database url");
    url.set_path(TEST_DATABASE);
    url.to_string()
}

/// Configuration pointing every role at `kanban_test`.
pub fn config() -> Config {
    let base = Config::load().expect("load config");
    let swap = |role: Role| {
        Some(with_test_database(base.url(role).expect(
            "database url configured (the tests also need KB_ADMIN_URL)",
        )))
    };
    let config = Config {
        writer_url: swap(Role::Writer),
        reader_url: swap(Role::Reader),
        admin_url: swap(Role::Admin),
    };
    for role in [Role::Writer, Role::Reader, Role::Admin] {
        assert!(
            config.url(role).unwrap().ends_with("/kanban_test"),
            "tests must use kanban_test"
        );
    }
    config
}

/// Migrates `kanban_test` once per process.
pub async fn ensure_migrated() {
    MIGRATED
        .get_or_init(|| async {
            db::migrate(&config(), Some(TEST_DATABASE))
                .await
                .expect("migrate kanban_test");
        })
        .await;
}

pub async fn writer() -> PgConnection {
    ensure_migrated().await;
    db::connect_role(&config(), Role::Writer)
        .await
        .expect("connect as writer")
}

pub async fn reader() -> PgConnection {
    ensure_migrated().await;
    db::connect_role(&config(), Role::Reader)
        .await
        .expect("connect as reader")
}

/// A key unique across test runs and parallel tests, e.g. for session keys.
pub fn unique(prefix: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "test-{prefix}-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}
