//! The remembered `next:` line and the migration confirmation (docs/board-v2.md sections
//! 15.2 and 15.5) against `kanban_test`; the plan and message checks need no database.

mod common;

use std::path::Path;
use std::time::Duration;

use kb_core::events::{EventListener, ListenEvent};
use kb_core::write::{self, BoardInit};
use kb_core::{Error, PgConnection, Role, db, read, resolve};

async fn new_board(conn: &mut PgConnection) -> (String, i64) {
    let session = common::unique("next-echo");
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    let args = BoardInit {
        title: Some("下一張".into()),
        goal: None,
        lanes: Some(write::parse_lanes("待辦:todo,完成:done").unwrap()),
        epics: None,
        cwd: None,
    };
    let out = write::board_init(conn, &session, args, || ws)
        .await
        .expect("board init");
    (session, out.board_id)
}

async fn board_row(conn: &mut PgConnection, session: &str) -> kb_core::model::Board {
    read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn record_next_reports_changes_without_touching_rev_or_notifying() {
    let config = common::config();
    common::ensure_migrated().await;
    let mut conn = common::writer().await;
    let (session, id) = new_board(&mut conn).await;
    let mut listener = EventListener::connect(config.url(Role::Reader).unwrap())
        .await
        .unwrap();
    let before = board_row(&mut conn, &session).await;

    // Nothing remembered: the first line is a change; the same line again is not.
    let a = "next: #1 甲 (待辦)";
    assert!(write::record_next(&mut conn, id, a).await.unwrap());
    assert!(!write::record_next(&mut conn, id, a).await.unwrap());
    assert!(
        write::record_next(&mut conn, id, "next: (no card in the todo lanes)")
            .await
            .unwrap()
    );
    // Forgetting makes the next line a change again, even when it is the same text.
    assert!(write::record_next(&mut conn, id, a).await.unwrap());
    write::forget_next(&mut conn, id).await.unwrap();
    assert!(write::record_next(&mut conn, id, a).await.unwrap());

    // rev and updated_at are untouched; no notification on kb_events.
    let after = board_row(&mut conn, &session).await;
    assert_eq!(after.rev, before.rev);
    assert_eq!(after.updated_at, before.updated_at);
    while let Ok(ev) = tokio::time::timeout(Duration::from_millis(400), listener.next()).await {
        if let ListenEvent::Board(e) = ev.expect("listener") {
            assert_ne!(e.board, id, "remembering next: must not notify");
        }
    }
}

#[tokio::test]
async fn concurrent_commands_print_the_same_line_once() {
    let mut conn = common::writer().await;
    let (_, id) = new_board(&mut conn).await;
    for round in 0..5 {
        let line = format!("next: #{round} 同時 (待辦)");
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let line = line.clone();
            tasks.push(tokio::spawn(async move {
                let mut c = common::writer().await;
                write::record_next(&mut c, id, &line).await.unwrap()
            }));
        }
        let mut printed = 0;
        for t in tasks {
            if t.await.unwrap() {
                printed += 1;
            }
        }
        assert_eq!(printed, 1, "round {round}");
    }
}

#[test]
fn pending_versions_and_the_plan_message() {
    let latest = db::latest_version();
    assert!(latest >= 6);
    let all: Vec<i32> = (1..=latest).collect();
    assert!(db::pending_versions(&all).is_empty());
    let done: Vec<i32> = (1..=latest - 2).collect();
    assert_eq!(db::pending_versions(&done), vec![latest - 1, latest]);

    let e = Error::MigrationNeedsConfirm {
        database: "kanban".into(),
        current: Some(5),
        pending: vec![6],
    };
    assert_eq!(e.exit_code(), 1);
    assert_eq!(
        e.to_string(),
        "migration plan for database kanban: schema version 5, pending 6\n\
         the database already has boards; nothing was changed. \
         To apply: kb db migrate --database kanban --yes"
    );
    let e = Error::MigrationNeedsConfirm {
        database: "x".into(),
        current: None,
        pending: vec![1, 2],
    };
    assert!(
        e.to_string()
            .starts_with("migration plan for database x: schema version none, pending 1, 2\n"),
        "{e}"
    );
    assert_eq!(Error::NoAdminUrl.exit_code(), 1);
}

#[tokio::test]
async fn migrate_checked_without_pending_migrations_needs_no_confirmation() {
    // kanban_test has boards; once it is current, the unconfirmed form succeeds unchanged.
    let config = common::config();
    common::ensure_migrated().await;
    let mut conn = common::writer().await;
    new_board(&mut conn).await;
    let report = db::migrate_checked(&config, Some(common::TEST_DATABASE), false)
        .await
        .expect("nothing pending");
    assert_eq!(report.database, common::TEST_DATABASE);
    assert!(report.applied.is_empty());
    assert!(!report.created_database);
}
