//! Pure state transitions: docs/tui.md sections 4.5, 5.3, 5.4 and 8.

use std::time::{Duration, Instant};

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use kb_core::model::{LaneKind, LinkKind, NoteKind};
use kb_core::read::{BoardSummary, KindCounts, LinkDirection, WorkspaceSummary};

use super::*;
use crate::app::{DEBOUNCE, HIGHLIGHT, Overlay, changed_cards};
use crate::layout::Section;
use crate::run::translate;

const SIZE: (u16, u16) = (120, 30);

// ----- board screen: §4.5 --------------------------------------------------------------

#[test]
fn arrows_move_one_step_and_stop_at_the_ends() {
    let mut app = app_with(demo_snapshot(), SIZE);
    assert_eq!((app.sel_lane, selected_seq(&app)), (0, Some(1)));
    press(&mut app, &[Key::Down]);
    assert_eq!(selected_seq(&app), Some(2));
    press(&mut app, &[Key::Down]);
    assert_eq!(selected_seq(&app), Some(2), "stays on the last card");
    press(&mut app, &[Key::Up, Key::Up]);
    assert_eq!(selected_seq(&app), Some(1), "stays on the first card");
    press(&mut app, &[Key::End]);
    assert_eq!(selected_seq(&app), Some(2));
    press(&mut app, &[Key::Home]);
    assert_eq!(selected_seq(&app), Some(1));
    press(&mut app, &[Key::Left]);
    assert_eq!(app.sel_lane, 0, "no lane left of the first");
    press(
        &mut app,
        &[Key::Right, Key::Right, Key::Right, Key::Right, Key::Right],
    );
    assert_eq!(app.sel_lane, 3, "no lane right of the last");
    assert_eq!(selected_seq(&app), None, "empty lane selects nothing");
    press(&mut app, &[Key::Char('h')]);
    assert_eq!((app.sel_lane, selected_seq(&app)), (2, Some(4)));
    press(
        &mut app,
        &[
            Key::Char('l'),
            Key::Char('h'),
            Key::Char('k'),
            Key::Char('j'),
        ],
    );
    assert_eq!((app.sel_lane, selected_seq(&app)), (2, Some(4)));
}

#[test]
fn switching_lanes_returns_to_the_last_selected_card() {
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Down]); // #2 in 待辦
    press(&mut app, &[Key::Right]);
    assert_eq!(
        selected_seq(&app),
        Some(3),
        "new lane without memory: first card"
    );
    press(&mut app, &[Key::Left]);
    assert_eq!(selected_seq(&app), Some(2), "back to the remembered card");
}

#[test]
fn remembered_card_that_left_the_lane_falls_back_to_the_first() {
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Down, Key::Right]); // remember #2, now on 實作
    // #2 moves to 完成 while 實作 is selected.
    let mut snap = demo_snapshot();
    let c2 = snap.lanes[0].cards.remove(1);
    snap.lanes[2].cards.push(c2);
    snap.lanes[2].cards[1].card.lane_id = 3;
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
    press(&mut app, &[Key::Left]);
    assert_eq!(selected_seq(&app), Some(1));
}

#[test]
fn enter_opens_the_selected_card() {
    let mut app = app_with(demo_snapshot(), SIZE);
    let fx = press(&mut app, &[Key::Right, Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 3
        }]
    );
    assert_eq!(app.card.as_ref().map(|c| c.seq), Some(3));
    // Enter on an empty lane does nothing.
    let mut app = app_with(demo_snapshot(), SIZE);
    let fx = press(
        &mut app,
        &[Key::End, Key::Right, Key::Right, Key::Right, Key::Enter],
    );
    assert!(fx.is_empty());
    assert!(app.card.is_none());
}

#[test]
fn q_and_ctrl_c_quit() {
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Char('q')]);
    assert!(app.quit);
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Right, Key::Enter, Key::CtrlC]);
    assert!(app.quit);
}

#[test]
fn horizontal_scroll_keeps_the_selected_lane_visible() {
    let lanes: Vec<_> = (1..=6)
        .map(|i| {
            (
                lane(i, i as i32, &format!("泳道{i}"), LaneKind::Doing),
                vec![card(i as i32, 0, "卡片", 2)],
            )
        })
        .collect();
    // 80 columns without the side panel: three lanes of 26 fit.
    let mut app = app_with(snapshot("", lanes), (80, 24));
    press(&mut app, &[Key::Char('s')]);
    assert!(!app.side_shown());
    let visible =
        |app: &App| -> Vec<usize> { app.lane_strip().slots.iter().map(|s| s.index).collect() };
    assert!(app.lane_strip().scrolling);
    assert_eq!(visible(&app), vec![0, 1, 2]);
    for _ in 0..5 {
        press(&mut app, &[Key::Right]);
        assert!(visible(&app).contains(&app.sel_lane));
    }
    assert_eq!((app.sel_lane, app.lane_offset), (5, 3));
    press(&mut app, &[Key::Left]);
    assert_eq!(
        app.lane_offset, 3,
        "moving left inside the window does not scroll"
    );
    press(&mut app, &[Key::Left, Key::Left]);
    assert_eq!((app.sel_lane, app.lane_offset), (2, 2));
}

#[test]
fn vertical_scroll_keeps_the_selected_card_visible() {
    let cards: Vec<_> = (1..=12).map(|i| card(i, 0, "卡片", 2)).collect();
    let mut app = app_with(
        snapshot("", vec![(lane(1, 1, "待辦", LaneKind::Todo), cards)]),
        (80, 20),
    );
    let slot = app.lane_strip().slots[0];
    let heights = crate::layout::card_heights(&app.lanes()[0].cards, slot.width, false, &[]);
    let room = crate::layout::board_areas((80, 20), true).lanes.height - 2;
    let (_, end) = crate::layout::card_window(&heights, room, Some(0), 0);
    assert!(end < 12, "not all cards fit ({end})");
    for i in 1..12 {
        press(&mut app, &[Key::Down]);
        let off = app.card_offsets[&1];
        let (first, end) = crate::layout::card_window(&heights, room, None, off);
        assert_eq!(first, off);
        assert!(i >= first && i < end, "card {i} visible ({first}..{end})");
    }
    press(&mut app, &[Key::Home]);
    assert_eq!(app.card_offsets[&1], 0);
}

// ----- card screen: §5.2, §5.3 ---------------------------------------------------------

fn open_card(app: &mut App, lane_moves: usize, d: CardDetail) {
    for _ in 0..lane_moves {
        press(app, &[Key::Right]);
    }
    press(app, &[Key::Enter]);
    deliver_card(app, d);
}

#[test]
fn initial_focus_is_links_when_there_are_links() {
    let mut app = app_with(demo_snapshot(), SIZE);
    let links = vec![card_link(
        1,
        LinkKind::Spawned,
        LinkDirection::Outgoing,
        1,
        "補上逾時測試",
    )];
    open_card(&mut app, 1, detail(3, "內容", vec![], links));
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Links);

    let mut app = app_with(demo_snapshot(), SIZE);
    open_card(&mut app, 0, detail(1, "內容", vec![], vec![]));
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Body);
}

#[test]
fn tab_and_shift_tab_cycle_focus_in_section_order() {
    // Without questions: 內容 → 連結 → 紀錄 (docs/board-v2.md section 6.3).
    let mut app = app_with(demo_snapshot(), SIZE);
    open_card(&mut app, 0, detail(1, "內容", vec![], vec![]));
    let focus = |app: &App| app.card.as_ref().unwrap().focus;
    press(&mut app, &[Key::Tab]);
    assert_eq!(focus(&app), Section::Links);
    press(&mut app, &[Key::Tab]);
    assert_eq!(focus(&app), Section::Notes);
    press(&mut app, &[Key::Tab]);
    assert_eq!(focus(&app), Section::Body);
    press(&mut app, &[Key::BackTab]);
    assert_eq!(focus(&app), Section::Notes);
    press(&mut app, &[Key::BackTab, Key::BackTab]);
    assert_eq!(focus(&app), Section::Body);

    // With questions: 提問 → 內容 → 連結 → 紀錄, starting on 提問.
    let mut app = app_with(demo_snapshot(), SIZE);
    let mut d = detail(1, "內容", vec![], vec![]);
    d.questions = vec![question(5, 1, "要不要？", false, &["要", "不要"])];
    open_card(&mut app, 0, d);
    assert_eq!(focus(&app), Section::Questions);
    let mut seen = Vec::new();
    for _ in 0..4 {
        press(&mut app, &[Key::Tab]);
        seen.push(focus(&app));
    }
    assert_eq!(
        seen,
        vec![
            Section::Body,
            Section::Links,
            Section::Notes,
            Section::Questions
        ]
    );
    press(&mut app, &[Key::BackTab]);
    assert_eq!(focus(&app), Section::Notes);
}

#[test]
fn arrows_scroll_the_focused_section_within_bounds() {
    let body: String = (1..=40).map(|i| format!("第 {i} 行內容\n")).collect();
    let notes: Vec<_> = (1..=30)
        .map(|i| note(i, NoteKind::Progress, &format!("紀錄 {i}")))
        .collect();
    let mut app = app_with(demo_snapshot(), (80, 24));
    open_card(&mut app, 0, detail(1, body.trim_end(), notes, vec![]));
    let view = |app: &App| app.card.clone().unwrap();
    assert_eq!(view(&app).focus, Section::Body);
    press(&mut app, &[Key::Down, Key::Down]);
    assert_eq!(view(&app).body_scroll, 2);
    press(&mut app, &[Key::Up, Key::Up, Key::Up]);
    assert_eq!(view(&app).body_scroll, 0, "cannot scroll above the top");
    press(&mut app, &[Key::PageDown]);
    let page = view(&app).body_scroll;
    assert!(page > 1, "PageDown scrolls one section height ({page})");
    for _ in 0..20 {
        press(&mut app, &[Key::PageDown]);
    }
    let max = view(&app).body_scroll;
    press(&mut app, &[Key::Down]);
    assert_eq!(view(&app).body_scroll, max, "cannot scroll past the end");
    assert!(max < 40);
    // Notes scroll independently (內容 → 連結 → 紀錄).
    press(&mut app, &[Key::Tab, Key::Tab, Key::Down]);
    assert_eq!(view(&app).notes_scroll, 1);
    assert_eq!(view(&app).body_scroll, max);
    press(&mut app, &[Key::PageUp]);
    assert_eq!(view(&app).notes_scroll, 0);
}

#[test]
fn arrows_select_links() {
    let links = vec![
        card_link(
            1,
            LinkKind::Spawned,
            LinkDirection::Incoming,
            1,
            "補上逾時測試",
        ),
        card_link(2, LinkKind::Blocks, LinkDirection::Outgoing, 2, "更新文件"),
        ref_link(3, "src/auth/token.rs:120"),
    ];
    let mut app = app_with(demo_snapshot(), SIZE);
    open_card(&mut app, 1, detail(3, "", vec![], links));
    let sel = |app: &App| app.card.as_ref().unwrap().link_sel;
    assert_eq!(sel(&app), 0);
    press(&mut app, &[Key::Down]);
    assert_eq!(sel(&app), 1);
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(sel(&app), 2);
    press(&mut app, &[Key::Up]);
    assert_eq!(sel(&app), 1);
    press(&mut app, &[Key::PageUp]);
    assert_eq!(sel(&app), 0);
}

#[test]
fn enter_on_external_link_shows_the_full_target_until_the_next_key() {
    let links = vec![ref_link(9, "src/auth/token.rs:120")];
    let mut app = app_with(demo_snapshot(), SIZE);
    open_card(&mut app, 1, detail(3, "", vec![], links));
    let fx = press(&mut app, &[Key::Enter]);
    assert!(fx.is_empty());
    assert_eq!(
        app.card.as_ref().unwrap().message.as_deref(),
        Some("src/auth/token.rs:120")
    );
    assert!(app.history.is_empty());
    press(&mut app, &[Key::Down]);
    assert_eq!(app.card.as_ref().unwrap().message, None);
}

// ----- link history: §5.4 --------------------------------------------------------------

/// Opens card #3, follows its link to #4, then #4's link to #1. Ends on #1 with two
/// cards in the history. Returns the effects of the first jump.
fn drill_from_3_to_4_to_1(app: &mut App) -> Vec<Effect> {
    // Card #3 (lane 實作) links to #1 and #4.
    let links_3 = vec![
        card_link(
            1,
            LinkKind::Spawned,
            LinkDirection::Outgoing,
            1,
            "補上逾時測試",
        ),
        card_link(
            2,
            LinkKind::Continues,
            LinkDirection::Outgoing,
            4,
            "重現登入逾時的問題",
        ),
    ];
    open_card(app, 1, detail(3, "", vec![], links_3.clone()));
    press(app, &[Key::Down]); // second link → #4
    let fx = press(app, &[Key::Enter]);
    assert_eq!(app.history.len(), 1);
    // #4 links back to #3 and to #1.
    deliver_card(
        app,
        detail(
            4,
            "",
            vec![],
            vec![
                card_link(
                    2,
                    LinkKind::Continues,
                    LinkDirection::Incoming,
                    3,
                    "修正 token 更新時間",
                ),
                card_link(
                    5,
                    LinkKind::Relates,
                    LinkDirection::Outgoing,
                    1,
                    "補上逾時測試",
                ),
            ],
        ),
    );
    press(app, &[Key::Down, Key::Enter]); // → #1
    assert_eq!(app.card.as_ref().unwrap().seq, 1);
    assert_eq!(app.history.len(), 2);
    deliver_card(app, detail(1, "", vec![], vec![]));
    fx
}

#[test]
fn link_jumps_are_remembered_and_back_walks_one_level_at_a_time() {
    let mut app = app_with(demo_snapshot(), SIZE);
    let fx = drill_from_3_to_4_to_1(&mut app);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 4
        }]
    );

    // Left: back to #4 with its link selection; a reload is requested.
    let fx = press(&mut app, &[Key::Left]);
    let view = app.card.as_ref().unwrap();
    assert_eq!((view.seq, view.link_sel), (4, 1));
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 4
        }]
    );
    // Backspace works like Left: back to #3, the link to #4 still selected.
    press(&mut app, &[Key::Backspace]);
    let view = app.card.as_ref().unwrap();
    assert_eq!(
        (view.seq, view.link_sel, view.focus),
        (3, 1, Section::Links)
    );
    assert!(app.history.is_empty());

    // History empty: one more step back returns to the board with #3 selected.
    press(&mut app, &[Key::Backspace]);
    assert!(app.card.is_none());
    assert_eq!((app.sel_lane, selected_seq(&app)), (1, Some(3)));
}

#[test]
fn esc_jumps_straight_to_the_board_from_a_deep_link_chain() {
    let mut app = app_with(demo_snapshot(), SIZE);
    drill_from_3_to_4_to_1(&mut app);

    // Two cards deep: Esc skips the history and lands on the board, on the card that
    // was on screen (#1, lane 待辦).
    let fx = press(&mut app, &[Key::Esc]);
    assert!(fx.is_empty());
    assert!(app.card.is_none());
    assert!(app.history.is_empty());
    assert_eq!((app.sel_lane, selected_seq(&app)), (0, Some(1)));

    // Opening another card afterwards starts with a clean history.
    press(&mut app, &[Key::Enter]);
    assert_eq!(app.card.as_ref().unwrap().seq, 1);
    press(&mut app, &[Key::Backspace]);
    assert!(app.card.is_none());
}

#[test]
fn esc_to_board_selects_the_card_in_its_current_lane() {
    let mut app = app_with(demo_snapshot(), SIZE);
    open_card(&mut app, 0, detail(1, "", vec![], vec![]));
    // While the card is open, #1 moves to 完成.
    let mut snap = demo_snapshot();
    let c1 = snap.lanes[0].cards.remove(0);
    snap.lanes[2].cards.push(c1);
    snap.lanes[2].cards[1].card.lane_id = 3;
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
    press(&mut app, &[Key::Esc]);
    assert_eq!((app.sel_lane, selected_seq(&app)), (2, Some(1)));
}

// ----- refresh: §8 ---------------------------------------------------------------------

fn reload(app: &mut App, snap: BoardSnapshot, at: Instant) {
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at,
    });
}

#[test]
fn selection_follows_a_card_moved_to_another_lane() {
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Right]); // #3 in 實作
    let mut snap = demo_snapshot();
    let c3 = snap.lanes[1].cards.remove(0);
    snap.lanes[2].cards.insert(0, c3);
    snap.lanes[2].cards[0].card.lane_id = 3;
    reload(&mut app, snap, Instant::now());
    assert_eq!((app.sel_lane, selected_seq(&app)), (2, Some(3)));
    // Leaving the lane and coming back still finds #3 (remembered in its new lane).
    press(&mut app, &[Key::Left, Key::Right]);
    assert_eq!(selected_seq(&app), Some(3));
}

#[test]
fn changed_and_new_cards_are_highlighted_for_two_seconds() {
    let mut app = app_with(demo_snapshot(), SIZE);
    assert!(app.highlights.is_empty(), "first load highlights nothing");
    let mut snap = demo_snapshot();
    snap.lanes[0].cards[0].card.title = "補上逾時測試（改）".into(); // #1 edited
    snap.lanes[2].cards[0].note_count += 1; // #4 got a note
    snap.lanes[0].cards.push(card(5, 1, "新卡片", 2)); // #5 new
    let t = Instant::now();
    reload(&mut app, snap.clone(), t);
    let mut ids: Vec<i64> = app.highlights.keys().copied().collect();
    ids.sort();
    assert_eq!(ids, vec![101, 104, 105]);
    assert_eq!(changed_cards(&snap, &snap), Vec::<i64>::new());

    app.update(Event::Tick(t + HIGHLIGHT - Duration::from_millis(1)));
    assert_eq!(app.highlights.len(), 3, "still green just before 2 s");
    app.update(Event::Tick(t + HIGHLIGHT));
    assert!(app.highlights.is_empty(), "green ends after 2 s");
}

#[test]
fn notifications_are_debounced_and_filtered_by_board() {
    let mut app = app_with(demo_snapshot(), SIZE);
    let t = Instant::now();
    let notify = |board: i64, at: Instant| Event::Notify {
        workspace: 10,
        board,
        at,
    };
    assert!(app.update(notify(BOARD_ID, t)).is_empty());
    assert!(
        app.update(notify(BOARD_ID, t + Duration::from_millis(60)))
            .is_empty()
    );
    assert!(
        app.update(Event::Tick(t + Duration::from_millis(120)))
            .is_empty()
    );
    let fx = app.update(Event::Tick(t + Duration::from_millis(60) + DEBOUNCE));
    assert_eq!(fx, vec![Effect::LoadBoard(BoardTarget::Id(BOARD_ID))]);
    assert!(
        app.update(Event::Tick(t + Duration::from_secs(1)))
            .is_empty(),
        "one reload per burst"
    );
    // Another board's notification does not reload.
    app.update(notify(99, t + Duration::from_secs(2)));
    assert!(
        app.update(Event::Tick(t + Duration::from_secs(3)))
            .is_empty()
    );
}

#[test]
fn open_card_screen_reloads_and_keeps_scroll_and_link_selection() {
    let links = vec![
        card_link(
            1,
            LinkKind::Spawned,
            LinkDirection::Incoming,
            1,
            "補上逾時測試",
        ),
        card_link(2, LinkKind::Blocks, LinkDirection::Outgoing, 2, "更新文件"),
    ];
    let notes: Vec<_> = (1..=30)
        .map(|i| note(i, NoteKind::Progress, "紀錄"))
        .collect();
    let mut app = app_with(demo_snapshot(), (80, 24));
    open_card(&mut app, 1, detail(3, "", notes.clone(), links.clone()));
    press(&mut app, &[Key::Down]); // link 2 selected
    press(&mut app, &[Key::Tab, Key::Down, Key::Down, Key::BackTab]); // notes scrolled by 2, focus back on links

    let t = Instant::now();
    app.update(Event::Notify {
        workspace: 10,
        board: BOARD_ID,
        at: t,
    });
    let fx = app.update(Event::Tick(t + DEBOUNCE));
    assert!(fx.contains(&Effect::LoadBoard(BoardTarget::Id(BOARD_ID))));
    assert!(fx.contains(&Effect::LoadCard {
        board_id: BOARD_ID,
        seq: 3
    }));

    // A new link arrives first in the list and a note is added.
    let mut links2 = vec![ref_link(7, "docs/tui.md")];
    links2.extend(links);
    let mut notes2 = notes;
    notes2.push(note(31, NoteKind::Decision, "新紀錄"));
    deliver_card(&mut app, detail(3, "", notes2, links2));
    let view = app.card.as_ref().unwrap();
    assert_eq!(view.link_sel, 2, "still on the link with id 2");
    assert_eq!(view.notes_scroll, 2);
    assert_eq!(view.focus, Section::Links);
}

#[test]
fn listener_and_query_failures_show_reconnecting_and_reload_on_recovery() {
    let mut app = app_with(demo_snapshot(), SIZE);
    assert!(app.connected());
    app.update(Event::ListenerDown);
    assert!(!app.connected());
    let fx = app.update(Event::ListenerUp);
    assert!(app.connected());
    assert_eq!(fx, vec![Effect::LoadBoard(BoardTarget::Id(BOARD_ID))]);

    // A failed query retries after 1 s, 2 s, 5 s, 5 s.
    let t = Instant::now();
    app.update(Event::LoadFailed { at: t });
    assert!(!app.connected());
    assert!(
        app.update(Event::Tick(t + Duration::from_millis(999)))
            .is_empty()
    );
    let fx = app.update(Event::Tick(t + Duration::from_secs(1)));
    assert_eq!(fx, vec![Effect::LoadBoard(BoardTarget::Id(BOARD_ID))]);
    app.update(Event::LoadFailed { at: t });
    assert!(
        app.update(Event::Tick(t + Duration::from_millis(1999)))
            .is_empty()
    );
    assert!(
        !app.update(Event::Tick(t + Duration::from_secs(2)))
            .is_empty()
    );
    app.update(Event::LoadFailed { at: t });
    assert!(
        app.update(Event::Tick(t + Duration::from_millis(4999)))
            .is_empty()
    );
    assert!(
        !app.update(Event::Tick(t + Duration::from_secs(5)))
            .is_empty()
    );
    reload(&mut app, demo_snapshot(), t);
    assert!(app.connected(), "a successful load clears the state");
}

#[test]
fn empty_screen_opens_a_board_when_one_appears() {
    let mut app = App::new(SIZE, today(), Scope::Workspaces(vec![10]), None);
    let fx = app.start();
    assert_eq!(
        fx,
        vec![Effect::LoadBoard(BoardTarget::Latest(Scope::Workspaces(
            vec![10]
        )))]
    );
    app.update(Event::BoardLoaded {
        target: BoardTarget::Latest(Scope::Workspaces(vec![10])),
        data: None,
        at: Instant::now(),
    });
    assert!(app.loaded && app.data.is_none());
    let t = Instant::now();
    app.update(Event::Notify {
        workspace: 11,
        board: 5,
        at: t,
    });
    assert!(
        app.update(Event::Tick(t + DEBOUNCE)).is_empty(),
        "other workspace"
    );
    app.update(Event::Notify {
        workspace: 10,
        board: 5,
        at: t,
    });
    let fx = app.update(Event::Tick(t + DEBOUNCE));
    assert_eq!(
        fx,
        vec![Effect::LoadBoard(BoardTarget::Latest(Scope::Workspaces(
            vec![10]
        )))]
    );
    app.update(Event::BoardLoaded {
        target: BoardTarget::Latest(Scope::Workspaces(vec![10])),
        data: Some(data(demo_snapshot())),
        at: t,
    });
    assert_eq!(app.want, Some(BOARD_ID));
    assert_eq!(selected_seq(&app), Some(1));
}

// ----- menus: §6 -----------------------------------------------------------------------

fn ws(id: i64, name: &str, active: i64) -> WorkspaceSummary {
    let w = workspace();
    WorkspaceSummary {
        id,
        key: format!("manual:{name}"),
        name: name.into(),
        source: w.source,
        created_at: w.created_at,
        updated_at: w.updated_at,
        active_boards: active,
        archived_boards: 0,
        last_updated_at: w.updated_at,
    }
}

fn board_summary(id: i64, title: &str) -> BoardSummary {
    let b = board("");
    BoardSummary {
        id,
        workspace_id: 11,
        workspace_name: "其他".into(),
        session_key: format!("s{id}"),
        title: title.into(),
        goal: String::new(),
        status: BoardStatus::Active,
        outcome: None,
        rev: 1,
        created_at: b.created_at,
        updated_at: b.updated_at,
        archived_at: None,
        terminal_space: None,
        terminal_tab: None,
        terminal_id: None,
        cwd: None,
        open_question_count: 0,
        cards: KindCounts::default(),
    }
}

#[test]
fn workspace_menu_leads_to_board_menu_and_switches_board() {
    let mut app = app_with(demo_snapshot(), SIZE);
    let fx = press(&mut app, &[Key::Char('w')]);
    assert_eq!(fx, vec![Effect::LoadWorkspaces]);
    app.update(Event::WorkspacesLoaded(vec![
        ws(10, "Acme_DevOps", 1),
        ws(11, "其他", 2),
    ]));
    let fx = press(&mut app, &[Key::Down, Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadBoards {
            workspace_id: Some(11)
        }]
    );
    app.update(Event::BoardsLoaded {
        workspace_id: Some(11),
        boards: vec![board_summary(7, "甲"), board_summary(8, "乙")],
    });
    let fx = press(&mut app, &[Key::Down, Key::Enter]);
    assert_eq!(fx, vec![Effect::LoadBoard(BoardTarget::Id(8))]);
    assert!(app.overlay.is_none());
    assert_eq!(app.want, Some(8));
    // A late reload of the old board is ignored.
    reload(&mut app, demo_snapshot(), Instant::now());
    assert_eq!(app.data.as_ref().unwrap().id(), BOARD_ID);
    assert_eq!(app.want, Some(8));
}

#[test]
fn menus_cancel_with_esc_and_update_live() {
    let mut app = app_with(demo_snapshot(), SIZE);
    let fx = press(&mut app, &[Key::Char('b')]);
    assert_eq!(
        fx,
        vec![Effect::LoadBoards {
            workspace_id: Some(10)
        }]
    );
    app.update(Event::BoardsLoaded {
        workspace_id: Some(10),
        boards: vec![board_summary(2, "乙"), board_summary(BOARD_ID, "目前")],
    });
    let sel = |app: &App| match &app.overlay {
        Some(Overlay::Boards { picker, .. }) => picker.sel,
        _ => usize::MAX,
    };
    assert_eq!(sel(&app), 1, "starts on the current board");
    // A notification while the menu is open reloads the menu.
    let t = Instant::now();
    app.update(Event::Notify {
        workspace: 10,
        board: 2,
        at: t,
    });
    let fx = app.update(Event::Tick(t + DEBOUNCE));
    assert_eq!(
        fx,
        vec![Effect::LoadBoards {
            workspace_id: Some(10)
        }]
    );
    // Board 2 moved below the current board: the selection stays on the same board.
    app.update(Event::BoardsLoaded {
        workspace_id: Some(10),
        boards: vec![board_summary(BOARD_ID, "目前"), board_summary(2, "乙")],
    });
    assert_eq!(sel(&app), 0);
    press(&mut app, &[Key::Esc]);
    assert!(app.overlay.is_none());
    assert!(!app.quit);
}

#[test]
fn any_key_closes_help() {
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Char('?')]);
    assert!(matches!(app.overlay, Some(Overlay::Help)));
    press(&mut app, &[Key::Char('x')]);
    assert!(app.overlay.is_none());
    assert_eq!(
        selected_seq(&app),
        Some(1),
        "the closing key is not acted on"
    );
}

#[test]
fn only_key_presses_count() {
    let key = |kind| {
        TermEvent::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        })
    };
    assert!(matches!(
        translate(key(KeyEventKind::Press)),
        Some(Event::Key(Key::Down))
    ));
    assert!(translate(key(KeyEventKind::Release)).is_none());
    let ctrl_c = TermEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(matches!(translate(ctrl_c), Some(Event::Key(Key::CtrlC))));
}
