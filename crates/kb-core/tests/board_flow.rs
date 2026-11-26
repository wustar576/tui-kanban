//! Integration tests for writes and reads against `kanban_test`.

mod common;

use std::path::Path;
use std::time::Duration;

use kb_core::events::{BoardEvent, EventListener, ListenEvent};
use kb_core::model::{BoardStatus, LaneKind, LinkKind, NoteKind, Origin, Outcome};
use kb_core::read::{self, BoardQuery, LinkDirection};
use kb_core::resolve::{self, SpaceContext, WorkspaceTarget};
use kb_core::write::{self, BoardInit, CardAdd, LinkTarget};
use kb_core::{Error, PgConnection, Role};
use serde_json::json;

fn manual_ws(name: &str) -> WorkspaceTarget {
    resolve::resolve_workspace(Some(name), None, Path::new("."))
}

/// Creates a board for a fresh session and returns the session key.
async fn new_board(conn: &mut PgConnection, lanes: Option<&str>) -> String {
    let session = common::unique("board");
    let args = BoardInit {
        title: Some("測試看板".into()),
        goal: Some("目標".into()),
        lanes: lanes.map(|l| write::parse_lanes(l).unwrap()),
        epics: None,
        cwd: None,
    };
    let ws = manual_ws(&common::unique("ws"));
    write::board_init(conn, &session, args, || ws)
        .await
        .expect("board init");
    session
}

async fn add_card(
    conn: &mut PgConnection,
    session: &str,
    lane: &str,
    title: &str,
    priority: i16,
) -> i32 {
    let mut args = CardAdd::new(lane, title);
    args.priority = priority;
    write::card_add(conn, session, args)
        .await
        .expect("card add")
        .card
        .unwrap()
}

async fn snapshot(conn: &mut PgConnection, session: &str) -> read::BoardSnapshot {
    let board = read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap();
    read::board_snapshot(conn, board.id).await.unwrap().unwrap()
}

async fn detail(conn: &mut PgConnection, session: &str, seq: i32) -> read::CardDetail {
    let board = read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap();
    read::card_detail(conn, board.id, seq)
        .await
        .unwrap()
        .unwrap()
}

fn lane_names(snap: &read::BoardSnapshot) -> Vec<(String, LaneKind)> {
    snap.lanes
        .iter()
        .map(|l| (l.lane.name.clone(), l.lane.kind))
        .collect()
}

fn titles_in(snap: &read::BoardSnapshot, lane: &str) -> Vec<String> {
    snap.lanes
        .iter()
        .find(|l| l.lane.name == lane)
        .unwrap()
        .cards
        .iter()
        .map(|c| c.card.title.clone())
        .collect()
}

// --- board init (docs/cli.md 4.3) ----------------------------------------------------------

#[tokio::test]
async fn board_init_new_board_gets_default_lanes() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(
        lane_names(&snap),
        vec![
            ("待辦".to_string(), LaneKind::Todo),
            ("進行中".to_string(), LaneKind::Doing),
            ("完成".to_string(), LaneKind::Done)
        ]
    );
    assert_eq!(snap.board.title, "測試看板");
    assert_eq!(snap.board.goal, "目標");
    assert_eq!(snap.board.rev, 1);
    assert_eq!(snap.board.status, BoardStatus::Active);
    let activity = read::recent_activity(&mut conn, snap.board.id, 10)
        .await
        .unwrap();
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].verb, "board.create");
    assert_eq!(activity[0].detail["title"], "測試看板");
}

#[tokio::test]
async fn board_init_new_board_with_lanes_keeps_order() {
    let mut conn = common::writer().await;
    let session = new_board(
        &mut conn,
        Some("待辦:todo,定位:doing,實作:doing,驗證:doing,完成:done"),
    )
    .await;
    let snap = snapshot(&mut conn, &session).await;
    let names: Vec<String> = snap.lanes.iter().map(|l| l.lane.name.clone()).collect();
    assert_eq!(names, ["待辦", "定位", "實作", "驗證", "完成"]);
    let positions: Vec<i32> = snap.lanes.iter().map(|l| l.lane.position).collect();
    assert_eq!(positions, [1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn board_init_existing_board_with_lanes() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let ws_must_not_resolve =
        || -> WorkspaceTarget { panic!("workspace resolved for an existing board") };

    // Has lanes, no --lanes: title updated, goal kept when not given.
    let args = BoardInit {
        title: Some("新標題".into()),
        ..Default::default()
    };
    let out = write::board_init(&mut conn, &session, args, ws_must_not_resolve)
        .await
        .unwrap();
    assert_eq!(out.rev, 2);
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.title, "新標題");
    assert_eq!(snap.board.goal, "目標");
    assert_eq!(snap.lanes.len(), 3);
    let activity = read::recent_activity(&mut conn, snap.board.id, 1)
        .await
        .unwrap();
    assert_eq!(activity[0].verb, "board.update");

    // Has lanes and --lanes: rejected, nothing changes.
    let args = BoardInit {
        title: Some("再改".into()),
        lanes: Some(write::parse_lanes("甲:todo,乙:done").unwrap()),
        ..Default::default()
    };
    let err = write::board_init(&mut conn, &session, args, ws_must_not_resolve)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "board already has lanes: use \"kb lane\" commands"
    );
    assert_eq!(err.exit_code(), 1);
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.title, "新標題");
    assert_eq!(snap.board.rev, 2);
}

#[tokio::test]
async fn board_init_existing_board_without_lanes() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("甲:todo,乙:done")).await;
    write::lane_remove(&mut conn, &session, "甲").await.unwrap();
    write::lane_remove(&mut conn, &session, "乙").await.unwrap();
    assert!(snapshot(&mut conn, &session).await.lanes.is_empty());

    // No lanes, --lanes given: lanes created.
    let args = BoardInit {
        title: Some("有泳道了".into()),
        goal: Some("新目標".into()),
        lanes: Some(write::parse_lanes("待辦:todo,完成:done").unwrap()),
        epics: None,
        cwd: None,
    };
    write::board_init(&mut conn, &session, args, || panic!("no resolve"))
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.title, "有泳道了");
    assert_eq!(snap.board.goal, "新目標");
    assert_eq!(lane_names(&snap).len(), 2);

    // No lanes, no --lanes: default lanes.
    write::lane_remove(&mut conn, &session, "待辦")
        .await
        .unwrap();
    write::lane_remove(&mut conn, &session, "完成")
        .await
        .unwrap();
    let args = BoardInit {
        title: Some("預設".into()),
        ..Default::default()
    };
    write::board_init(&mut conn, &session, args, || panic!("no resolve"))
        .await
        .unwrap();
    let names: Vec<String> = snapshot(&mut conn, &session)
        .await
        .lanes
        .iter()
        .map(|l| l.lane.name.clone())
        .collect();
    assert_eq!(names, ["待辦", "進行中", "完成"]);
}

/// docs/board-v2.md sections 20.1 and 20.2: `--title` is optional when the board has a
/// title, and `board.update` records a changed goal.
#[tokio::test]
async fn board_init_without_title_keeps_it_and_records_the_goal() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let no_resolve = || -> WorkspaceTarget { panic!("workspace resolved for an existing board") };

    // Only the goal: the title is kept, the goal is in the activity detail.
    let args = BoardInit {
        goal: Some("新的目標".into()),
        ..Default::default()
    };
    let out = write::board_init(&mut conn, &session, args, no_resolve)
        .await
        .unwrap();
    assert_eq!(out.message, "board updated");
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.title, "測試看板");
    assert_eq!(snap.board.goal, "新的目標");
    let activity = read::recent_activity(&mut conn, snap.board.id, 1)
        .await
        .unwrap();
    assert_eq!(activity[0].verb, "board.update");
    assert_eq!(
        activity[0].detail,
        json!({ "title": "測試看板", "goal": "新的目標" })
    );

    // The same goal again, or only a title: no "goal" in the detail.
    let args = BoardInit {
        title: Some("改過的標題".into()),
        goal: Some("新的目標".into()),
        ..Default::default()
    };
    write::board_init(&mut conn, &session, args, no_resolve)
        .await
        .unwrap();
    let activity = read::recent_activity(&mut conn, snap.board.id, 1)
        .await
        .unwrap();
    assert_eq!(activity[0].detail, json!({ "title": "改過的標題" }));

    // An empty title is still rejected; nothing changes.
    let args = BoardInit {
        title: Some("  ".into()),
        goal: Some("不會寫入".into()),
        ..Default::default()
    };
    let err = write::board_init(&mut conn, &session, args, no_resolve)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "title must not be empty");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(snapshot(&mut conn, &session).await.board.goal, "新的目標");
}

#[tokio::test]
async fn board_init_without_title_needs_a_title_to_keep() {
    let mut conn = common::writer().await;

    // No board: refused before the workspace is resolved; no board is created.
    let session = common::unique("notitle");
    let args = BoardInit {
        goal: Some("目標".into()),
        ..Default::default()
    };
    let err = write::board_init(&mut conn, &session, args, || {
        panic!("workspace resolved without a title")
    })
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "--title is required: the board has no title yet"
    );
    assert_eq!(err.exit_code(), 1);
    assert!(
        read::board_by_session(&mut conn, &session)
            .await
            .unwrap()
            .is_none()
    );

    // A board with an empty title (kb board start without --title): refused, unchanged.
    write::board_start(
        &mut conn,
        &session,
        write::BoardStartArgs::default(),
        None,
        || manual_ws(&common::unique("ws")),
    )
    .await
    .unwrap();
    let before = read::board_by_session(&mut conn, &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.title, "");
    let args = BoardInit {
        goal: Some("目標".into()),
        ..Default::default()
    };
    let err = write::board_init(&mut conn, &session, args, || panic!("no resolve"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), write::TITLE_REQUIRED);
    let after = read::board_by_session(&mut conn, &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.rev, after.goal.as_str()),
        (before.rev, before.goal.as_str())
    );

    // With a title it works, and later calls may leave it out.
    let args = BoardInit {
        title: Some("有標題了".into()),
        ..Default::default()
    };
    write::board_init(&mut conn, &session, args, || panic!("no resolve"))
        .await
        .unwrap();
    let args = BoardInit {
        goal: Some("目標".into()),
        ..Default::default()
    };
    write::board_init(&mut conn, &session, args, || panic!("no resolve"))
        .await
        .unwrap();
    let b = read::board_by_session(&mut conn, &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((b.title.as_str(), b.goal.as_str()), ("有標題了", "目標"));
}

#[test]
fn parse_lanes_validates() {
    assert!(write::parse_lanes("待辦:todo,完成:done").is_ok());
    let missing_done = write::parse_lanes("待辦:todo,實作:doing").unwrap_err();
    assert!(
        missing_done
            .to_string()
            .contains("at least one todo lane and one done lane")
    );
    assert!(
        write::parse_lanes("待辦:todo,完成:finished")
            .unwrap_err()
            .to_string()
            .starts_with("invalid lane kind: finished")
    );
    assert!(
        write::parse_lanes("待辦:todo,待辦:done")
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(write::parse_lanes("待辦,完成:done").is_err());
}

// --- cards (docs/cli.md 4.10-4.16) --------------------------------------------------------

#[tokio::test]
async fn card_move_sets_and_clears_closed_at() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("待辦:todo,實作:doing,完成:done")).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片", 2).await;
    assert!(
        detail(&mut conn, &session, seq)
            .await
            .card
            .closed_at
            .is_none()
    );

    write::card_move(&mut conn, &session, seq, "完成", None)
        .await
        .unwrap();
    assert!(
        detail(&mut conn, &session, seq)
            .await
            .card
            .closed_at
            .is_some()
    );

    let out = write::card_move(&mut conn, &session, seq, "實作", Some("重新開始".into()))
        .await
        .unwrap();
    let d = detail(&mut conn, &session, seq).await;
    assert!(d.card.closed_at.is_none());
    assert_eq!(d.lane.name, "實作");
    assert_eq!(d.notes.len(), 1);
    assert_eq!(d.notes[0].kind, NoteKind::Progress);
    assert_eq!(d.notes[0].body, "重新開始");
    let activity = read::recent_activity(&mut conn, out.board_id, 2)
        .await
        .unwrap();
    assert_eq!(activity[1].verb, "card.move");
    assert_eq!(activity[1].detail["from"], "完成");
    assert_eq!(activity[1].detail["to"], "實作");
    assert_eq!(activity[0].verb, "card.note");

    let err = write::card_move(&mut conn, &session, seq, "實作", None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "card is already in 實作");
    let err = write::card_move(&mut conn, &session, seq, "沒有", None)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "lane not found: 沒有 (lanes: 待辦, 實作, 完成)"
    );
    let err = write::card_move(&mut conn, &session, 999, "實作", None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "card not found: 999");
}

#[tokio::test]
async fn card_block_unblock_and_done() {
    let mut conn = common::writer().await;
    // Two done lanes: done goes to the leftmost one.
    let session = new_board(&mut conn, Some("待辦:todo,完成:done,歸檔:done")).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片", 1).await;

    write::card_block(&mut conn, &session, seq, "等待確認")
        .await
        .unwrap();
    let d = detail(&mut conn, &session, seq).await;
    assert_eq!(d.card.blocked_reason.as_deref(), Some("等待確認"));
    assert_eq!(d.lane.name, "待辦");

    write::card_unblock(&mut conn, &session, seq).await.unwrap();
    assert!(
        detail(&mut conn, &session, seq)
            .await
            .card
            .blocked_reason
            .is_none()
    );
    let err = write::card_unblock(&mut conn, &session, seq)
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("card is not blocked"));

    write::card_block(&mut conn, &session, seq, "再次阻擋")
        .await
        .unwrap();
    write::card_done(&mut conn, &session, seq, "修好了")
        .await
        .unwrap();
    let d = detail(&mut conn, &session, seq).await;
    assert_eq!(d.lane.name, "完成");
    assert_eq!(d.card.result.as_deref(), Some("修好了"));
    assert!(d.card.closed_at.is_some());
    assert!(d.card.blocked_reason.is_none());
}

#[tokio::test]
async fn card_drop_creates_dropped_lane_once() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("待辦:todo,完成:done")).await;
    let a = add_card(&mut conn, &session, "待辦", "甲", 2).await;
    let b = add_card(&mut conn, &session, "待辦", "乙", 2).await;
    write::card_block(&mut conn, &session, a, "卡住")
        .await
        .unwrap();

    write::card_drop(&mut conn, &session, a, "不需要了")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    let last = snap.lanes.last().unwrap();
    assert_eq!(
        (last.lane.name.as_str(), last.lane.kind, last.lane.position),
        ("捨棄", LaneKind::Dropped, 3)
    );
    let d = detail(&mut conn, &session, a).await;
    assert_eq!(d.lane.name, "捨棄");
    assert_eq!(d.card.result.as_deref(), Some("不需要了"));
    assert!(d.card.closed_at.is_some());
    assert!(d.card.blocked_reason.is_none());
    let activity = read::recent_activity(&mut conn, snap.board.id, 2)
        .await
        .unwrap();
    assert_eq!(activity[0].verb, "card.drop");
    assert_eq!(activity[1].verb, "lane.add");

    write::card_drop(&mut conn, &session, b, "也不需要")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.lanes.len(), 3);
    // The card dropped last comes first (docs/board-v2.md section 16.3).
    assert_eq!(titles_in(&snap, "捨棄"), ["乙", "甲"]);
}

#[tokio::test]
async fn finished_lanes_list_the_latest_entry_first() {
    // docs/board-v2.md section 16.3.
    let mut conn = common::writer().await;
    let session = new_board(
        &mut conn,
        Some("待辦:todo,實作:doing,完成:done,封存:done,捨棄:dropped"),
    )
    .await;
    for title in ["甲", "乙", "丙", "丁", "戊"] {
        add_card(&mut conn, &session, "待辦", title, 2).await;
    }
    for seq in [1, 2, 3] {
        write::card_done(&mut conn, &session, seq, "好了")
            .await
            .unwrap();
    }
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "完成"), ["丙", "乙", "甲"]);

    // A card that leaves and re-enters counts from its return.
    write::card_move(&mut conn, &session, 1, "實作", None)
        .await
        .unwrap();
    assert!(
        detail(&mut conn, &session, 1)
            .await
            .card
            .closed_at
            .is_none()
    );
    write::card_done(&mut conn, &session, 1, "再修一次")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "完成"), ["甲", "丙", "乙"]);

    // Done again in place: the card did not re-enter; closed_at and the order stay.
    let before = detail(&mut conn, &session, 2).await.card.closed_at;
    write::card_done(&mut conn, &session, 2, "結果改寫")
        .await
        .unwrap();
    let d = detail(&mut conn, &session, 2).await;
    assert_eq!(d.card.closed_at, before);
    assert_eq!(d.card.result.as_deref(), Some("結果改寫"));
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "完成"), ["甲", "丙", "乙"]);

    // Moving between two done lanes is an entry; the position does not order these lanes.
    write::card_move(&mut conn, &session, 3, "封存", None)
        .await
        .unwrap();
    write::card_move_to(&mut conn, &session, 2, "封存", None, true)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "封存"), ["乙", "丙"]);
    write::card_next(&mut conn, &session, 3).await.unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(
        titles_in(&snap, "封存"),
        ["乙", "丙"],
        "card next does not reorder"
    );

    // Dropped lanes follow the same rule; the todo lane keeps its position order.
    write::card_drop(&mut conn, &session, 4, "不需要")
        .await
        .unwrap();
    write::card_drop(&mut conn, &session, 1, "改成捨棄")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "捨棄"), ["甲", "丁"]);
    assert_eq!(titles_in(&snap, "待辦"), ["戊"]);
    assert_eq!(snap.next_card().map(|(_, c)| c.card.seq), Some(5));
}

#[tokio::test]
async fn cards_sort_by_position_only() {
    // docs/board-v2.md section 10.2: priority no longer sorts.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("待辦:todo,實作:doing,完成:done")).await;
    add_card(&mut conn, &session, "待辦", "a-P2", 2).await;
    add_card(&mut conn, &session, "待辦", "b-P0", 0).await;
    add_card(&mut conn, &session, "待辦", "c-P2", 2).await;
    add_card(&mut conn, &session, "待辦", "d-P1", 1).await;
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦"), ["a-P2", "b-P0", "c-P2", "d-P1"]);

    // A card entering a lane goes last; changing its priority does not move it.
    let x = add_card(&mut conn, &session, "實作", "x-P2", 2).await;
    add_card(&mut conn, &session, "實作", "y-P2", 2).await;
    write::card_move(&mut conn, &session, x, "待辦", None)
        .await
        .unwrap();
    write::card_move(&mut conn, &session, x, "實作", None)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "實作"), ["y-P2", "x-P2"]);
    let edit = write::CardEdit {
        priority: Some(0),
        ..Default::default()
    };
    write::card_edit(&mut conn, &session, x, edit)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "實作"), ["y-P2", "x-P2"]);
    assert_eq!(
        snap.card(x).unwrap().1.card.priority,
        0,
        "still an attribute"
    );
}

#[tokio::test]
async fn blocked_cards_sort_last_in_todo_lanes_until_unblocked() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("待辦:todo,實作:doing,完成:done")).await;
    let a = add_card(&mut conn, &session, "待辦", "a", 2).await;
    let b = add_card(&mut conn, &session, "待辦", "b", 2).await;
    let c = add_card(&mut conn, &session, "待辦", "c", 2).await;
    let blocker = add_card(&mut conn, &session, "實作", "blocker", 2).await;
    // A blocked reason.
    write::card_block(&mut conn, &session, a, "等待")
        .await
        .unwrap();
    // An unfinished blocker through a `blocks` link.
    write::link_add(
        &mut conn,
        &session,
        blocker,
        LinkKind::Blocks,
        write::LinkTarget::Card(b),
        None,
    )
    .await
    .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(
        titles_in(&snap, "待辦"),
        ["c", "a", "b"],
        "blocked last, by position"
    );
    let (lane, next) = snap.next_card().unwrap();
    assert_eq!((lane.name.as_str(), next.card.seq), ("待辦", c));
    // Positions are not rewritten.
    let positions: Vec<i32> = snap.lanes[0]
        .cards
        .iter()
        .map(|x| x.card.position)
        .collect();
    assert_eq!(positions, [3, 1, 2]);

    // Unblocked, a goes back to its place; when the blocker finishes, so does b.
    write::card_unblock(&mut conn, &session, a).await.unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦"), ["a", "c", "b"]);
    write::card_done(&mut conn, &session, blocker, "好了")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦"), ["a", "b", "c"]);

    // Only todo lanes: a blocked card in a doing lane keeps its position.
    let d = add_card(&mut conn, &session, "實作", "d", 2).await;
    add_card(&mut conn, &session, "實作", "e", 2).await;
    write::card_block(&mut conn, &session, d, "等待")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "實作"), ["d", "e"]);

    // Every todo card blocked: no card is next, though the lane has cards.
    for x in [a, b, c] {
        write::card_block(&mut conn, &session, x, "等待")
            .await
            .unwrap();
    }
    let snap = snapshot(&mut conn, &session).await;
    assert!(snap.next_card().is_none());
    assert_eq!(snap.todo_card_count(), 3);
}

/// Adds a card with origin `discussing`.
async fn add_discussing(conn: &mut PgConnection, session: &str, lane: &str, title: &str) -> i32 {
    let mut args = CardAdd::new(lane, title);
    args.origin = Origin::Discussing;
    write::card_add(conn, session, args)
        .await
        .expect("card add")
        .card
        .unwrap()
}

fn next_seq(snap: &read::BoardSnapshot) -> Option<(String, i32)> {
    snap.next_card().map(|(l, c)| (l.name.clone(), c.card.seq))
}

#[tokio::test]
async fn the_head_is_the_first_card_neither_blocked_nor_discussing() {
    // docs/board-v2.md section 10.2.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("研究:todo,待辦:todo,實作:doing,完成:done")).await;

    // A discussing card alone: the lanes have a card, but none is ready.
    let talk = add_discussing(&mut conn, &session, "研究", "討論中").await;
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(next_seq(&snap), None);
    assert_eq!(snap.todo_card_count(), 1);

    // Discussing first, then blocked, then ready, in the next todo lane: the ready one.
    let blocked = add_card(&mut conn, &session, "待辦", "被阻擋", 2).await;
    write::card_block(&mut conn, &session, blocked, "等待")
        .await
        .unwrap();
    let talk2 = add_discussing(&mut conn, &session, "待辦", "也在討論").await;
    let ready = add_card(&mut conn, &session, "待辦", "可以做", 2).await;
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(next_seq(&snap), Some(("待辦".to_string(), ready)));
    // The display order does not change: only blocked cards sort last.
    assert_eq!(titles_in(&snap, "待辦"), ["也在討論", "可以做", "被阻擋"]);

    // Ready first, discussing after it: still the ready one.
    write::card_next(&mut conn, &session, ready).await.unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(next_seq(&snap), Some(("待辦".to_string(), ready)));

    // Once confirmed, the discussing card of the leftmost todo lane is the head.
    write::card_set_origin(&mut conn, &session, &[talk], Origin::Agreed)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(next_seq(&snap), Some(("研究".to_string(), talk)));

    // A discussing card moved to the front does not take the head.
    write::card_move(&mut conn, &session, talk, "實作", None)
        .await
        .unwrap();
    write::card_next(&mut conn, &session, talk2).await.unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦")[0], "也在討論");
    assert_eq!(next_seq(&snap), Some(("待辦".to_string(), ready)));

    // Nothing ready: the ready card leaves, a blocked and a discussing card stay.
    write::card_move(&mut conn, &session, ready, "實作", None)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(next_seq(&snap), None);
    assert_eq!(snap.todo_card_count(), 2);
}

#[tokio::test]
async fn next_order_and_the_next_flag_reorder_a_lane() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("待辦:todo,實作:doing,完成:done")).await;
    let a = add_card(&mut conn, &session, "待辦", "a", 2).await;
    let b = add_card(&mut conn, &session, "待辦", "b", 2).await;
    let c = add_card(&mut conn, &session, "待辦", "c", 2).await;
    let d = add_card(&mut conn, &session, "待辦", "d", 2).await;
    let x = add_card(&mut conn, &session, "實作", "x", 2).await;

    // card next: to the front; activity card.order with one card.
    let out = write::card_next(&mut conn, &session, c).await.unwrap();
    assert_eq!(out.message, format!("card {c} moved to the front of 待辦"));
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦"), ["c", "a", "b", "d"]);
    let act = read::recent_activity(&mut conn, snap.board.id, 1)
        .await
        .unwrap();
    assert_eq!(act[0].verb, "card.order");
    assert_eq!(act[0].detail, json!({"lane": "待辦", "seqs": [c]}));

    // card order: the listed cards first, the others keep their relative order.
    let out = write::card_order(&mut conn, &session, &[d, b])
        .await
        .unwrap();
    assert_eq!(
        out.message,
        format!("cards {d}, {b} moved to the front of 待辦")
    );
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦"), ["d", "b", "c", "a"]);
    let act = read::recent_activity(&mut conn, snap.board.id, 1)
        .await
        .unwrap();
    assert_eq!(act[0].detail, json!({"lane": "待辦", "seqs": [d, b]}));

    // No change: success, nothing written (same rev, no activity).
    let rev = snap.board.rev;
    let out = write::card_order(&mut conn, &session, &[d, b, c])
        .await
        .unwrap();
    assert_eq!(out.message, "order of 待辦 unchanged");
    write::card_next(&mut conn, &session, d).await.unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.rev, rev);
    let act = read::recent_activity(&mut conn, snap.board.id, 1)
        .await
        .unwrap();
    assert_eq!(act[0].detail, json!({"lane": "待辦", "seqs": [d, b]}));

    // Cards in different lanes: refused.
    let err = write::card_order(&mut conn, &session, &[a, x])
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "cards are not in the same lane");
    assert_eq!(err.exit_code(), 1);

    // --next on add and move.
    let mut args = write::CardAdd::new("待辦", "first");
    args.next = true;
    let f = write::card_add(&mut conn, &session, args)
        .await
        .unwrap()
        .card
        .unwrap();
    write::card_move_to(&mut conn, &session, x, "待辦", None, true)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(titles_in(&snap, "待辦"), ["x", "first", "d", "b", "c", "a"]);
    let _ = f;
    // Without --next, last as before.
    let y = add_card(&mut conn, &session, "待辦", "y", 2).await;
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(
        titles_in(&snap, "待辦").last().map(String::as_str),
        Some("y")
    );
    let _ = y;
}
#[tokio::test]
async fn card_edit_requires_a_field() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let seq = add_card(&mut conn, &session, "待辦", "原標題", 2).await;
    let err = write::card_edit(&mut conn, &session, seq, Default::default())
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("nothing to edit"));
    let edit = write::CardEdit {
        title: Some("新標題".into()),
        owner: Some("tester".into()),
        ..Default::default()
    };
    write::card_edit(&mut conn, &session, seq, edit)
        .await
        .unwrap();
    let d = detail(&mut conn, &session, seq).await;
    assert_eq!(
        (d.card.title.as_str(), d.card.owner.as_str()),
        ("新標題", "tester")
    );
    let board = read::board_by_session(&mut conn, &session)
        .await
        .unwrap()
        .unwrap();
    let act = read::recent_activity(&mut conn, board.id, 1).await.unwrap();
    assert_eq!(
        act[0].detail["fields"],
        serde_json::json!(["title", "owner"])
    );
}

#[tokio::test]
async fn card_note_records_excerpt() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片", 2).await;
    let long: String = "長".repeat(100);
    write::card_note(&mut conn, &session, seq, NoteKind::Decision, &long)
        .await
        .unwrap();
    let d = detail(&mut conn, &session, seq).await;
    assert_eq!(d.notes[0].body, long);
    let act = read::recent_activity(&mut conn, d.card.board_id, 1)
        .await
        .unwrap();
    assert_eq!(act[0].verb, "card.note");
    assert_eq!(act[0].detail["kind"], "decision");
    assert_eq!(
        act[0].detail["excerpt"].as_str().unwrap().chars().count(),
        80
    );
}

// --- lanes (docs/cli.md 4.9) ---------------------------------------------------------------

#[tokio::test]
async fn lane_add_rename_move_remove() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, Some("待辦:todo,實作:doing,完成:done")).await;
    write::lane_add(
        &mut conn,
        &session,
        "驗證",
        LaneKind::Doing,
        Some("實作"),
        Some(2),
    )
    .await
    .unwrap();
    write::lane_add(&mut conn, &session, "最後", LaneKind::Todo, None, None)
        .await
        .unwrap();
    let names = |s: &read::BoardSnapshot| {
        s.lanes
            .iter()
            .map(|l| l.lane.name.clone())
            .collect::<Vec<_>>()
    };
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(names(&snap), ["待辦", "實作", "驗證", "完成", "最後"]);
    assert_eq!(snap.lanes[2].lane.wip_limit, Some(2));

    let err = write::lane_add(&mut conn, &session, "驗證", LaneKind::Doing, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "lane already exists: 驗證");

    write::lane_rename(&mut conn, &session, "驗證", "測試")
        .await
        .unwrap();
    write::lane_move(&mut conn, &session, "最後", 1)
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(names(&snap), ["最後", "待辦", "實作", "測試", "完成"]);
    assert_eq!(
        snap.lanes
            .iter()
            .map(|l| l.lane.position)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert!(
        write::lane_move(&mut conn, &session, "最後", 6)
            .await
            .is_err()
    );

    add_card(&mut conn, &session, "測試", "卡片", 2).await;
    let err = write::lane_remove(&mut conn, &session, "測試")
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("lane is not empty"));
    write::lane_remove(&mut conn, &session, "最後")
        .await
        .unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(names(&snap), ["待辦", "實作", "測試", "完成"]);
    assert_eq!(
        snap.lanes
            .iter()
            .map(|l| l.lane.position)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
}

#[tokio::test]
async fn wip_limit_exceeded_is_a_warning() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    write::lane_add(&mut conn, &session, "窄", LaneKind::Doing, None, Some(1))
        .await
        .unwrap();
    let a = add_card(&mut conn, &session, "待辦", "甲", 2).await;
    let b = add_card(&mut conn, &session, "待辦", "乙", 2).await;
    assert!(
        write::card_move(&mut conn, &session, a, "窄", None)
            .await
            .unwrap()
            .warnings
            .is_empty()
    );
    let out = write::card_move(&mut conn, &session, b, "窄", None)
        .await
        .unwrap();
    assert_eq!(out.warnings, ["lane 窄 is over its WIP limit (2/1)"]);
}

// --- links (docs/cli.md 4.10, 4.18) ---------------------------------------------------------

#[tokio::test]
async fn links_spawn_and_errors() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let parent = add_card(&mut conn, &session, "待辦", "父卡", 1).await;
    let other = add_card(&mut conn, &session, "待辦", "其他", 2).await;

    let mut args = CardAdd::new("待辦", "子卡");
    args.from = Some(parent);
    let out = write::card_add(&mut conn, &session, args).await.unwrap();
    let child = out.card.unwrap();
    assert!(out.link.is_some());
    let act = read::recent_activity(&mut conn, out.board_id, 2)
        .await
        .unwrap();
    assert_eq!(act[0].verb, "link.add");
    assert_eq!(
        act[0].detail,
        serde_json::json!({"seq": parent, "kind": "spawned", "to_seq": child})
    );
    assert_eq!(act[1].verb, "card.add");

    let mut bad = CardAdd::new("待辦", "孤兒");
    bad.from = Some(999);
    assert_eq!(
        write::card_add(&mut conn, &session, bad)
            .await
            .unwrap_err()
            .to_string(),
        "card not found: 999"
    );

    write::link_add(
        &mut conn,
        &session,
        parent,
        LinkKind::Blocks,
        LinkTarget::Card(other),
        None,
    )
    .await
    .unwrap();
    write::link_add(
        &mut conn,
        &session,
        other,
        LinkKind::Relates,
        LinkTarget::Card(child),
        None,
    )
    .await
    .unwrap();
    let ref_out = write::link_add(
        &mut conn,
        &session,
        parent,
        LinkKind::Ref,
        LinkTarget::External("src/auth/token.rs:120".into()),
        Some("逾時設定".into()),
    )
    .await
    .unwrap();

    let err = write::link_add(
        &mut conn,
        &session,
        parent,
        LinkKind::Blocks,
        LinkTarget::Card(other),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "link already exists");
    let err = write::link_add(
        &mut conn,
        &session,
        parent,
        LinkKind::Ref,
        LinkTarget::External("src/auth/token.rs:120".into()),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "link already exists");
    let err = write::link_add(
        &mut conn,
        &session,
        parent,
        LinkKind::Relates,
        LinkTarget::Card(parent),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "cannot link a card to itself");
    assert!(
        write::link_add(
            &mut conn,
            &session,
            parent,
            LinkKind::Spawned,
            LinkTarget::Card(other),
            None
        )
        .await
        .is_err()
    );

    // Parent: spawned (out), blocks (out), ref (out).
    let d = detail(&mut conn, &session, parent).await;
    let summary: Vec<(LinkKind, LinkDirection, Option<i32>, Option<&str>)> = d
        .links
        .iter()
        .map(|l| {
            (
                l.kind,
                l.direction,
                l.card.as_ref().map(|c| c.seq),
                l.target.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                LinkKind::Spawned,
                LinkDirection::Outgoing,
                Some(child),
                None
            ),
            (LinkKind::Blocks, LinkDirection::Outgoing, Some(other), None),
            (
                LinkKind::Ref,
                LinkDirection::Outgoing,
                None,
                Some("src/auth/token.rs:120")
            ),
        ]
    );
    assert_eq!(d.links[2].label.as_deref(), Some("逾時設定"));
    // Child: spawned (in) from parent with its title, relates (in) from other.
    let d = detail(&mut conn, &session, child).await;
    assert_eq!(d.links[0].kind, LinkKind::Spawned);
    assert_eq!(d.links[0].direction, LinkDirection::Incoming);
    assert_eq!(d.links[0].card.as_ref().unwrap().title, "父卡");
    assert_eq!(d.links[1].direction, LinkDirection::Incoming);

    // Link counts on the board include both directions.
    let snap = snapshot(&mut conn, &session).await;
    let count = |seq| snap.card(seq).unwrap().1.link_count;
    assert_eq!((count(parent), count(other), count(child)), (3, 2, 2));

    write::link_remove(&mut conn, &session, ref_out.link.unwrap())
        .await
        .unwrap();
    assert_eq!(detail(&mut conn, &session, parent).await.links.len(), 2);
    let err = write::link_remove(&mut conn, &session, ref_out.link.unwrap())
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("link not found"));
}

// --- archive (docs/cli.md 2.9, 4.6, 4.7) ----------------------------------------------------

#[tokio::test]
async fn archived_board_rejects_writes() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn, None).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片", 2).await;

    let out = write::board_archive(&mut conn, &session, None, Some("摘要".into()))
        .await
        .unwrap();
    assert_eq!(out.message, "board archived (incomplete)");
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.status, BoardStatus::Archived);
    assert_eq!(snap.board.outcome, Some(Outcome::Incomplete));
    assert!(snap.board.archived_at.is_some());

    let expected = "board is archived: run \"kb board reopen\" first";
    let err = write::card_add(&mut conn, &session, CardAdd::new("待辦", "新卡"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::BoardArchived));
    assert_eq!(err.to_string(), expected);
    assert_eq!(err.exit_code(), 1);
    let err = write::card_note(&mut conn, &session, seq, NoteKind::Progress, "x")
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), expected);
    let err = write::lane_add(&mut conn, &session, "新", LaneKind::Todo, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), expected);
    let args = BoardInit {
        title: Some("x".into()),
        ..Default::default()
    };
    let err = write::board_init(&mut conn, &session, args, || panic!())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), expected);
    let err = write::board_archive(&mut conn, &session, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), expected);

    write::board_reopen(&mut conn, &session).await.unwrap();
    let snap = snapshot(&mut conn, &session).await;
    assert_eq!(snap.board.status, BoardStatus::Active);
    assert!(snap.board.outcome.is_none() && snap.board.archived_at.is_none());
    assert!(write::board_reopen(&mut conn, &session).await.is_err());

    write::card_done(&mut conn, &session, seq, "完成")
        .await
        .unwrap();
    let out = write::board_archive(&mut conn, &session, None, None)
        .await
        .unwrap();
    assert_eq!(out.message, "board archived (completed)");
    write::board_reopen(&mut conn, &session).await.unwrap();
    let out = write::board_archive(&mut conn, &session, Some(Outcome::Abandoned), None)
        .await
        .unwrap();
    assert_eq!(out.message, "board archived (abandoned)");
}

#[tokio::test]
async fn missing_board_is_reported() {
    let mut conn = common::writer().await;
    let session = common::unique("none");
    let err = write::card_add(&mut conn, &session, CardAdd::new("待辦", "x"))
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with(&format!("no board for session {session}"))
    );
    assert_eq!(err.exit_code(), 1);
}

// --- resolution (docs/cli.md 2.3, 2.4) ------------------------------------------------------

#[test]
fn session_key_resolution_order() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    };
    let all = env(&[
        ("KB_SESSION", "from-kb"),
        ("CLAUDE_CODE_SESSION_ID", "from-claude"),
    ]);
    assert_eq!(
        resolve::resolve_session_key(Some("from-flag"), all).unwrap(),
        "from-flag"
    );
    assert_eq!(resolve::resolve_session_key(None, all).unwrap(), "from-kb");
    let claude_only = env(&[("CLAUDE_CODE_SESSION_ID", "from-claude")]);
    assert_eq!(
        resolve::resolve_session_key(None, claude_only).unwrap(),
        "from-claude"
    );
    let empty_kb = env(&[
        ("KB_SESSION", ""),
        ("CLAUDE_CODE_SESSION_ID", "from-claude"),
    ]);
    assert_eq!(
        resolve::resolve_session_key(None, empty_kb).unwrap(),
        "from-claude"
    );
    let err = resolve::resolve_session_key(None, env(&[])).unwrap_err();
    assert_eq!(
        err.to_string(),
        "no session: set KB_SESSION or pass --session"
    );
    assert_eq!(err.exit_code(), 1);
}

#[test]
fn workspace_resolution_order() {
    let env = |k: &str| match k {
        "KB_TERMINAL_ID" => Some("t2b".to_string()),
        "KB_TERMINAL_SPACE" => Some("spc1".to_string()),
        "KB_TERMINAL_TAB" => Some("tb1".to_string()),
        "KB_WORKSPACE_NAME" => Some("Acme_DevOps".to_string()),
        _ => None,
    };
    let ctx = resolve::terminal_env(env).space.unwrap();
    assert_eq!(
        ctx,
        SpaceContext {
            space: "spc1".into(),
            name: "Acme_DevOps".into()
        }
    );

    // docs/cli.md section 2.4: the name is the last component of the folder's absolute
    // path on the machine where the session runs, so the folder must be absolute here.
    let folder = if cfg!(windows) {
        r"D:\Project\tui_kanban"
    } else {
        "/srv/project/tui_kanban"
    };
    let cwd = Path::new(folder);
    let manual = resolve::resolve_workspace(Some("手動"), Some(ctx.clone()), cwd);
    assert_eq!(
        (manual.key.as_str(), manual.name.as_str()),
        ("manual:手動", "手動")
    );
    let space = resolve::resolve_workspace(None, Some(ctx), cwd);
    assert_eq!(
        (space.key.as_str(), space.name.as_str()),
        ("space:spc1", "Acme_DevOps")
    );
    assert!(space.space.is_some());
    let dir = resolve::resolve_workspace(None, None, cwd);
    assert_eq!(
        (dir.key.as_str(), dir.name.as_str()),
        (format!("dir:{folder}").as_str(), "tui_kanban")
    );
}

#[test]
fn terminal_fields_without_a_terminal_space() {
    // A terminal id without KB_TERMINAL_SPACE: the workspace comes from the folder, and
    // the terminal is still attached to the new board, also for a manual workspace.
    let here = resolve::terminal_env(|k| (k == "KB_TERMINAL_ID").then(|| " t7 ".to_string()));
    let bare = resolve::TerminalInfo {
        terminal: "t7".into(),
        space: None,
        tab: None,
    };
    assert_eq!(here.terminal, Some(bare.clone()));
    assert_eq!(here.space, None);
    let cwd = std::env::temp_dir();
    let ws = resolve::workspace_here(None, here.clone(), &cwd);
    assert_eq!(ws.key, format!("dir:{}", cwd.to_string_lossy()));
    assert_eq!(ws.terminal, Some(bare.clone()));
    let manual = resolve::workspace_here(Some("手動"), here, &cwd);
    assert_eq!(manual.key, "manual:手動");
    assert_eq!(manual.terminal, Some(bare));
    assert_eq!(
        resolve::workspace_here(None, resolve::TerminalEnv::default(), &cwd).terminal,
        None
    );
}

#[tokio::test]
async fn new_board_stores_terminal_space_and_updates_workspace_name() {
    let mut conn = common::writer().await;
    let space = common::unique("space");
    let target = |name: &str| {
        let env = |k: &str| match k {
            "KB_TERMINAL_ID" => Some("t2b".to_string()),
            "KB_TERMINAL_SPACE" => Some(space.clone()),
            "KB_TERMINAL_TAB" => Some("tb1".to_string()),
            "KB_WORKSPACE_NAME" => Some(name.to_string()),
            _ => None,
        };
        resolve::workspace_here(None, resolve::terminal_env(env), Path::new("."))
    };
    let init = || BoardInit {
        title: Some("space".into()),
        ..Default::default()
    };

    let s1 = common::unique("space-board");
    let ws1 = target("舊名稱");
    write::board_init(&mut conn, &s1, init(), || ws1)
        .await
        .unwrap();
    let s2 = common::unique("space-board");
    let ws2 = target("新名稱");
    write::board_init(&mut conn, &s2, init(), || ws2)
        .await
        .unwrap();

    let b1 = read::board_by_session(&mut conn, &s1)
        .await
        .unwrap()
        .unwrap();
    let b2 = read::board_by_session(&mut conn, &s2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b1.workspace_id, b2.workspace_id);
    assert_eq!(b1.terminal_space.as_deref(), Some(space.as_str()));
    assert_eq!(b1.terminal_tab.as_deref(), Some("tb1"));
    assert_eq!(b1.terminal_id.as_deref(), Some("t2b"));
    let ws = read::workspace_by_key(&mut conn, &format!("space:{space}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ws.name, "新名稱");
    let list = read::list_boards(
        &mut conn,
        BoardQuery {
            workspace_id: Some(ws.id),
            include_archived: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].session_key, s2, "most recently changed first");
}

// --- reads as kb_reader, privileges ---------------------------------------------------------

#[tokio::test]
async fn reader_can_read_but_not_write() {
    let mut writer = common::writer().await;
    let session = new_board(&mut writer, Some("待辦:todo,實作:doing,完成:done")).await;
    let a = add_card(&mut writer, &session, "待辦", "甲", 1).await;
    add_card(&mut writer, &session, "實作", "乙", 2).await;
    write::card_note(&mut writer, &session, a, NoteKind::Progress, "紀錄")
        .await
        .unwrap();

    let mut reader = common::reader().await;
    let board = read::board_by_session(&mut reader, &session)
        .await
        .unwrap()
        .unwrap();
    let snap = read::board_snapshot(&mut reader, board.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.progress(), (0, 2));
    assert_eq!(snap.card(a).unwrap().1.note_count, 1);
    let summary = read::list_boards(
        &mut reader,
        BoardQuery {
            workspace_id: Some(board.workspace_id),
            include_archived: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        (
            summary[0].cards.todo,
            summary[0].cards.doing,
            summary[0].cards.done
        ),
        (1, 1, 0)
    );
    let workspaces = read::list_workspaces(&mut reader).await.unwrap();
    let ws = workspaces
        .iter()
        .find(|w| w.id == board.workspace_id)
        .unwrap();
    assert_eq!((ws.active_boards, ws.archived_boards), (1, 0));
    assert!(
        read::card_detail(&mut reader, board.id, a)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        !read::recent_activity(&mut reader, board.id, 3)
            .await
            .unwrap()
            .is_empty()
    );

    for sql in [
        "INSERT INTO workspace (key, name, source) VALUES ('manual:x', 'x', 'manual')",
        "UPDATE board SET title = 'hacked'",
        "DELETE FROM activity",
    ] {
        let err = sqlx::query(sql).execute(&mut reader).await.unwrap_err();
        let code = err
            .as_database_error()
            .and_then(|e| e.code())
            .map(|c| c.to_string());
        assert_eq!(code.as_deref(), Some("42501"), "{sql}: {err}");
    }
    // Writes through kb-core fail too.
    let err = write::card_note(&mut reader, &session, a, NoteKind::Progress, "x")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"), "{err}");
}

// --- notifications (docs/architecture.md 4) -------------------------------------------------

/// Collects this board's events until `quiet` passes without any.
async fn board_events(
    listener: &mut EventListener,
    board_id: i64,
    quiet: Duration,
) -> Vec<BoardEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = tokio::time::timeout(quiet, listener.next()).await {
        match ev.expect("listener") {
            ListenEvent::Board(e) if e.board == board_id => events.push(e),
            ListenEvent::Board(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    events
}

#[tokio::test]
async fn each_write_sends_exactly_one_notification() {
    let config = common::config();
    common::ensure_migrated().await;
    let mut listener = EventListener::connect(config.url(Role::Reader).unwrap())
        .await
        .unwrap();
    let mut conn = common::writer().await;
    let quiet = Duration::from_millis(400);

    // Board creation (insert + lanes + rev bump in one transaction): one event.
    let session = new_board(&mut conn, None).await;
    let board = read::board_by_session(&mut conn, &session)
        .await
        .unwrap()
        .unwrap();
    let events = board_events(&mut listener, board.id, quiet).await;
    assert_eq!(
        events,
        vec![BoardEvent {
            workspace: board.workspace_id,
            board: board.id,
            rev: 1
        }]
    );

    // Card add with --from (two activity rows, one transaction): one event, rev + 1.
    let seq = add_card(&mut conn, &session, "待辦", "卡片", 2).await;
    let mut args = CardAdd::new("待辦", "子卡");
    args.from = Some(seq);
    let _ = board_events(&mut listener, board.id, quiet).await;
    let out = write::card_add(&mut conn, &session, args).await.unwrap();
    let events = board_events(&mut listener, board.id, quiet).await;
    assert_eq!(
        events,
        vec![BoardEvent {
            workspace: board.workspace_id,
            board: board.id,
            rev: out.rev
        }]
    );
    assert_eq!(out.rev, 3);

    // A note: one event with the incremented rev.
    write::card_note(&mut conn, &session, seq, NoteKind::Progress, "紀錄")
        .await
        .unwrap();
    let events = board_events(&mut listener, board.id, quiet).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].rev, 4);

    // Archive changes status and rev in one transaction: one event.
    write::board_archive(&mut conn, &session, None, None)
        .await
        .unwrap();
    let events = board_events(&mut listener, board.id, quiet).await;
    assert_eq!(events.iter().map(|e| e.rev).collect::<Vec<_>>(), [5]);

    // A failed write changes nothing and sends nothing.
    assert!(
        write::card_note(&mut conn, &session, seq, NoteKind::Progress, "x")
            .await
            .is_err()
    );
    assert!(
        board_events(&mut listener, board.id, quiet)
            .await
            .is_empty()
    );
    assert_eq!(
        read::board_by_session(&mut conn, &session)
            .await
            .unwrap()
            .unwrap()
            .rev,
        5
    );
}
