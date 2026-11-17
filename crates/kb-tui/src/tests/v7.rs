//! Section 16 (docs/board-v2.md): the display-only sort toggle `o` (16.4) and the feed line
//! of a rule failure recorded after the turn's one block (16.1).

use kb_core::model::LaneKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use serde_json::json;

use super::*;
use crate::app::{EpicKey, SortMode, epic_cards};
use crate::run::render_text;
use crate::text::{activity_text, width};
use crate::view;

const SIZE: (u16, u16) = (120, 30);

fn sorted_card(seq: i32, title: &str, updated: i64, closed: Option<i64>) -> CardSummary {
    let mut c = card(seq, 0, title, 2);
    c.card.updated_at = at(updated);
    c.card.closed_at = closed.map(at);
    c
}

/// The read API's order: 待辦 by position with the blocked #1 last, 完成 and 捨棄 by
/// entry time, newest first. #2, #4 and #7 belong to epic 1.
fn sort_snapshot() -> BoardSnapshot {
    let mut c1 = sorted_card(1, "被擋住的卡片", 50, None);
    c1.card.blocked_reason = Some("等待回答".into());
    let in_epic = |mut c: CardSummary| {
        c.card.epic_id = Some(1);
        c.epic_name = Some("P1 看板".into());
        c
    };
    let mut snap = snapshot(
        "",
        vec![
            (
                lane(1, 1, "待辦", LaneKind::Todo),
                vec![
                    in_epic(sorted_card(2, "第二張", 5, None)),
                    sorted_card(5, "第五張", 40, None),
                    c1,
                ],
            ),
            (
                lane(2, 2, "實作", LaneKind::Doing),
                vec![sorted_card(3, "實作中", 10, None)],
            ),
            (
                lane(3, 3, "完成", LaneKind::Done),
                vec![
                    sorted_card(6, "最近完成", 31, Some(30)),
                    in_epic(sorted_card(7, "稍早完成", 21, Some(20))),
                    in_epic(sorted_card(4, "最早完成", 60, Some(10))),
                ],
            ),
            (
                lane(4, 4, "捨棄", LaneKind::Dropped),
                vec![sorted_card(8, "不做了", 2, Some(1))],
            ),
        ],
    );
    snap.epics = vec![epic(1, "P1 看板", None, counts(3, 2, 0))];
    snap.no_epic = Some(counts(5, 1, 0));
    snap
}

/// Card numbers per lane, left to right.
fn order(app: &App) -> Vec<Vec<i32>> {
    app.lanes()
        .iter()
        .map(|l| l.cards.iter().map(|c| c.card.seq).collect())
        .collect()
}

fn reload(app: &mut App, snap: BoardSnapshot) {
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
}

const DEFAULT: [&[i32]; 4] = [&[2, 5, 1], &[3], &[6, 7, 4], &[8]];
const UPDATED: [&[i32]; 4] = [&[1, 5, 2], &[3], &[4, 6, 7], &[8]];
const NUMBER: [&[i32]; 4] = [&[1, 2, 5], &[3], &[4, 6, 7], &[8]];

#[test]
fn o_cycles_the_display_order_and_keeps_the_selection() {
    let mut app = app_with(sort_snapshot(), SIZE);
    assert_eq!(app.sort, SortMode::Default);
    assert_eq!(order(&app), DEFAULT);
    // Select #7 in 完成.
    press(&mut app, &[Key::Right, Key::Right, Key::Down]);
    assert_eq!(selected_seq(&app), Some(7));

    // 最後異動: updated_at, newest first, in every lane; the blocked card does not sink.
    let fx = press(&mut app, &[Key::Char('o')]);
    assert!(fx.is_empty(), "display only: no load, no write: {fx:?}");
    assert_eq!(app.sort, SortMode::Updated);
    assert_eq!(order(&app), UPDATED);
    assert_eq!(selected_seq(&app), Some(7));
    assert_eq!(app.sel_lane, 2);

    // 編號: ascending in every lane.
    assert!(press(&mut app, &[Key::Char('o')]).is_empty());
    assert_eq!(app.sort, SortMode::Number);
    assert_eq!(order(&app), NUMBER);
    assert_eq!(selected_seq(&app), Some(7));

    // Back to 預設: the read API's order again.
    assert!(press(&mut app, &[Key::Char('o')]).is_empty());
    assert_eq!(app.sort, SortMode::Default);
    assert_eq!(order(&app), DEFAULT);
    assert_eq!(selected_seq(&app), Some(7));
}

#[test]
fn the_display_order_survives_reloads_but_not_a_restart() {
    let mut app = app_with(sort_snapshot(), SIZE);
    press(
        &mut app,
        &[Key::Right, Key::Right, Key::Down, Key::Char('o')],
    );
    assert_eq!(selected_seq(&app), Some(7));
    // A reload brings the read API's order; the chosen order is applied again.
    reload(&mut app, sort_snapshot());
    assert_eq!(order(&app), UPDATED);
    assert_eq!(selected_seq(&app), Some(7));
    // Moving in the lane follows the shown order.
    press(&mut app, &[Key::Up]);
    assert_eq!(selected_seq(&app), Some(6));
    // A new TUI starts with the default.
    assert_eq!(
        App::new(SIZE, today(), Scope::All, None).sort,
        SortMode::Default
    );
}

#[test]
fn o_works_with_the_side_panel_focused() {
    let mut app = app_with(sort_snapshot(), SIZE);
    press(&mut app, &[Key::Left]);
    assert!(app.side_focused());
    press(&mut app, &[Key::Char('o')]);
    assert_eq!(app.sort, SortMode::Updated);
    assert!(app.side_focused(), "the focus stays in the side panel");
}

fn buffer(app: &App, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    view::render(app, area, &mut buf);
    buf
}

/// The first cell of `text` on row `y`.
fn find(buf: &Buffer, y: u16, text: &str) -> Option<u16> {
    let want: Vec<String> = text.chars().map(|c| c.to_string()).collect();
    (0..buf.area.width).find(|&x| {
        let mut cx = x;
        for ch in &want {
            if cx >= buf.area.width || buf[(cx, y)].symbol() != ch {
                return false;
            }
            cx += width(ch).max(1) as u16;
        }
        true
    })
}

#[test]
fn a_non_default_order_is_named_in_the_header() {
    let mut app = app_with(sort_snapshot(), SIZE);
    let header = |app: &App| {
        render_text(app, 120, 30)
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    assert!(!header(&app).contains("排序"), "{}", header(&app));

    press(&mut app, &[Key::Char('o')]);
    let h = header(&app);
    assert!(h.contains("排序：最後異動  ● 已連線"), "{h}");
    let buf = buffer(&app, 120, 30);
    let x = find(&buf, 0, "排序：最後異動").expect("label on the header");
    assert!(buf[(x, 0)].modifier.contains(Modifier::REVERSED));

    press(&mut app, &[Key::Char('o')]);
    assert!(header(&app).contains("排序：編號"), "{}", header(&app));
    press(&mut app, &[Key::Char('o')]);
    assert!(!header(&app).contains("排序"));
}

#[test]
fn the_narrow_done_lane_shows_the_latest_card_first() {
    let app = app_with(sort_snapshot(), SIZE);
    let text = render_text(&app, 120, 30);
    let row = |seq: &str| {
        text.lines()
            .position(|l| l.contains(seq))
            .unwrap_or_else(|| panic!("{seq}: {text}"))
    };
    assert!(row("#6 ") < row("#7 ") && row("#7 ") < row("#4 "), "{text}");
}

#[test]
fn the_epic_screen_follows_the_display_order() {
    let mut app = app_with(sort_snapshot(), SIZE);
    let seqs = |app: &App| -> Vec<i32> {
        let snap = &app.data.as_ref().unwrap().snapshot;
        epic_cards(snap, EpicKey::Epic(1))
            .iter()
            .map(|c| c.card.seq)
            .collect()
    };
    assert_eq!(seqs(&app), [2, 7, 4]);
    press(&mut app, &[Key::Char('o'), Key::Char('o')]);
    assert_eq!(seqs(&app), [2, 4, 7]);
    // Open the epic screen from the side panel: the stats row names the order.
    press(&mut app, &[Key::Left, Key::Enter]);
    assert!(app.epic.is_some());
    let text = render_text(&app, 120, 30);
    assert!(text.contains("2/3 完成 · 排序：編號"), "{text}");
    let row = |s: &str| text.lines().position(|l| l.contains(s)).unwrap();
    assert!(row("#4 ") < row("#7 "), "{text}");
}

#[test]
fn the_hint_line_and_the_help_offer_o() {
    let mut app = app_with(sort_snapshot(), SIZE);
    let bar = |app: &App, w: u16| render_text(app, w, 30).lines().last().unwrap().to_string();
    assert!(bar(&app, 120).contains("e epic  o 排序  s 側欄"));
    for w in [60u16, 70, 80, 89, 100, 120] {
        app.update(Event::Resize(w, 30));
        let b = bar(&app, w);
        assert!(width(&b) <= w as usize, "{w}: {b}");
    }
    // The help screen lists `o` and still fits 80 × 24.
    let mut app = app_with(sort_snapshot(), (80, 24));
    press(&mut app, &[Key::Char('?')]);
    let text = render_text(&app, 80, 24);
    for s in ["切換排序（只影響畫面）", "回到泳道", "清除文字"] {
        assert!(text.contains(s), "{s}: {text}");
    }
}

#[test]
fn feed_line_of_a_failure_recorded_after_a_block() {
    let warn = activity(
        9,
        "check.warn",
        json!({"rule": "R2", "message": "泳道「驗證」超過上限（2/1）"}),
    );
    assert_eq!(
        activity_text(&warn),
        "守則 R2 警告：泳道「驗證」超過上限（2/1）"
    );
    let after = activity(
        9,
        "check.warn",
        json!({"rule": "R4", "message": "#3 完成但沒有結果", "level": "block", "after_block": true}),
    );
    assert_eq!(
        activity_text(&after),
        "守則 R4 未通過（本輪已阻擋過一次，只記錄）：#3 完成但沒有結果"
    );
    let block = activity(
        9,
        "check.block",
        json!({"rule": "R4", "message": "#3 完成但沒有結果"}),
    );
    assert_eq!(activity_text(&block), "守則 R4 阻擋：#3 完成但沒有結果");
}

#[test]
fn feed_lines_of_rules_r7_to_r10() {
    // docs/board-v2.md sections 17.3 and 17.4: the same line format, with the rules'
    // Traditional Chinese messages.
    let line = |verb: &str, detail: serde_json::Value| activity_text(&activity(9, verb, detail));
    assert_eq!(
        line(
            "check.warn",
            json!({"rule": "R7", "message": "#3 還在討論中，卻在進行中的泳道"})
        ),
        "守則 R7 警告：#3 還在討論中，卻在進行中的泳道"
    );
    assert_eq!(
        line(
            "check.warn",
            json!({"rule": "R8", "message": "#4 有未回答的提問 12，卻沒有標記阻擋"})
        ),
        "守則 R8 警告：#4 有未回答的提問 12，卻沒有標記阻擋"
    );
    assert_eq!(
        line(
            "check.block",
            json!({"rule": "R9", "message": "#5 收到提問 7 的回答，但沒有記下決定"})
        ),
        "守則 R9 阻擋：#5 收到提問 7 的回答，但沒有記下決定"
    );
    assert_eq!(
        line(
            "check.warn",
            json!({"rule": "R10", "message": "#7 跳過「待辦」的第一張 #5 就開始", "level": "warn", "after_block": true})
        ),
        "守則 R10 未通過（本輪已阻擋過一次，只記錄）：#7 跳過「待辦」的第一張 #5 就開始"
    );
}
