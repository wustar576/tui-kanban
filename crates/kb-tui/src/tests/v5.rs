//! The epic screen's 已捨棄 section (docs/board-v2.md section 5.8): cards in `dropped`
//! lanes form their own group below the 卡片 section, with one selection over both, and
//! scroll together with them on a short screen (open layout of 6.0).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::v2_state::v2_snapshot;
use super::*;
use crate::app::EpicKey;
use crate::run::render_text;
use crate::text::width;
use crate::view;

fn buffer(app: &App, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    view::render(app, area, &mut buf);
    buf
}

fn reload(app: &mut App, snap: BoardSnapshot) {
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
}

fn in_epic_one(seq: i32, title: &str) -> CardSummary {
    let mut c = card(seq, 0, title, 2);
    c.card.epic_id = Some(1);
    c.epic_name = Some("P1 看板".into());
    c
}

/// The v2 board where epic 1 「P1 看板」 has #1 (待辦), #3 (處理中) and #8 (完成), and
/// four dropped cards in 捨棄 (after #5, which has no epic): #6, #7, #9, #10. Its
/// statistics: 1/3 完成. Epic 4 「P4 只有捨棄」 has only the dropped #11.
fn dropped_board() -> BoardSnapshot {
    let mut snap = v2_snapshot();
    snap.lanes[2].cards.push(in_epic_one(8, "一張完成的卡片"));
    for (seq, title) in [
        (6, "側欄改放右邊"),
        (7, "用 256 色畫標籤"),
        (9, "舊的提問畫面"),
        (10, "第二版的統計列"),
    ] {
        snap.lanes[3].cards.push(in_epic_one(seq, title));
    }
    let mut c11 = card(11, 0, "被放棄的總覽畫面", 2);
    c11.card.epic_id = Some(4);
    snap.lanes[3].cards.push(c11);
    snap.epics[0].counts = counts(3, 1, 0);
    snap.epics
        .push(epic(4, "P4 只有捨棄", None, counts(0, 0, 0)));
    snap.lanes = snap
        .lanes
        .into_iter()
        .map(|mut l| {
            for c in &mut l.cards {
                c.card.lane_id = l.lane.id;
            }
            l
        })
        .collect();
    snap
}

fn open_epic_one(app: &mut App) {
    press(app, &[Key::Left, Key::Enter]);
}

/// (selected row, selected card's seq).
fn sel(app: &App) -> (usize, Option<i64>) {
    let v = app.epic.as_ref().expect("epic screen");
    (v.sel, v.card_id.map(|id| id - 100))
}

/// Moves card `seq` to the lane at `to` (at the end).
fn moved(mut snap: BoardSnapshot, seq: i32, to: usize) -> BoardSnapshot {
    let (li, ci) = snap
        .lanes
        .iter()
        .enumerate()
        .find_map(|(li, l)| {
            l.cards
                .iter()
                .position(|c| c.card.seq == seq)
                .map(|ci| (li, ci))
        })
        .unwrap();
    let mut c = snap.lanes[li].cards.remove(ci);
    c.card.lane_id = snap.lanes[to].lane.id;
    snap.lanes[to].cards.push(c);
    snap
}

// ----- one selection over both groups -------------------------------------------------

#[test]
fn up_and_down_cross_between_the_cards_and_the_dropped_cards() {
    let mut app = app_with(dropped_board(), (88, 30));
    open_epic_one(&mut app);
    let order: Vec<i32> =
        crate::app::epic_cards(&app.data.as_ref().unwrap().snapshot, EpicKey::Epic(1))
            .iter()
            .map(|c| c.card.seq)
            .collect();
    assert_eq!(order, vec![1, 3, 8, 6, 7, 9, 10], "cards, then dropped");
    assert_eq!(sel(&app), (0, Some(1)));
    press(&mut app, &[Key::Down, Key::Down]);
    assert_eq!(sel(&app), (2, Some(8)), "the last card row");
    press(&mut app, &[Key::Down]);
    assert_eq!(sel(&app), (3, Some(6)), "↓ to the first dropped row");
    press(&mut app, &[Key::Char('j'), Key::Down, Key::Down, Key::Down]);
    assert_eq!(sel(&app), (6, Some(10)), "stops at the last row");
    press(&mut app, &[Key::Up, Key::Up, Key::Up]);
    assert_eq!(sel(&app), (3, Some(6)));
    press(&mut app, &[Key::Up]);
    assert_eq!(sel(&app), (2, Some(8)), "↑ back to the last card row");
    press(&mut app, &[Key::Char('k')]);
    assert_eq!(sel(&app), (1, Some(3)));
    // Home / End over the combined list.
    press(&mut app, &[Key::End]);
    assert_eq!(sel(&app), (6, Some(10)));
    press(&mut app, &[Key::Home]);
    assert_eq!(sel(&app), (0, Some(1)));
}

#[test]
fn page_keys_move_over_the_combined_list() {
    // 12 rows: a 5-row list, so a page is 5 rows (the state works at any size).
    let mut app = app_with(dropped_board(), (88, 12));
    open_epic_one(&mut app);
    press(&mut app, &[Key::PageDown]);
    assert_eq!(sel(&app), (5, Some(9)), "into the dropped group");
    press(&mut app, &[Key::PageDown]);
    assert_eq!(sel(&app), (6, Some(10)), "stops at the last row");
    press(&mut app, &[Key::PageUp]);
    assert_eq!(sel(&app), (1, Some(3)), "back into the card group");
    press(&mut app, &[Key::PageUp]);
    assert_eq!(sel(&app), (0, Some(1)));
}

#[test]
fn enter_opens_a_dropped_card_and_backspace_returns_to_the_same_row() {
    let mut app = app_with(dropped_board(), (88, 30));
    open_epic_one(&mut app);
    press(&mut app, &[Key::End, Key::Up]);
    assert_eq!(sel(&app), (5, Some(9)));
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 9
        }]
    );
    assert!(app.epic.is_none());
    assert_eq!(app.card.as_ref().unwrap().seq, 9);
    deliver_card(&mut app, detail(9, "", vec![], vec![]));
    press(&mut app, &[Key::Backspace]);
    assert!(app.card.is_none());
    assert_eq!(sel(&app), (5, Some(9)), "the same dropped row");
    let text = render_text(&app, 88, 30);
    assert!(text.contains("\n  ▶ #9   捨棄    舊的提問畫面"), "{text}");
    // Esc from there: the board, the side panel on that epic.
    press(&mut app, &[Key::Esc]);
    assert!(app.epic.is_none() && app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)));
}

#[test]
fn the_selection_follows_a_card_that_is_dropped_or_restored() {
    let mut app = app_with(dropped_board(), (88, 15));
    open_epic_one(&mut app);
    press(&mut app, &[Key::Down]);
    assert_eq!(sel(&app), (1, Some(3)));
    // #3 is dropped: it moves to the end of the dropped group and stays selected.
    let dropped3 = moved(dropped_board(), 3, 3);
    reload(&mut app, dropped3.clone());
    assert_eq!(sel(&app), (6, Some(3)));
    let text = render_text(&app, 88, 15);
    assert!(
        text.contains("卡片 (2)") && text.contains("已捨棄 (5)"),
        "{text}"
    );
    // No card left in 處理中: the lane column narrows to 捨棄.
    assert!(
        text.contains("\n  ▶ #3   捨棄  看板模型加入 epic"),
        "{text}"
    );
    // Restored to 處理中: back in the card group, still selected.
    reload(&mut app, dropped_board());
    assert_eq!(sel(&app), (1, Some(3)));
    let text = render_text(&app, 88, 15);
    assert!(
        text.contains("\n  ▶ #3   處理中  看板模型加入 epic"),
        "{text}"
    );

    // A dropped card restored to 待辦 while selected: it joins the card group.
    press(&mut app, &[Key::End, Key::Up, Key::Up]);
    assert_eq!(sel(&app), (4, Some(7)));
    reload(&mut app, moved(dropped_board(), 7, 0));
    assert_eq!(
        sel(&app),
        (1, Some(7)),
        "#1, #7 (待辦), #3, #8, then dropped"
    );
    let text = render_text(&app, 88, 15);
    assert!(
        text.contains("卡片 (4)") && text.contains("已捨棄 (3)"),
        "{text}"
    );
    assert!(
        text.contains("\n  ▶ #7   待辦    用 256 色畫標籤"),
        "{text}"
    );
}

#[test]
fn an_epic_with_no_dropped_cards_or_only_dropped_cards() {
    // Epic 2: only #2, nothing dropped.
    let mut app = app_with(dropped_board(), (88, 30));
    press(&mut app, &[Key::Left, Key::Down, Key::Enter]);
    assert_eq!(app.epic.as_ref().unwrap().key, EpicKey::Epic(2));
    assert_eq!(sel(&app), (0, Some(2)));
    press(&mut app, &[Key::Down, Key::End, Key::PageDown]);
    assert_eq!(sel(&app), (0, Some(2)));
    let text = render_text(&app, 88, 30);
    assert!(text.contains("\n  卡片 (1) ─"), "{text}");
    assert!(!text.contains("已捨棄"), "no dropped section: {text}");

    // Epic 4: only the dropped #11. The card section shows its empty text; the dropped
    // row takes the selection.
    let mut app = app_with(dropped_board(), (88, 30));
    press(&mut app, &[Key::Left, Key::End, Key::Enter]);
    assert_eq!(app.epic.as_ref().unwrap().key, EpicKey::Epic(4));
    assert_eq!(sel(&app), (0, Some(11)));
    press(&mut app, &[Key::Up, Key::Down, Key::Home]);
    assert_eq!(sel(&app), (0, Some(11)));
    let text = render_text(&app, 88, 30);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[3], "  0/0 完成");
    assert!(lines[6].starts_with("  卡片 (0) ─"), "{text}");
    assert_eq!(lines[8], "    （沒有卡片）");
    assert_eq!((lines[9], lines[10]), ("", ""));
    assert!(lines[11].starts_with("  已捨棄 (1) ─"), "{text}");
    assert_eq!(lines[12], "");
    assert!(
        lines[13].starts_with("  ▶ #11  捨棄  被放棄的總覽畫面"),
        "{text}"
    );
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 11
        }]
    );
}

#[test]
fn the_no_epic_screen_puts_its_dropped_cards_in_their_own_group() {
    // 「未歸類」: #5 (捨棄) is the only card without an epic.
    let mut snap = dropped_board();
    snap.no_epic = Some(counts(0, 0, 0));
    let mut app = app_with(snap, (88, 30));
    press(&mut app, &[Key::Left, Key::End, Key::Enter]);
    assert_eq!(app.epic.as_ref().unwrap().key, EpicKey::NoEpic);
    assert_eq!(sel(&app), (0, Some(5)));
    let text = render_text(&app, 88, 30);
    assert!(text.contains("\n  卡片 (0) ─"), "{text}");
    assert!(text.contains("\n    （沒有卡片）"), "{text}");
    assert!(text.contains("\n  已捨棄 (1) ─"), "{text}");
    assert!(text.contains("\n  ▶ #5  捨棄  標籤改用 256 色"), "{text}");
}

// ----- rendering ----------------------------------------------------------------------

#[test]
fn the_dropped_section_sits_below_the_cards_in_the_open_layout() {
    let mut app = app_with(dropped_board(), (88, 30));
    open_epic_one(&mut app);
    let text = render_text(&app, 88, 30);
    let lines: Vec<&str> = text.lines().collect();
    // The statistics' total equals the 卡片 count; the dropped cards are not in it.
    assert_eq!(lines[3], "  1/3 完成");
    assert!(lines[6].starts_with("  卡片 (3) ─"), "{text}");
    assert_eq!(lines[7], "");
    assert!(
        lines[8].starts_with("  ▶ #1   待辦    TUI 回答提問"),
        "{text}"
    );
    assert!(
        lines[9].starts_with("    #3   處理中  看板模型加入 epic"),
        "{text}"
    );
    assert!(
        lines[10].starts_with("    #8   完成    一張完成的卡片"),
        "{text}"
    );
    // Two blank rows, the 已捨棄 title with its rule to the right margin, one blank row.
    assert_eq!((lines[11], lines[12]), ("", ""), "{text}");
    assert!(lines[13].starts_with("  已捨棄 (4) ─"), "{text}");
    assert!(lines[13].ends_with('─'));
    assert_eq!(width(lines[13]), 86, "the rule reaches the right margin");
    assert_eq!(lines[14], "");
    // The same row format: aligned columns across both sections.
    assert!(
        lines[15].starts_with("    #6   捨棄    側欄改放右邊"),
        "{text}"
    );
    assert!(
        lines[16].starts_with("    #7   捨棄    用 256 色畫標籤"),
        "{text}"
    );
    assert!(
        lines[17].starts_with("    #9   捨棄    舊的提問畫面"),
        "{text}"
    );
    assert!(
        lines[18].starts_with("    #10  捨棄    第二版的統計列"),
        "{text}"
    );
    assert_eq!(lines[19], "");
    assert!(!text.contains("#5"), "#5 has no epic");

    let buf = buffer(&app, 88, 30);
    // The 卡片 title is the focused section (reverse); the 已捨棄 title is bold, not
    // reversed, its rule dim.
    assert!(buf[(2, 6)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(2, 13)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(2, 13)].modifier.contains(Modifier::REVERSED));
    assert_ne!(buf[(2, 13)].fg, Color::DarkGray);
    assert_eq!(buf[(20, 13)].symbol(), "─");
    assert_eq!(buf[(20, 13)].fg, Color::DarkGray);
    // Every dropped row is dim; the done card #8 is dim in the card section; the others
    // are not.
    for y in 15..19 {
        for x in [4, 9, 17] {
            assert_eq!(buf[(x, y)].fg, Color::DarkGray, "{x},{y}");
        }
    }
    assert_eq!(buf[(4, 10)].fg, Color::DarkGray);
    assert_ne!(buf[(4, 9)].fg, Color::DarkGray);

    // A selected dropped row: ▶ at the margin, reverse video on its text, still dim.
    press(&mut app, &[Key::End]);
    let text = render_text(&app, 88, 30);
    assert!(
        text.lines()
            .nth(18)
            .unwrap()
            .starts_with("  ▶ #10  捨棄    第二版的統計列"),
        "{text}"
    );
    let buf = buffer(&app, 88, 30);
    assert_eq!(buf[(2, 18)].symbol(), "▶");
    assert!(buf[(4, 18)].modifier.contains(Modifier::REVERSED));
    assert_eq!(buf[(4, 18)].fg, Color::DarkGray);
    assert!(
        !buf[(4, 8)].modifier.contains(Modifier::REVERSED),
        "one selection"
    );
    assert!(!buf[(2, 13)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn both_sections_scroll_together_and_the_selected_row_stays_visible() {
    // 15 rows (a short screen): 1 blank row above each section title, an 8-line list.
    let mut app = app_with(dropped_board(), (88, 15));
    open_epic_one(&mut app);
    let text = render_text(&app, 88, 15);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[4].starts_with("  卡片 (3) ─"), "{text}");
    assert!(
        lines[4].ends_with(" 1-5/7"),
        "the range over both groups: {text}"
    );
    assert!(lines[6].starts_with("  ▶ #1 "), "{text}");
    // Short screen: one blank row above the 已捨棄 title, one below.
    assert_eq!(lines[9], "");
    assert!(lines[10].starts_with("  已捨棄 (4) ─"), "{text}");
    assert_eq!(lines[11], "");
    assert!(!text.contains("#10"), "{text}");

    // Every row, down and back up: the selected one is always on screen, in either group.
    let seqs = [1, 3, 8, 6, 7, 9, 10];
    let check = |app: &App, seq: i32| {
        let text = render_text(app, 88, 15);
        let lines: Vec<&str> = text.lines().collect();
        let marked: Vec<&&str> = lines[6..14]
            .iter()
            .filter(|l| l.starts_with("  ▶ "))
            .collect();
        assert_eq!(marked.len(), 1, "{text}");
        assert!(
            marked[0].starts_with(&format!("  ▶ #{seq} ")),
            "#{seq}: {text}"
        );
        assert!(
            lines[4].starts_with("  卡片 (3) ─"),
            "the 卡片 title stays: {text}"
        );
        assert!(lines[14].starts_with("  ↑↓ 選卡片"), "{text}");
    };
    for (i, seq) in seqs.iter().enumerate() {
        if i > 0 {
            press(&mut app, &[Key::Down]);
        }
        check(&app, *seq);
    }
    // At the end: the dropped rows and their title are on screen, the first cards are
    // not.
    let text = render_text(&app, 88, 15);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[4].ends_with(" 3-7/7"), "{text}");
    assert!(lines[6].starts_with("    #8 "), "{text}");
    assert!(lines[8].starts_with("  已捨棄 (4) ─"), "{text}");
    assert!(lines[13].starts_with("  ▶ #10 "), "{text}");
    assert!(!text.contains("#1 ") && !text.contains("#3 "), "{text}");
    for seq in seqs.iter().rev() {
        check(&app, *seq);
        press(&mut app, &[Key::Up]);
    }
    // Home / End jump and scroll.
    press(&mut app, &[Key::End]);
    check(&app, 10);
    press(&mut app, &[Key::Home]);
    check(&app, 1);
}
