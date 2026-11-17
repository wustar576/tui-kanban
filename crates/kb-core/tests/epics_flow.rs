//! Board v2 integration tests against `kanban_test`: epics, card origin, the new read API
//! fields and migration 0003 (docs/board-v2.md sections 3 and 4).

mod common;

use std::path::Path;
use std::time::Duration;

use kb_core::events::{BoardEvent, EventListener, ListenEvent};
use kb_core::model::{LinkKind, Origin};
use kb_core::read::{self, BoardSnapshot, CardSummary, EpicCounts};
use kb_core::resolve;
use kb_core::write::{self, BoardInit, CardAdd, CardEdit, LinkTarget};
use kb_core::{Error, PgConnection, Role};
use serde_json::{Value, json};

const LANES: &str = "待辦:todo,實作:doing,完成:done,捨棄:dropped";

async fn new_board(conn: &mut PgConnection, epics: Option<&str>) -> String {
    let session = common::unique("v2");
    let args = BoardInit {
        title: Some("epic 看板".into()),
        goal: None,
        lanes: Some(write::parse_lanes(LANES).unwrap()),
        epics: epics.map(|e| write::parse_epics(e).unwrap()),
        cwd: None,
    };
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    write::board_init(conn, &session, args, || ws)
        .await
        .unwrap();
    session
}

async fn board_id(conn: &mut PgConnection, session: &str) -> i64 {
    read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn rev(conn: &mut PgConnection, session: &str) -> i64 {
    read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap()
        .rev
}

async fn snapshot(conn: &mut PgConnection, session: &str) -> BoardSnapshot {
    let id = board_id(conn, session).await;
    read::board_snapshot(conn, id).await.unwrap().unwrap()
}

fn card(snap: &BoardSnapshot, seq: i32) -> &CardSummary {
    snap.card(seq).expect("card on board").1
}

fn epic_names(snap: &BoardSnapshot) -> Vec<(String, i32)> {
    snap.epics
        .iter()
        .map(|e| (e.epic.name.clone(), e.epic.position))
        .collect()
}

/// Adds a card and returns its number.
async fn add(
    conn: &mut PgConnection,
    session: &str,
    lane: &str,
    title: &str,
    f: impl FnOnce(&mut CardAdd),
) -> i32 {
    let mut args = CardAdd::new(lane, title);
    f(&mut args);
    write::card_add(conn, session, args)
        .await
        .unwrap()
        .card
        .unwrap()
}

async fn last_activity(conn: &mut PgConnection, session: &str, n: i64) -> Vec<(String, Value)> {
    let id = board_id(conn, session).await;
    read::recent_activity(conn, id, n)
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.verb, a.detail))
        .collect()
}

fn data_error(err: Error, message: &str) {
    assert_eq!(err.to_string(), message);
    assert_eq!(err.exit_code(), 1);
}

fn counts(total: i64, done: i64, derived: i64, discussing: i64) -> EpicCounts {
    EpicCounts {
        total,
        done,
        derived,
        discussing,
    }
}

// --- epics (4.1, 4.3) ----------------------------------------------------------------------

#[tokio::test]
async fn board_init_creates_epics_in_order() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, Some("P0 資料庫, P1 只讀畫面,P2")).await;
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(
        epic_names(&snap),
        [
            ("P0 資料庫".into(), 1),
            ("P1 只讀畫面".into(), 2),
            ("P2".into(), 3)
        ]
    );
    assert!(snap.epics.iter().all(|e| e.counts == EpicCounts::default()));
    assert_eq!(snap.no_epic, None);
    let rows = last_activity(&mut conn, &s, 10).await;
    assert_eq!(rows[0], ("epic.add".into(), json!({"epic": "P2"})));
    assert_eq!(rows[3].0, "board.create");

    // Re-running init appends only the epics the board does not have yet.
    let args = BoardInit {
        title: Some("epic 看板".into()),
        epics: Some(write::parse_epics("P2,P3").unwrap()),
        ..Default::default()
    };
    let out = write::board_init(&mut conn, &s, args, || panic!("exists"))
        .await
        .unwrap();
    assert_eq!(out.message, "board updated, 1 epics created");
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(snap.epics.len(), 4);
    assert_eq!(snap.epics[3].epic.name, "P3");

    data_error(
        write::parse_epics("a,b,a").unwrap_err(),
        "duplicate epic name: a",
    );
    data_error(
        write::parse_epics(" , ").unwrap_err(),
        "--epics needs at least one name",
    );
}

#[tokio::test]
async fn epic_commands_and_their_failures() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, Some("P0,P1")).await;

    // add: last by default, after another epic with --after.
    let out = write::epic_add(&mut conn, &s, "P3", None).await.unwrap();
    assert_eq!(out.message, "epic P3 added at position 3");
    let out = write::epic_add(&mut conn, &s, "P2", Some("P1"))
        .await
        .unwrap();
    assert_eq!(out.message, "epic P2 added at position 3");
    assert_eq!(
        epic_names(&snapshot(&mut conn, &s).await),
        [
            ("P0".into(), 1),
            ("P1".into(), 2),
            ("P2".into(), 3),
            ("P3".into(), 4)
        ]
    );
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0],
        ("epic.add".into(), json!({"epic": "P2"}))
    );
    data_error(
        write::epic_add(&mut conn, &s, "P1", None)
            .await
            .unwrap_err(),
        "epic already exists: P1",
    );
    data_error(
        write::epic_add(&mut conn, &s, "P9", Some("無"))
            .await
            .unwrap_err(),
        "epic not found: 無 (epics: P0, P1, P2, P3)",
    );
    data_error(
        write::epic_add(&mut conn, &s, " ", None).await.unwrap_err(),
        "epic name must not be empty",
    );

    // rename.
    write::epic_rename(&mut conn, &s, "P2", "P2 hook 與守則")
        .await
        .unwrap();
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0],
        (
            "epic.rename".into(),
            json!({"from": "P2", "to": "P2 hook 與守則"})
        )
    );
    data_error(
        write::epic_rename(&mut conn, &s, "P0", "P1")
            .await
            .unwrap_err(),
        "epic already exists: P1",
    );
    data_error(
        write::epic_rename(&mut conn, &s, "P2", "X")
            .await
            .unwrap_err(),
        "epic not found: P2 (epics: P0, P1, P2 hook 與守則, P3)",
    );

    // move.
    write::epic_move(&mut conn, &s, "P3", 1).await.unwrap();
    assert_eq!(
        epic_names(&snapshot(&mut conn, &s).await),
        [
            ("P3".into(), 1),
            ("P0".into(), 2),
            ("P1".into(), 3),
            ("P2 hook 與守則".into(), 4)
        ]
    );
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0],
        ("epic.move".into(), json!({"epic": "P3", "position": 1}))
    );
    for bad in [0, 5] {
        data_error(
            write::epic_move(&mut conn, &s, "P3", bad)
                .await
                .unwrap_err(),
            &format!("invalid position: {bad} (board has 4 epics)"),
        );
    }

    // remove: only an empty epic, also when its only card is closed.
    let c = add(&mut conn, &s, "待辦", "卡片", |a| {
        a.epic = Some("P0".into())
    })
    .await;
    write::card_drop(&mut conn, &s, c, "不需要").await.unwrap();
    data_error(
        write::epic_remove(&mut conn, &s, "P0").await.unwrap_err(),
        "epic is not empty",
    );
    write::epic_remove(&mut conn, &s, "P3").await.unwrap();
    assert_eq!(
        epic_names(&snapshot(&mut conn, &s).await),
        [
            ("P0".into(), 1),
            ("P1".into(), 2),
            ("P2 hook 與守則".into(), 3)
        ]
    );
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0],
        ("epic.remove".into(), json!({"epic": "P3"}))
    );
    data_error(
        write::epic_remove(&mut conn, &s, "P3").await.unwrap_err(),
        "epic not found: P3 (epics: P0, P1, P2 hook 與守則)",
    );
}

// --- origin (3.2, 4.2) ---------------------------------------------------------------------

#[tokio::test]
async fn origin_defaults_confirm_and_discuss() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, None).await;
    let derived = add(&mut conn, &s, "待辦", "衍生", |_| {}).await;
    let discussing = add(&mut conn, &s, "待辦", "討論", |a| {
        a.origin = Origin::Discussing
    })
    .await;
    let agreed = add(&mut conn, &s, "待辦", "約定", |a| {
        a.origin = Origin::Agreed
    })
    .await;
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(card(&snap, derived).card.origin, Origin::Derived);
    assert_eq!(card(&snap, discussing).card.origin, Origin::Discussing);
    assert_eq!(card(&snap, agreed).card.origin, Origin::Agreed);

    // confirm several cards in one write; one already agreed.
    let out = write::card_set_origin(
        &mut conn,
        &s,
        &[discussing, agreed, derived],
        Origin::Agreed,
    )
    .await
    .unwrap();
    assert_eq!(
        out.message,
        format!(
            "card {discussing}: discussing -> agreed\ncard {agreed} is already agreed\n\
             card {derived}: derived -> agreed"
        )
    );
    let rows = last_activity(&mut conn, &s, 2).await;
    assert_eq!(
        rows[1],
        (
            "card.origin".into(),
            json!({"seq": discussing, "from": "discussing", "to": "agreed"})
        )
    );
    assert_eq!(
        rows[0],
        (
            "card.origin".into(),
            json!({"seq": derived, "from": "derived", "to": "agreed"})
        )
    );

    // Already that value: success, no activity, no rev bump.
    let before = rev(&mut conn, &s).await;
    let rows_before = last_activity(&mut conn, &s, 1).await;
    let out = write::card_set_origin(&mut conn, &s, &[agreed], Origin::Agreed)
        .await
        .unwrap();
    assert_eq!(out.message, format!("card {agreed} is already agreed"));
    assert_eq!(out.rev, before);
    assert_eq!(rev(&mut conn, &s).await, before);
    assert_eq!(last_activity(&mut conn, &s, 1).await, rows_before);

    // discuss.
    write::card_set_origin(&mut conn, &s, &[agreed], Origin::Discussing)
        .await
        .unwrap();
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0].1,
        json!({"seq": agreed, "from": "agreed", "to": "discussing"})
    );

    // An unknown card fails the whole command.
    let before = rev(&mut conn, &s).await;
    let err = write::card_set_origin(&mut conn, &s, &[derived, 999], Origin::Discussing)
        .await
        .unwrap_err();
    data_error(err, "card not found: 999");
    assert_eq!(rev(&mut conn, &s).await, before);
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(card(&snap, derived).card.origin, Origin::Agreed);
}

#[tokio::test]
async fn moving_a_discussing_card_into_doing_warns() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, None).await;
    let d = add(&mut conn, &s, "待辦", "還在討論", |a| {
        a.origin = Origin::Discussing
    })
    .await;
    let a = add(&mut conn, &s, "待辦", "已約定", |a| {
        a.origin = Origin::Agreed
    })
    .await;
    let out = write::card_move(&mut conn, &s, d, "實作", None)
        .await
        .unwrap();
    // R7's immediate echo (docs/board-v2.md section 17.5).
    assert_eq!(
        out.warnings,
        [format!(
            "R7: card {d} is still under discussion; run \"kb card confirm {d}\" once the user \
             agreed, or move it back"
        )]
    );
    let out = write::card_move(&mut conn, &s, a, "實作", None)
        .await
        .unwrap();
    assert!(out.warnings.is_empty());
    let out = write::card_move(&mut conn, &s, d, "完成", None)
        .await
        .unwrap();
    assert!(out.warnings.is_empty(), "only doing lanes warn");
}

// --- card epic (4.2) -----------------------------------------------------------------------

#[tokio::test]
async fn from_inherits_the_epic_and_edit_changes_it() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, Some("P1,P2")).await;
    let root = add(&mut conn, &s, "實作", "根", |a| {
        a.epic = Some("P1".into())
    })
    .await;
    let child = add(&mut conn, &s, "待辦", "延伸", |a| a.from = Some(root)).await;
    let other = add(&mut conn, &s, "待辦", "延伸到別的 epic", |a| {
        a.from = Some(root);
        a.epic = Some("P2".into());
    })
    .await;
    let loose = add(&mut conn, &s, "待辦", "沒有 epic", |_| {}).await;
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(card(&snap, child).epic_name.as_deref(), Some("P1"));
    assert_eq!(card(&snap, other).epic_name.as_deref(), Some("P2"));
    assert_eq!(card(&snap, loose).epic_name, None);
    assert_eq!(card(&snap, loose).card.epic_id, None);
    let err = write::card_add(&mut conn, &s, {
        let mut a = CardAdd::new("待辦", "x");
        a.epic = Some("P9".into());
        a
    })
    .await
    .unwrap_err();
    data_error(err, "epic not found: P9 (epics: P1, P2)");

    // --epic.
    let edit = |epic: Option<Option<&str>>| CardEdit {
        epic: epic.map(|e| e.map(str::to_string)),
        ..Default::default()
    };
    let out = write::card_edit(&mut conn, &s, loose, edit(Some(Some("P2"))))
        .await
        .unwrap();
    assert_eq!(out.message, format!("card {loose} edited (epic P2)"));
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0],
        (
            "card.epic".into(),
            json!({"seq": loose, "from": null, "to": "P2"})
        )
    );
    // Same epic again: nothing written.
    let before = rev(&mut conn, &s).await;
    let out = write::card_edit(&mut conn, &s, loose, edit(Some(Some("P2"))))
        .await
        .unwrap();
    assert_eq!(out.message, format!("card {loose} unchanged"));
    assert_eq!(rev(&mut conn, &s).await, before);
    // With other fields: card.edit and card.epic in one write.
    let mut e = edit(Some(Some("P1")));
    e.title = Some("改名".into());
    write::card_edit(&mut conn, &s, loose, e).await.unwrap();
    assert_eq!(rev(&mut conn, &s).await, before + 1);
    let rows = last_activity(&mut conn, &s, 2).await;
    assert_eq!(rows[1].0, "card.edit");
    assert_eq!(rows[0].1, json!({"seq": loose, "from": "P2", "to": "P1"}));
    // --no-epic.
    write::card_edit(&mut conn, &s, loose, edit(Some(None)))
        .await
        .unwrap();
    assert_eq!(
        last_activity(&mut conn, &s, 1).await[0].1,
        json!({"seq": loose, "from": "P1", "to": null})
    );
    data_error(
        write::card_edit(&mut conn, &s, loose, edit(None))
            .await
            .unwrap_err(),
        "nothing to edit: pass --title, --body, --priority, --owner, --epic or --no-epic",
    );
}

// --- read API (3.4, 4.3) -------------------------------------------------------------------

#[tokio::test]
async fn epic_and_board_counts_as_reader() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, Some("P1,P2,P3")).await;
    let in_p1 = |o: Origin| {
        move |a: &mut CardAdd| {
            a.epic = Some("P1".into());
            a.origin = o;
        }
    };
    // P1: 2 done (agreed, derived), 1 doing derived, 1 todo discussing, 1 dropped discussing.
    let d1 = add(&mut conn, &s, "待辦", "一", in_p1(Origin::Agreed)).await;
    let d2 = add(&mut conn, &s, "待辦", "二", in_p1(Origin::Derived)).await;
    add(&mut conn, &s, "實作", "三", in_p1(Origin::Derived)).await;
    add(&mut conn, &s, "待辦", "四", in_p1(Origin::Discussing)).await;
    let dropped = add(&mut conn, &s, "待辦", "五", in_p1(Origin::Discussing)).await;
    write::card_done(&mut conn, &s, d1, "好了").await.unwrap();
    write::card_done(&mut conn, &s, d2, "好了").await.unwrap();
    write::card_drop(&mut conn, &s, dropped, "不做")
        .await
        .unwrap();
    // P2: finished (one done card), a discussing card that is done does not count on the board.
    let p2 = add(&mut conn, &s, "待辦", "六", |a| {
        a.epic = Some("P2".into());
        a.origin = Origin::Discussing;
    })
    .await;
    write::card_done(&mut conn, &s, p2, "好了").await.unwrap();
    // No epic: one discussing card in todo, one dropped.
    add(&mut conn, &s, "待辦", "七", |a| {
        a.origin = Origin::Discussing
    })
    .await;
    let gone = add(&mut conn, &s, "待辦", "八", |_| {}).await;
    write::card_drop(&mut conn, &s, gone, "不做").await.unwrap();

    let id = board_id(&mut conn, &s).await;
    let mut reader = common::reader().await;
    let snap = read::board_snapshot(&mut reader, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.epics[0].counts, counts(4, 2, 2, 1));
    assert_eq!(snap.epics[1].counts, counts(1, 1, 0, 1));
    assert!(snap.epics[1].counts.is_finished());
    assert!(!snap.epics[0].counts.is_finished());
    assert_eq!(snap.epics[2].counts, EpicCounts::default());
    assert!(
        !snap.epics[2].counts.is_finished(),
        "an empty epic is not finished"
    );
    assert_eq!(snap.no_epic, Some(counts(1, 0, 0, 1)));
    // Discussing cards on the board: #4 (todo) and #7 (todo); #5 dropped and #6 done excluded.
    assert_eq!(snap.discussing_count, 2);
    assert_eq!(card(&snap, d1).epic_name.as_deref(), Some("P1"));

    // Card detail as reader: epic name and origin.
    let d = read::card_detail(&mut reader, id, p2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.epic_name.as_deref(), Some("P2"));
    assert_eq!(d.card.origin, Origin::Discussing);
    let d = read::card_detail(&mut reader, id, gone)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.epic_name, None);

    // The JSON of the snapshot carries the same data.
    let v = serde_json::to_value(&snap).unwrap();
    assert_eq!(v["discussing_count"], 2);
    assert_eq!(
        v["epics"][0],
        json!({
            "id": snap.epics[0].epic.id, "board_id": id, "name": "P1", "position": 1,
            "created_at": v["epics"][0]["created_at"],
            "total": 4, "done": 2, "derived": 2, "discussing": 1
        })
    );
    assert_eq!(
        v["no_epic"],
        json!({"total": 1, "done": 0, "derived": 0, "discussing": 1})
    );
    let c = &v["lanes"][0]["cards"][0];
    for key in [
        "origin",
        "epic_id",
        "epic_name",
        "spawned_from",
        "spawned_count",
        "blocked_by",
        "open_question_count",
    ] {
        assert!(c.get(key).is_some(), "card JSON lacks {key}: {c}");
    }
}

#[tokio::test]
async fn blocked_by_and_spawn_relations() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, None).await;
    let target = add(&mut conn, &s, "待辦", "被阻擋", |_| {}).await;
    let a = add(&mut conn, &s, "實作", "甲", |_| {}).await;
    let b = add(&mut conn, &s, "實作", "乙", |_| {}).await;
    let c = add(&mut conn, &s, "實作", "丙", |_| {}).await;
    for from in [c, a, b] {
        write::link_add(
            &mut conn,
            &s,
            from,
            LinkKind::Blocks,
            LinkTarget::Card(target),
            None,
        )
        .await
        .unwrap();
    }
    // A relates link is not a blocker.
    write::link_add(
        &mut conn,
        &s,
        target,
        LinkKind::Relates,
        LinkTarget::Card(a),
        None,
    )
    .await
    .unwrap();
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(card(&snap, target).blocked_by, [a, b, c]);
    assert!(card(&snap, a).blocked_by.is_empty());

    // Blockers in done or dropped lanes no longer block.
    write::card_done(&mut conn, &s, a, "好了").await.unwrap();
    write::card_drop(&mut conn, &s, c, "不做").await.unwrap();
    let snap = snapshot(&mut conn, &s).await;
    assert_eq!(card(&snap, target).blocked_by, [b]);

    // Spawn chain three levels deep: root -> mid -> leaf, root -> sibling.
    let root = add(&mut conn, &s, "待辦", "根", |_| {}).await;
    let mid = add(&mut conn, &s, "待辦", "中", |x| x.from = Some(root)).await;
    let leaf = add(&mut conn, &s, "待辦", "葉", |x| x.from = Some(mid)).await;
    let sibling = add(&mut conn, &s, "待辦", "旁", |x| x.from = Some(root)).await;
    let mut reader = common::reader().await;
    let id = board_id(&mut conn, &s).await;
    let snap = read::board_snapshot(&mut reader, id)
        .await
        .unwrap()
        .unwrap();
    let rel = |seq| {
        let c = card(&snap, seq);
        (c.spawned_from, c.spawned_count)
    };
    assert_eq!(rel(root), (None, 2));
    assert_eq!(rel(mid), (Some(root), 1));
    assert_eq!(rel(leaf), (Some(mid), 0));
    assert_eq!(rel(sibling), (Some(root), 0));
}

// --- notifications ---------------------------------------------------------------------------

async fn board_events(listener: &mut EventListener, board_id: i64) -> Vec<BoardEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = tokio::time::timeout(Duration::from_millis(400), listener.next()).await {
        match ev.expect("listener") {
            ListenEvent::Board(e) if e.board == board_id => events.push(e),
            ListenEvent::Board(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    events
}

#[tokio::test]
async fn each_new_write_sends_one_notification() {
    let config = common::config();
    common::ensure_migrated().await;
    let mut listener = EventListener::connect(config.url(Role::Reader).unwrap())
        .await
        .unwrap();
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, Some("P1")).await;
    let id = board_id(&mut conn, &s).await;
    let x = add(&mut conn, &s, "待辦", "甲", |a| {
        a.origin = Origin::Discussing
    })
    .await;
    let y = add(&mut conn, &s, "待辦", "乙", |a| {
        a.origin = Origin::Discussing
    })
    .await;
    let _ = board_events(&mut listener, id).await;

    let mut expect_one = async |conn: &mut PgConnection| {
        let events = board_events(&mut listener, id).await;
        let rev = rev(conn, &s).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].rev, rev);
    };
    write::epic_add(&mut conn, &s, "P2", None).await.unwrap();
    expect_one(&mut conn).await;
    // Reordering a lane (docs/board-v2.md 10.2): one notification; a no-op sends none.
    write::card_next(&mut conn, &s, y).await.unwrap();
    expect_one(&mut conn).await;
    // Two cards change origin: two activity rows, one notification.
    write::card_set_origin(&mut conn, &s, &[x, y], Origin::Agreed)
        .await
        .unwrap();
    expect_one(&mut conn).await;
    let mut e = CardEdit {
        epic: Some(Some("P2".into())),
        ..Default::default()
    };
    e.priority = Some(1);
    write::card_edit(&mut conn, &s, x, e).await.unwrap();
    expect_one(&mut conn).await;
    write::epic_move(&mut conn, &s, "P2", 1).await.unwrap();
    expect_one(&mut conn).await;
    write::epic_rename(&mut conn, &s, "P2", "P0").await.unwrap();
    expect_one(&mut conn).await;
    write::epic_add(&mut conn, &s, "空", None).await.unwrap();
    expect_one(&mut conn).await;
    write::epic_remove(&mut conn, &s, "空").await.unwrap();
    expect_one(&mut conn).await;

    // A no-op write sends nothing.
    write::card_set_origin(&mut conn, &s, &[x], Origin::Agreed)
        .await
        .unwrap();
    assert!(board_events(&mut listener, id).await.is_empty());
    // Neither does a reorder that changes nothing (y is already first).
    write::card_order(&mut conn, &s, &[y, x]).await.unwrap();
    assert!(board_events(&mut listener, id).await.is_empty());
}

// --- migration 0003 --------------------------------------------------------------------------

#[tokio::test]
async fn card_insert_without_new_columns_still_works() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn, None).await;
    let id = board_id(&mut conn, &s).await;
    // The INSERT a version-2 binary sends: no epic_id, no origin.
    sqlx::query(
        "INSERT INTO card (board_id, seq, lane_id, title, body, priority, position, owner, closed_at)
         SELECT $1, 1, l.id, '舊版卡片', '', 2, 1, 'agent', NULL FROM lane l
         WHERE l.board_id = $1 AND l.position = 1",
    )
    .bind(id)
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query("UPDATE board SET next_card_seq = 2 WHERE id = $1")
        .bind(id)
        .execute(&mut conn)
        .await
        .unwrap();
    let mut reader = common::reader().await;
    let snap = read::board_snapshot(&mut reader, id)
        .await
        .unwrap()
        .unwrap();
    let c = card(&snap, 1);
    assert_eq!((c.card.origin, c.card.epic_id), (Origin::Derived, None));
    assert_eq!(snap.no_epic, Some(counts(1, 0, 1, 0)));
    // The epic table is readable by kb_reader.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM epic WHERE board_id = $1")
        .bind(id)
        .fetch_one(&mut reader)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // A card's epic must belong to the card's board.
    let other = new_board(&mut conn, Some("別的看板")).await;
    let other_snap = snapshot(&mut conn, &other).await;
    let err = sqlx::query("UPDATE card SET epic_id = $2 WHERE board_id = $1 AND seq = 1")
        .bind(id)
        .bind(other_snap.epics[0].epic.id)
        .execute(&mut conn)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("card_epic_fk"), "{err}");
}
