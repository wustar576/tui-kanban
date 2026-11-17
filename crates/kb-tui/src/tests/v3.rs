//! Epic drill-down (docs/board-v2.md 5.7 and 5.8), the selected epic's `◆` marks, and
//! the open layout of the card and epic screens (6.0), with both look switches.

use kb_core::model::{LaneKind, LinkKind, NoteKind};
use kb_core::read::LinkDirection;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::v2_state::v2_snapshot;
use super::*;
use crate::app::{EpicKey, MSG_EPIC_REMOVED};
use crate::layout::Look;
use crate::markdown::TableStyle;
use crate::run::render_text;
use crate::text::width;
use crate::view;

const SIZE: (u16, u16) = (120, 30);

fn buffer(app: &App, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    view::render(app, area, &mut buf);
    buf
}

/// The cell where `text` starts on row `y` (display columns).
fn find(buf: &Buffer, y: u16, text: &str) -> Option<u16> {
    let want: Vec<char> = text.chars().collect();
    (0..buf.area.width).find(|&x| {
        let mut cx = x;
        for ch in &want {
            if cx >= buf.area.width || buf[(cx, y)].symbol() != ch.to_string() {
                return false;
            }
            cx += width(buf[(cx, y)].symbol()).max(1) as u16;
        }
        true
    })
}

/// The first row containing `text`, and its column.
fn find_any(buf: &Buffer, text: &str) -> Option<(u16, u16)> {
    (0..buf.area.height).find_map(|y| find(buf, y, text).map(|x| (x, y)))
}

/// Display columns of `ch` in a text line.
fn columns_of(line: &str, ch: char) -> Vec<usize> {
    let mut col = 0;
    let mut out = Vec::new();
    for c in line.chars() {
        if c == ch {
            out.push(col);
        }
        col += width(c.encode_utf8(&mut [0; 4]));
    }
    out
}

fn reload(app: &mut App, snap: BoardSnapshot) {
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
}

/// The v2 board with 「未歸類」: #5 (in 捨棄) has no epic.
fn with_no_epic() -> BoardSnapshot {
    let mut snap = v2_snapshot();
    snap.no_epic = Some(counts(0, 0, 0));
    snap
}

// ----- focus between the side panel and the lanes (5.7) -----------------------------

#[test]
fn left_from_the_leftmost_lane_focuses_the_side_panel() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Right, Key::Left]);
    assert!(
        !app.side_focused(),
        "first ← only moves to the leftmost lane"
    );
    assert_eq!(app.sel_lane, 0);
    press(&mut app, &[Key::Left]);
    assert!(app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)), "first row selected");
    press(&mut app, &[Key::Down, Key::Down]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(3)));
    press(&mut app, &[Key::Down]);
    assert_eq!(
        app.sel_epic,
        Some(EpicKey::Epic(3)),
        "stops at the last row"
    );
    press(&mut app, &[Key::Home]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)));
    press(&mut app, &[Key::End, Key::Char('k')]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
    // → back to the lanes: the same lane and card; the epic stays selected.
    press(&mut app, &[Key::Right]);
    assert!(!app.side_focused());
    assert_eq!((app.sel_lane, selected_seq(&app)), (0, Some(1)));
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
    // Back into the panel keeps that selection; e / E work and keep the focus.
    press(&mut app, &[Key::Left]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
    press(&mut app, &[Key::Char('e')]);
    assert!(app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(3)));
    press(&mut app, &[Key::Char('E'), Key::Char('E')]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)));
    // 「未歸類」 is a row too.
    let mut app = app_with(with_no_epic(), SIZE);
    press(&mut app, &[Key::Left, Key::End]);
    assert_eq!(app.sel_epic, Some(EpicKey::NoEpic));
}

#[test]
fn the_side_panel_cannot_take_the_focus_when_hidden_or_empty() {
    // Hidden by `s`.
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('s'), Key::Left]);
    assert!(!app.side_focused() && !app.side_focus);
    // Hidden under 80 columns.
    let mut app = app_with(v2_snapshot(), (79, 30));
    press(&mut app, &[Key::Left]);
    assert!(!app.side_focus);
    // No epic and no 「未歸類」 row.
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Left]);
    assert!(!app.side_focus);
    // No epics but 「未歸類」: that row takes the selection.
    let mut snap = demo_snapshot();
    snap.no_epic = Some(counts(3, 1, 0));
    let mut app = app_with(snap, SIZE);
    press(&mut app, &[Key::Left]);
    assert!(app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::NoEpic));

    // Hiding the panel while it has the focus gives the focus back to the lanes.
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Left]);
    assert!(app.side_focused());
    press(&mut app, &[Key::Char('s')]);
    assert!(!app.side_focus);
    press(&mut app, &[Key::Char('s')]);
    assert!(
        !app.side_focused(),
        "showing it again does not take the focus"
    );
    press(&mut app, &[Key::Left]);
    assert!(app.side_focused());
    app.update(Event::Resize(70, 30));
    assert!(!app.side_focus, "a resize under 80 columns too");
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)), "the selection stays");
}

// ----- epic screen and the shared history (5.8) -------------------------------------

fn open_epic_one(app: &mut App) {
    press(app, &[Key::Left, Key::Enter]);
}

#[test]
fn enter_opens_the_epic_screen_and_the_history_is_shared_with_cards() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open_epic_one(&mut app);
    let view = app.epic.clone().expect("epic screen");
    assert_eq!(
        (view.key, view.sel, view.card_id),
        (EpicKey::Epic(1), 0, Some(101))
    );
    // Rows: #1 (待辦) then #3 (處理中).
    press(&mut app, &[Key::Down]);
    assert_eq!(app.epic.as_ref().unwrap().card_id, Some(103));
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 3
        }]
    );
    assert!(app.epic.is_none());
    assert_eq!(app.card.as_ref().unwrap().seq, 3);
    assert_eq!(app.history.len(), 1);
    deliver_card(&mut app, detail(3, "", vec![], vec![]));

    // Backspace: back to the epic screen, the same row selected.
    press(&mut app, &[Key::Backspace]);
    assert!(app.card.is_none());
    let view = app.epic.as_ref().unwrap();
    assert_eq!((view.sel, view.card_id), (1, Some(103)));
    // ← from the epic screen: the board, focus in the side panel on that epic.
    press(&mut app, &[Key::Left]);
    assert!(app.epic.is_none());
    assert!(app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)));

    // Esc from a card opened from the epic screen: the board, focus in the lanes on
    // that card.
    press(&mut app, &[Key::Enter, Key::End, Key::Enter]);
    assert_eq!(app.card.as_ref().unwrap().seq, 3);
    press(&mut app, &[Key::Esc]);
    assert!(app.card.is_none() && app.epic.is_none() && app.history.is_empty());
    assert!(!app.side_focused());
    assert_eq!((app.sel_lane, selected_seq(&app)), (1, Some(3)));

    // Esc from the epic screen itself: the board, focus in the side panel.
    press(
        &mut app,
        &[Key::Left, Key::Left, Key::Down, Key::Enter, Key::Esc],
    );
    assert!(app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
}

#[test]
fn the_epic_screen_follows_its_cards_and_returns_when_the_epic_is_removed() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open_epic_one(&mut app);
    press(&mut app, &[Key::Down]); // #3
    // A new card of the epic arrives before #3: the selection stays on #3.
    let mut snap = v2_snapshot();
    let mut c6 = card(6, 1, "新的卡片", 0);
    c6.card.epic_id = Some(1);
    c6.epic_name = Some("P1 看板".into());
    snap.lanes[0].cards.insert(0, c6);
    reload(&mut app, snap.clone());
    let view = app.epic.as_ref().unwrap();
    assert_eq!((view.sel, view.card_id), (2, Some(103)));
    // #3 leaves the epic: the selection stays on the same row index.
    let mut snap2 = snap.clone();
    snap2.lanes[1].cards[0].card.epic_id = Some(2);
    reload(&mut app, snap2);
    let view = app.epic.as_ref().unwrap();
    assert_eq!(view.sel, 1);

    // The epic is removed while its screen is open: back to the board, with a message.
    let mut snap3 = snap;
    snap3.epics.remove(0);
    reload(&mut app, snap3);
    assert!(app.epic.is_none() && app.card.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_EPIC_REMOVED));
    assert!(!app.side_focused());
    assert_eq!(app.sel_epic, None);

    // Removed while a card opened from it is on screen: Backspace finds it gone.
    let mut app = app_with(v2_snapshot(), SIZE);
    open_epic_one(&mut app);
    press(&mut app, &[Key::Enter]);
    let mut snap = v2_snapshot();
    snap.epics.remove(0);
    reload(&mut app, snap);
    assert!(app.card.is_some(), "the card screen stays");
    press(&mut app, &[Key::Backspace]);
    assert!(app.card.is_none() && app.epic.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_EPIC_REMOVED));
}

#[test]
fn the_no_epic_row_opens_the_cards_without_an_epic() {
    let mut app = app_with(with_no_epic(), SIZE);
    press(&mut app, &[Key::Left, Key::End, Key::Enter]);
    assert_eq!(app.epic.as_ref().unwrap().key, EpicKey::NoEpic);
    assert_eq!(app.epic.as_ref().unwrap().card_id, Some(105));
    let text = render_text(&app, 88, 30);
    assert!(
        text.lines().nth(1).unwrap().starts_with("  未歸類"),
        "{text}"
    );
    assert!(text.contains("#5  捨棄  標籤改用 256 色"), "{text}");
}

// ----- the selected epic's marks (5.7) ----------------------------------------------

#[test]
fn cards_of_the_selected_epic_carry_a_blue_diamond() {
    let mut app = app_with(v2_snapshot(), SIZE);
    // No epic selected: no mark anywhere.
    assert!(!render_text(&app, 120, 30).contains('◆'));

    press(&mut app, &[Key::Char('e')]); // epic 1: #1 (selected card) and #3
    let text = render_text(&app, 120, 30);
    let lines: Vec<&str> = text.lines().collect();
    let buf = buffer(&app, 120, 30);
    // The side panel row.
    assert!(lines[2].starts_with("│ ◆ P1 看板"), "{}", lines[2]);
    // #1 is the selected card: thick border, the mark replaces its second top cell.
    let strip = app.lane_strip();
    let lane0 = strip.slots[0];
    let (x0, top) = (lane0.x + 1, 2u16);
    assert_eq!(buf[(x0, top)].symbol(), "┏");
    assert_eq!(buf[(x0 + 1, top)].symbol(), "◆");
    assert_eq!(buf[(x0 + 2, top)].symbol(), "━");
    assert_eq!(
        buf[(x0 + lane0.width - 3, top)].symbol(),
        "┓",
        "border width kept"
    );
    assert_eq!(buf[(x0 + 1, top)].fg, Color::LightBlue);
    assert!(buf[(x0 + 1, top)].modifier.contains(Modifier::BOLD));
    // #3 in 處理中: a plain border with the mark.
    let lane1 = strip.slots[1];
    let x1 = lane1.x + 1;
    assert_eq!(buf[(x1, top)].symbol(), "┌");
    assert_eq!(buf[(x1 + 1, top)].symbol(), "◆");
    assert_eq!(buf[(x1 + lane1.width - 3, top)].symbol(), "┐");
    // The epic name of a marked card: bright blue and bold.
    let (ex, ey) = (0..30)
        .find_map(|y| find(&buf, y, "P1 看板").filter(|x| *x > 24).map(|x| (x, y)))
        .unwrap();
    assert_eq!(buf[(ex, ey)].fg, Color::LightBlue);
    assert!(buf[(ex, ey)].modifier.contains(Modifier::BOLD));
    // #2 (epic 2) has no mark and stays dim.
    let (tx, ty) = find_any(&buf, "#2").unwrap();
    assert_eq!(buf[(tx, ty)].fg, Color::DarkGray);
    let box_top = ty - 1;
    assert_eq!(buf[(tx - 2, box_top)].symbol(), "┌");
    assert_eq!(buf[(tx - 1, box_top)].symbol(), "─");
    // Only the selected epic's cards and row carry marks: three in all.
    assert_eq!(text.matches('◆').count(), 3, "{text}");
    // Bright blue is used for nothing else.
    for y in 0..30 {
        for x in 0..120 {
            if buf[(x, y)].fg == Color::LightBlue {
                let s = buf[(x, y)].symbol();
                assert!(
                    s == "◆" || "P1看板 ".contains(s) || s.is_empty(),
                    "bright blue at {x},{y}: {s:?}"
                );
            }
        }
    }

    // Epic 3: #4 sits in the narrow 完成 lane: `◆` before its number, not dim.
    press(&mut app, &[Key::Char('e'), Key::Char('e')]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(3)));
    let text = render_text(&app, 120, 30);
    assert!(text.contains("│◆#4 建立資… │"), "{text}");
    let buf = buffer(&app, 120, 30);
    let (nx, ny) = find_any(&buf, "#4").unwrap();
    assert_eq!(buf[(nx - 1, ny)].symbol(), "◆");
    assert_ne!(buf[(nx, ny)].fg, Color::DarkGray);
    assert!(text.contains("│ #9") || !text.contains("◆#9"));
}

#[test]
fn the_no_epic_row_marks_the_cards_without_an_epic() {
    let mut app = app_with(with_no_epic(), SIZE);
    press(&mut app, &[Key::Left, Key::End]);
    let text = render_text(&app, 120, 30);
    // #5 sits in the narrow 捨棄 lane; it is the only card without an epic.
    assert!(text.contains("│◆#5 標籤改… │"), "{text}");
    assert!(text.contains("◆ 未歸類"), "{text}");
    assert_eq!(text.matches('◆').count(), 2, "{text}");
    // Cards that belong to any epic are dim.
    let buf = buffer(&app, 120, 30);
    let (x, y) = find_any(&buf, "#1").unwrap();
    assert_eq!(buf[(x, y)].fg, Color::DarkGray);
}

// ----- the focused side panel ---------------------------------------------------------

#[test]
fn the_focused_side_panel_shows_its_row_in_reverse_video() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Left, Key::Down]);
    let buf = buffer(&app, 120, 30);
    let (x, y) = find_any(&buf, "P2 hook").unwrap();
    assert_eq!(y, 3);
    assert!(buf[(x, y)].modifier.contains(Modifier::REVERSED));
    assert!(
        buf[(21, y)].modifier.contains(Modifier::REVERSED),
        "the whole row"
    );
    assert!(!buf[(x, 2)].modifier.contains(Modifier::REVERSED));
    let (ex, ey) = find_any(&buf, "EPIC").unwrap();
    assert_eq!(
        buf[(ex, ey)].fg,
        Color::Cyan,
        "EPIC title in the accent color"
    );
    // The key bar says what the keys do now, and fits in 80 columns.
    let text = render_text(&app, 120, 30);
    let bar = text.lines().nth(29).unwrap();
    assert!(
        bar.starts_with(" ↑↓ 選 epic  Enter 開啟  → 泳道  e epic  Esc 取消 epic"),
        "{bar}"
    );
    for app in [&app, &app_with(v2_snapshot(), (80, 30))] {
        let text = render_text(app, 80, 30);
        let bar = text.lines().nth(29).unwrap();
        assert!(!bar.ends_with('…') && bar.ends_with("q 離開"), "{bar}");
    }
    // The help screen lists the side panel and the epic screen.
    press(&mut app, &[Key::Char('?')]);
    let text = render_text(&app, 80, 24);
    assert!(
        text.contains("側欄") && text.contains("epic 畫面"),
        "{text}"
    );
    assert!(
        text.contains("焦點移到側欄") && text.contains("開啟 epic 畫面"),
        "{text}"
    );
}

// ----- the epic screen (5.8, open layout of 6.0) ------------------------------------

#[test]
fn the_epic_screen_lists_its_cards_in_aligned_columns() {
    let mut snap = v2_snapshot();
    snap.lanes[1].cards[0].card.title =
        "看板模型加入 epic、來源與標籤，並讓 TUI 的側欄、窄泳道與卡片關聯列都能直接取得所需的資料"
            .into();
    let mut c12 = card(12, 3, "一張完成的卡片", 2);
    c12.card.epic_id = Some(1);
    snap.lanes[2].cards.push(c12);
    snap.epics
        .push(epic(4, "P3 總覽畫面", None, counts(0, 0, 0)));
    let mut app = app_with(snap, (88, 30));
    open_epic_one(&mut app);
    let text = render_text(&app, 88, 30);
    let lines: Vec<&str> = text.lines().collect();
    // Open layout: top padding, title, title gap, dim counts, two blank rows, 卡片.
    assert_eq!(lines[0], "");
    assert!(lines[1].starts_with("  epic：P1 看板"), "{}", lines[1]);
    assert!(lines[1].ends_with("● 已連線"));
    assert_eq!(width(lines[1]), 86, "status aligned with the right margin");
    assert_eq!(lines[2], "");
    assert_eq!(lines[3], "  0/2 完成");
    assert_eq!((lines[4], lines[5]), ("", ""));
    assert!(lines[6].starts_with("  卡片 (3) ─"), "{}", lines[6]);
    assert_eq!(lines[7], "");
    // Rows: #1 (待辦), #3 (處理中), #12 (完成), in lane order; `▶` at the side margin.
    assert!(
        lines[8].starts_with("  ▶ #1   待辦    TUI 回答提問"),
        "{}",
        lines[8]
    );
    assert!(
        lines[9].starts_with("    #3   處理中  看板模型加入 epic"),
        "{}",
        lines[9]
    );
    assert!(
        lines[10].starts_with("    #12  完成    一張完成的卡片"),
        "{}",
        lines[10]
    );
    // Long titles are cut with …; tags complete, in their own aligned column.
    assert!(lines[9].contains('…'), "{}", lines[9]);
    assert!(lines[8].ends_with("提問   討論"), "{}", lines[8]);
    assert!(lines[9].ends_with(" 提問"), "{}", lines[9]);
    assert_eq!(
        columns_of(lines[8], '提')[1..2],
        columns_of(lines[9], '提')[..1],
        "tags start in the same column"
    );
    for l in &lines[8..11] {
        assert!(width(l) <= 86, "{l}");
    }
    assert_eq!(
        lines[29],
        "  ↑↓ 選卡片  Enter 前往  i 留言  ← 上一層  Esc 看板"
    );
    let buf = buffer(&app, 88, 30);
    // The 卡片 title is the focused section: reverse video; its rule is dim.
    assert!(buf[(2, 6)].modifier.contains(Modifier::REVERSED));
    assert_eq!(buf[(15, 6)].fg, Color::DarkGray);
    // The selected row: ▶ at the side margin, reverse video on its text only.
    assert_eq!(buf[(2, 8)].symbol(), "▶");
    assert!(!buf[(2, 8)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(4, 8)].modifier.contains(Modifier::REVERSED));
    let end = 4 + width("#1   待辦    TUI 回答提問") as u16;
    // The last character (問) is wide: its own cell is two before `end`.
    assert!(buf[(end - 2, 8)].modifier.contains(Modifier::REVERSED));
    assert!(
        !buf[(end, 8)].modifier.contains(Modifier::REVERSED),
        "not to the edge"
    );
    // Done and dropped rows are dim.
    assert_eq!(buf[(4, 10)].fg, Color::DarkGray);
    assert_ne!(buf[(4, 9)].fg, Color::DarkGray);

    // No summary section (docs/board-v2.md section 10.1); an epic without cards shows
    // the empty text.
    press(&mut app, &[Key::Esc, Key::Down, Key::Enter]);
    let text = render_text(&app, 88, 30);
    assert!(!text.contains("回顧"), "{text}");
    press(&mut app, &[Key::Esc, Key::End, Key::Enter]);
    let text = render_text(&app, 88, 30);
    assert!(text.contains("\n    （沒有卡片）"), "{text}");
}

// ----- the open card screen (6.0) -----------------------------------------------------

fn rich_card(app: &mut App) {
    press(app, &[Key::Right, Key::Enter]);
    let mut d = detail(
        3,
        "# 方案\n| 部分 | 內容 |\n|---|---|\n| 資料 | epic、來源、回顧 |\n| 畫面 | 側欄 |",
        vec![
            note(1, NoteKind::Decision, "泳道維持任務進度。\n- 側欄放左邊"),
            note(2, NoteKind::Result, "完成"),
        ],
        vec![
            card_link(
                1,
                LinkKind::Spawned,
                LinkDirection::Incoming,
                1,
                "TUI 回答提問",
            ),
            card_link(
                2,
                LinkKind::Spawned,
                LinkDirection::Outgoing,
                4,
                "建立資料表",
            ),
        ],
    );
    d.epic_name = Some("P1 看板".into());
    deliver_card(app, d);
}

/// Row of the section title `t` (`  <t> ─…`).
fn title_row(lines: &[&str], t: &str) -> usize {
    lines
        .iter()
        .position(|l| l.starts_with(&format!("  {t} ─")))
        .unwrap_or_else(|| panic!("{t}:\n{}", lines.join("\n")))
}

#[test]
fn the_card_screen_is_open_with_margins_blank_rows_indent_and_text_highlights() {
    let mut app = app_with(v2_snapshot(), (88, 40));
    rich_card(&mut app);
    let text = render_text(&app, 88, 40);
    let lines: Vec<&str> = text.lines().collect();
    let buf = buffer(&app, 88, 40);
    // No frame: the two margin columns on each side stay blank.
    for y in 0..40 {
        for x in [0, 1, 86, 87] {
            assert_eq!(buf[(x, y)].symbol(), " ", "margin column {x}, row {y}");
        }
    }
    // Title block: top padding, bold title, a blank row, the dim info row, 阻塞.
    assert_eq!(lines[0], "");
    assert!(
        lines[1].starts_with("  #3 修正 token 更新時間"),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with("● 已連線") && width(lines[1]) == 86);
    assert!(buf[(2, 1)].modifier.contains(Modifier::BOLD));
    assert_eq!(lines[2], "");
    assert!(
        lines[3].starts_with("  P1 看板 · 實作 · 約定"),
        "{}",
        lines[3]
    );
    assert_eq!(buf[(2, 3)].fg, Color::DarkGray, "the info row is dim");
    assert!(lines[4].starts_with("  阻塞："), "{}", lines[4]);
    // Each section title: two blank rows above, one below, from the side margin, with
    // a dim rule to the right margin.
    let titles: Vec<usize> = ["內容", "連結 (2)", "紀錄 (2)"]
        .iter()
        .map(|t| title_row(&lines, t))
        .collect();
    for &t in &titles {
        assert_eq!(
            (lines[t - 2], lines[t - 1]),
            ("", ""),
            "above: {}",
            lines[t]
        );
        assert_eq!(lines[t + 1], "", "below: {}", lines[t]);
        assert_eq!(
            width(lines[t]),
            86,
            "rule to the right margin: {}",
            lines[t]
        );
    }
    // The focused section (連結) has its title in reverse video; the others do not.
    let (b, l) = (titles[0] as u16, titles[1] as u16);
    assert!(buf[(2, l)].modifier.contains(Modifier::REVERSED));
    assert!(!buf[(2, b)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(2, b)].modifier.contains(Modifier::BOLD));
    // Content indented by two cells from the margin (column 4).
    let t0 = titles[0];
    assert_eq!(lines[t0 + 2], "    方案");
    // The table (the chosen style: a box of dim lines, bold header, aligned cells).
    assert_eq!(lines[t0 + 3], "    ┌──────┬──────────────────┐");
    assert_eq!(lines[t0 + 4], "    │ 部分 │ 內容             │");
    assert_eq!(lines[t0 + 5], "    ├──────┼──────────────────┤");
    assert_eq!(lines[t0 + 6], "    │ 資料 │ epic、來源、回顧 │");
    assert_eq!(lines[t0 + 7], "    │ 畫面 │ 側欄             │");
    assert_eq!(lines[t0 + 8], "    └──────┴──────────────────┘");
    assert_eq!(buf[(4, (t0 + 3) as u16)].fg, Color::DarkGray);
    assert_eq!(buf[(4, (t0 + 4) as u16)].fg, Color::DarkGray);
    assert!(buf[(6, (t0 + 4) as u16)].modifier.contains(Modifier::BOLD));
    // Links: ▶ at the side margin, the text at the content column; reverse video on the
    // text only.
    let t1 = titles[1];
    assert_eq!(lines[t1 + 2], "  ▶ 延伸自  #1 TUI 回答提問");
    assert_eq!(lines[t1 + 3], "    延伸出  #4 建立資料表");
    let y = (t1 + 2) as u16;
    assert!(!buf[(2, y)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(4, y)].modifier.contains(Modifier::REVERSED));
    let end = 4 + width("延伸自  #1 TUI 回答提問") as u16;
    assert!(buf[(end - 2, y)].modifier.contains(Modifier::REVERSED));
    assert!(
        !buf[(end, y)].modifier.contains(Modifier::REVERSED),
        "not to the edge"
    );
    // Notes, newest first (docs/board-v2.md section 19.3): the header, then the body
    // indented by two more cells.
    let t2 = titles[2];
    assert!(lines[t2 + 2].starts_with("    ") && lines[t2 + 2].ends_with(" 結果"));
    assert_eq!(lines[t2 + 3], "      完成");
    assert_eq!(lines[t2 + 4], "");
    assert!(lines[t2 + 5].starts_with("    ") && lines[t2 + 5].ends_with(" 決定"));
    assert_eq!(lines[t2 + 6], "      泳道維持任務進度。");
    assert_eq!(lines[t2 + 7], "      - 側欄放左邊");
    // The last row: key hints from the side margin, without any border.
    assert_eq!(
        lines[39],
        "  ↑↓ 選連結  Enter 前往  i 留言  Tab 切換區塊  ← 上一層  Esc 看板"
    );
    // Tab moves the reverse video to the next section's title.
    press(&mut app, &[Key::Tab]);
    let buf = buffer(&app, 88, 40);
    assert!(!buf[(2, l)].modifier.contains(Modifier::REVERSED));
    assert!(
        buf[(2, titles[2] as u16)]
            .modifier
            .contains(Modifier::REVERSED)
    );
}

#[test]
fn short_screens_use_fewer_blank_rows_and_long_sections_scroll() {
    let body: String = (1..=40).map(|i| format!("第 {i} 行\n")).collect();
    let mut app = app_with(v2_snapshot(), (88, 20));
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, detail(3, body.trim_end(), vec![], vec![]));
    let text = render_text(&app, 88, 20);
    let lines: Vec<&str> = text.lines().collect();
    // Under 24 rows: no top padding, one blank row above section titles; the title gap
    // and the blank row below titles stay.
    assert!(lines[0].starts_with("  #3 "), "{}", lines[0]);
    assert_eq!(lines[1], "");
    assert!(lines[2].starts_with("  實作 · "), "{}", lines[2]);
    assert!(lines[3].starts_with("  阻塞："), "{}", lines[3]);
    assert_eq!(lines[4], "");
    // The scroll range sits at the right end of the rule, in dim.
    assert!(lines[5].starts_with("  內容 ─"), "{}", lines[5]);
    assert!(lines[5].ends_with("─ 1-4/40"), "{}", lines[5]);
    assert_eq!(width(lines[5]), 86);
    let buf = buffer(&app, 88, 20);
    assert_eq!(buf[(85, 5)].fg, Color::DarkGray);
    assert_eq!(lines[6], "");
    assert_eq!(lines[7], "    第 1 行");
    press(&mut app, &[Key::PageDown]);
    let text = render_text(&app, 88, 20);
    assert!(!text.contains("    第 1 行\n"), "{text}");
    let title = text.lines().nth(5).unwrap();
    assert!(title.ends_with("5-8/40"), "{title}");
}

#[test]
fn the_section_rule_switch() {
    let mut app = app_with(v2_snapshot(), (88, 40));
    rich_card(&mut app);
    // ON (the chosen default): a dim rule from one cell after the title to the margin.
    assert_eq!(app.look, Look::default());
    assert!(app.look.section_rule);
    let text = render_text(&app, 88, 40);
    let line = text.lines().find(|l| l.starts_with("  內容")).unwrap();
    assert_eq!(line, format!("  內容 {}", "─".repeat(84 - 5)));
    let buf = buffer(&app, 88, 40);
    let (x, y) = find_any(&buf, "內容 ─").unwrap();
    assert_eq!(buf[(x + 5, y)].fg, Color::DarkGray);
    // The focused title's rule stays dim and is not reversed.
    let (x, y) = find_any(&buf, "連結 (2) ─").unwrap();
    assert!(buf[(x, y)].modifier.contains(Modifier::REVERSED));
    assert_eq!(buf[(x + 10, y)].fg, Color::DarkGray);
    assert!(!buf[(x + 10, y)].modifier.contains(Modifier::REVERSED));
    // OFF: the title only.
    app.look.section_rule = false;
    let text = render_text(&app, 88, 40);
    assert!(text.lines().any(|l| l == "  內容"), "{text}");
    assert!(!text.contains("─────────────────────────────────────"));
}

#[test]
fn the_table_style_switch() {
    let mut app = app_with(v2_snapshot(), (88, 40));
    rich_card(&mut app);
    let table_of = |app: &App| -> Vec<String> {
        let text = render_text(app, 88, 40);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let at = lines.iter().position(|l| l == "    方案").unwrap();
        lines[at + 1..]
            .iter()
            .take_while(|l| !l.is_empty())
            .cloned()
            .collect()
    };
    assert_eq!(app.look.tables, TableStyle::Grid, "the chosen default");
    app.look.tables = TableStyle::HeaderRule;
    assert_eq!(
        table_of(&app),
        vec![
            "    部分  內容",
            "    ────  ────────────────",
            "    資料  epic、來源、回顧",
            "    畫面  側欄",
        ]
    );
    app.look.tables = TableStyle::Book;
    let rule = format!("    {}", "─".repeat(22));
    assert_eq!(
        table_of(&app),
        vec![
            rule.as_str(),
            "    部分  內容",
            rule.as_str(),
            "    資料  epic、來源、回顧",
            "    畫面  側欄",
            rule.as_str(),
        ]
    );
    app.look.tables = TableStyle::Grid;
    assert_eq!(
        table_of(&app),
        vec![
            "    ┌──────┬──────────────────┐",
            "    │ 部分 │ 內容             │",
            "    ├──────┼──────────────────┤",
            "    │ 資料 │ epic、來源、回顧 │",
            "    │ 畫面 │ 側欄             │",
            "    └──────┴──────────────────┘",
        ]
    );
    // Every table line is dim; the header text is bold.
    let buf = buffer(&app, 88, 40);
    let (x, y) = find_any(&buf, "┌──").unwrap();
    assert_eq!(buf[(x, y)].fg, Color::DarkGray);
    assert_eq!(buf[(x, y + 1)].fg, Color::DarkGray);
    assert!(buf[(x + 2, y + 1)].modifier.contains(Modifier::BOLD));
    // The layout accounts for the extra rows: the notes still follow.
    let text = render_text(&app, 88, 40);
    assert!(text.contains("  紀錄 (2)"), "{text}");
    let _ = LaneKind::Todo;
}
