//! Rendering of the redesign through `TestBackend` / `Buffer`, with Chinese content:
//! header counts, side panel, card boxes, narrow lanes, colors, the card screen.

use kb_core::model::LaneKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::v2_state::v2_snapshot;
use super::*;
use crate::run::render_text;
use crate::text::width;
use crate::view;

fn buffer(app: &App, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    view::render(app, area, &mut buf);
    buf
}

/// Display columns of the vertical border characters in a text line.
fn border_columns(line: &str) -> Vec<usize> {
    let mut col = 0;
    let mut out = Vec::new();
    for c in line.chars() {
        if "│┃┌┐└┘┏┓┗┛├┤‹›".contains(c) {
            out.push(col);
        }
        col += width(c.encode_utf8(&mut [0; 4]));
    }
    out
}

/// Display columns `from..from + w` of a text line (a wide character that straddles the
/// start is dropped).
fn slice_cols(line: &str, from: usize, w: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for c in line.chars() {
        let cw = width(c.encode_utf8(&mut [0; 4]));
        if col >= from && col + cw <= from + w {
            out.push(c);
        }
        col += cw;
    }
    let used = width(&out);
    out + &" ".repeat(w.saturating_sub(used))
}

/// The cell where `text` starts on row `y`, by display column.
fn find_cell(buf: &Buffer, y: u16, text: &str) -> Option<u16> {
    let first = text.chars().next()?.to_string();
    (0..buf.area.width).find(|&x| {
        buf[(x, y)].symbol() == first && {
            let mut s = String::new();
            let mut cx = x;
            while s.chars().count() < text.chars().count() && cx < buf.area.width {
                let sym = buf[(cx, y)].symbol().to_string();
                cx += width(&sym).max(1) as u16;
                s.push_str(&sym);
            }
            s == text
        }
    })
}

#[test]
fn header_shows_question_and_discussion_counts_but_no_completion_ratio() {
    let app = app_with(v2_snapshot(), (120, 30));
    let text = render_text(&app, 120, 30);
    let header = text.lines().next().unwrap();
    assert!(
        header.starts_with(" Acme_DevOps › 修正登入逾時"),
        "{header}"
    );
    assert!(header.ends_with("提問 3  討論 1  ● 已連線"), "{header}");
    assert!(!header.contains("完成"), "{header}");
    // Zero counts are left out.
    let mut snap = v2_snapshot();
    snap.open_question_count = 0;
    snap.discussing_count = 0;
    let app = app_with(snap, (120, 30));
    let text = render_text(&app, 120, 30);
    let header = text.lines().next().unwrap();
    assert!(
        header.ends_with("● 已連線") && !header.contains("提問") && !header.contains("討論"),
        "{header}"
    );
    // Colors: 提問 N yellow, 討論 N cyan.
    let app = app_with(v2_snapshot(), (120, 30));
    let buf = buffer(&app, 120, 30);
    let x = find_cell(&buf, 0, "提問 3").unwrap();
    assert_eq!(buf[(x, 0)].fg, Color::Yellow);
    let x = find_cell(&buf, 0, "討論 1").unwrap();
    assert_eq!(buf[(x, 0)].fg, Color::Cyan);
}

#[test]
fn side_panel_lists_epics_with_full_ratios_and_cut_names() {
    let mut snap = v2_snapshot();
    snap.epics[0].epic.name = "非常非常長的 epic 名稱一定會被截斷".into();
    snap.epics[0].counts = counts(345, 12, 67);
    snap.no_epic = Some(counts(2, 0, 1));
    let app = app_with(snap, (90, 39));
    let text = render_text(&app, 90, 39);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[1].starts_with("┌ EPIC ─"), "{}", lines[1]);
    let side: Vec<String> = lines
        .iter()
        .map(|l| crate::text::take_width(l, 24))
        .collect();
    // Ratio and +N in full, name cut with …, and the right border in column 23.
    assert_eq!(side[2], "│   非常非… 12/345 +67 │");
    assert_eq!(side[3], "│   P2 hook 與… 0/1 +1 │");
    assert_eq!(side[4], "│ ✓ P0 資料庫      1/1 │");
    assert_eq!(side[5], "│   未歸類      0/2 +1 │");
    // The panel holds the epics only and ends above the three feed rows at the bottom.
    assert!(side[6].starts_with("│ "), "{}", side[6]);
    assert!(side[34].starts_with("└"), "{}", side[34]);
    for l in &side[1..35] {
        assert_eq!(width(l), 24, "{l}");
    }
    assert!(!text.contains("即時"), "{text}");
    assert!(
        lines[35].ends_with("#3 紀錄（決定）改用滑動視窗"),
        "{}",
        lines[35]
    );

    // A finished epic is dim; a selected one shows ◆.
    let buf = buffer(&app, 90, 39);
    assert_eq!(buf[(4, 4)].fg, Color::DarkGray, "finished epic row is dim");
    let mut app = app_with(v2_snapshot(), (90, 39));
    press(&mut app, &[Key::Char('e'), Key::Char('e')]);
    let text = render_text(&app, 90, 39);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[3].starts_with("│ ◆ P2 hook 與… 0/1 +1 │"),
        "{}",
        lines[3]
    );
    // No summary (docs/board-v2.md section 10.1), even for an epic that has one stored.
    assert!(
        !text.contains("回顧") && !text.contains("約定：hook"),
        "{text}"
    );

    // No epics at all.
    let app = app_with(demo_snapshot(), (90, 39));
    let text = render_text(&app, 90, 39);
    assert!(text.lines().nth(2).unwrap().starts_with("│ （沒有 epic）"));
}

#[test]
fn card_boxes_wrap_titles_show_tags_in_order_and_relations() {
    let mut snap = v2_snapshot();
    let long =
        "看板模型加入 epic、來源與標籤，並讓 TUI 的側欄、窄泳道與卡片關聯列都能直接取得所需的資料";
    snap.lanes[0].cards[0].card.title = long.into();
    snap.lanes[0].cards[0].card.blocked_reason = Some("等".into());
    let app = app_with(snap, (90, 39));
    let text = render_text(&app, 90, 39);
    let lines: Vec<&str> = text.lines().collect();
    // Lane 待辦 is columns 24..50; its first card box starts at row 2.
    let lane: Vec<String> = lines.iter().map(|l| slice_cols(l, 24, 26)).collect();
    assert_eq!(lane[1], "┌ 待辦 (2) ──────────────┐");
    assert_eq!(lane[2], "│┏━━━━━━━━━━━━━━━━━━━━━━┓│");
    // `epic、` moves down together: a closing mark never starts a line.
    assert_eq!(lane[3], "│┃▶#1 看板模型加入      ┃│");
    assert_eq!(lane[4], "│┃ epic、來源與標籤，並 ┃│");
    assert_eq!(lane[5], "│┃ 讓 TUI 的側欄、窄泳… ┃│");
    // Tags in order 提問 阻塞 討論 (each padded by a space on its colored background),
    // after the epic name, which is cut or left out when there is no room.
    assert_eq!(lane[6], "│┃  提問   阻塞   討論  ┃│");
    assert_eq!(lane[7], "│┃ → 1                  ┃│");
    assert_eq!(lane[8], "│┗━━━━━━━━━━━━━━━━━━━━━━┛│");
    // The second card: epic name and 衍生, then its relations on two rows (the pair
    // does not fit in one row at this width; neither item is cut).
    assert_eq!(lane[9], "│┌──────────────────────┐│");
    assert_eq!(lane[10], "││ #2 用真實的 session  ││");
    assert_eq!(lane[11], "││ 驗證 hook            ││");
    assert_eq!(lane[12], "││ P2 hoo…   阻塞  衍生 ││");
    assert_eq!(lane[13], "││ ↳ 延伸自 #1          ││");
    assert_eq!(lane[14], "││ ↳ 被 #3 阻擋         ││");

    // Border alignment in every row of the lane band: lane and card borders only at the
    // expected display columns, whatever the Chinese text inside.
    let strip = app.lane_strip();
    let mut expected = vec![0usize, 23];
    for s in &strip.slots {
        let (x, w) = (s.x as usize, s.width as usize);
        expected.extend([x, x + w - 1]);
        if !s.narrow {
            expected.extend([x + 1, x + w - 2]);
        }
    }
    // Rows 2..=34 are the panel and lane band; rows 35..=37 are the feed.
    for (y, l) in lines.iter().enumerate().take(35).skip(2) {
        for c in border_columns(l) {
            assert!(expected.contains(&c), "row {y}: border at column {c}: {l}");
        }
        for s in &strip.slots {
            let cols = border_columns(l);
            assert!(
                cols.contains(&(s.x as usize)),
                "row {y}: lane edge {}: {l}",
                s.x
            );
            assert!(
                cols.contains(&(s.x as usize + s.width as usize - 1)),
                "row {y}: lane edge: {l}"
            );
        }
        assert!(width(l) <= 90, "{l}");
    }

    // Colors: the border follows the first filled tag (提問 → yellow); tags have their
    // background; the selected card's border is thick.
    let buf = buffer(&app, 90, 39);
    assert_eq!(buf[(25, 2)].symbol(), "┏");
    assert_eq!(buf[(25, 2)].fg, Color::Yellow);
    let x = find_cell(&buf, 6, "提問").unwrap();
    assert_eq!(
        (buf[(x, 6)].fg, buf[(x, 6)].bg),
        (Color::Black, Color::Yellow)
    );
    let x = find_cell(&buf, 6, "阻塞").unwrap();
    assert_eq!((buf[(x, 6)].fg, buf[(x, 6)].bg), (Color::White, Color::Red));
    let x = find_cell(&buf, 6, "討論").unwrap();
    assert_eq!(
        (buf[(x, 6)].fg, buf[(x, 6)].bg),
        (Color::Black, Color::Cyan)
    );
}

#[test]
fn narrow_lanes_show_one_dim_row_per_card_and_widen_when_selected() {
    let app = app_with(v2_snapshot(), (120, 30));
    let text = render_text(&app, 120, 30);
    let lines: Vec<&str> = text.lines().collect();
    let strip = app.lane_strip();
    let narrow: Vec<_> = strip.slots.iter().filter(|s| s.narrow).collect();
    assert_eq!(narrow.len(), 2);
    assert!(narrow.iter().all(|s| s.width == 14));
    assert!(lines[1].contains("┌ 完成 1 ────┐"), "{}", lines[1]);
    assert!(lines[1].contains("┌ 捨棄 1 ────┐"), "{}", lines[1]);
    assert!(lines[2].contains("│ #4 建立資… │"), "{}", lines[2]);
    let buf = buffer(&app, 120, 30);
    let x = find_cell(&buf, 2, "#4").unwrap();
    assert_eq!(buf[(x, 2)].fg, Color::DarkGray);

    // Selected: normal width, a card box, and the title with parentheses.
    let mut app = app_with(v2_snapshot(), (120, 30));
    press(&mut app, &[Key::Right, Key::Right]);
    let text = render_text(&app, 120, 30);
    assert!(text.contains("完成 (1)"), "{text}");
    assert!(text.contains("┃▶#4 建立資料表"), "{text}");
}

#[test]
fn cards_outside_the_selected_epic_are_dim() {
    let mut app = app_with(v2_snapshot(), (120, 30));
    press(&mut app, &[Key::Char('e')]); // epic 1: #1 and #3
    let buf = buffer(&app, 120, 30);
    let y1 = (0..30)
        .find(|&y| find_cell(&buf, y, "#1").is_some())
        .unwrap();
    let x = find_cell(&buf, y1, "#1").unwrap();
    assert_ne!(buf[(x, y1)].fg, Color::DarkGray, "#1 is in the epic");
    let y2 = (0..30)
        .find(|&y| find_cell(&buf, y, "#2").is_some())
        .unwrap();
    let x = find_cell(&buf, y2, "#2").unwrap();
    assert_eq!(buf[(x, y2)].fg, Color::DarkGray, "#2 is not");
    // A dim card loses its tag colors.
    let (x, y) = (y2..y2 + 4)
        .find_map(|y| find_cell(&buf, y, "阻塞").map(|x| (x, y)))
        .unwrap();
    assert_eq!(
        (buf[(x, y)].fg, buf[(x, y)].bg),
        (Color::DarkGray, Color::Reset)
    );
}

#[test]
fn changed_cards_get_a_green_border_for_two_seconds() {
    let mut app = app_with(v2_snapshot(), (120, 30));
    let mut snap = v2_snapshot();
    snap.lanes[1].cards[0].card.title = "看板模型加入 epic（改）".into();
    let t = Instant::now();
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: t,
    });
    let border = |app: &App| {
        let buf = buffer(app, 120, 30);
        let y = (0..30)
            .find(|&y| find_cell(&buf, y, "#3").is_some())
            .unwrap();
        let x = find_cell(&buf, y, "#3").unwrap();
        // The card box's left border, two cells left of the title.
        (buf[(x - 2, y)].symbol().to_string(), buf[(x - 2, y)].fg)
    };
    assert_eq!(border(&app), ("│".to_string(), Color::Green));
    app.update(Event::Tick(t + crate::app::HIGHLIGHT));
    // Back to the color of its first filled tag (提問: yellow).
    assert_eq!(border(&app), ("│".to_string(), Color::Yellow));
}

#[test]
fn bottom_feed_stays_whether_the_side_panel_is_shown_or_hidden() {
    let mut app = app_with(v2_snapshot(), (120, 30));
    let text = render_text(&app, 120, 30);
    let lines: Vec<&str> = text.lines().collect();
    assert!(text.contains("EPIC") && !text.contains("即時"));
    assert!(
        lines[26].ends_with("#3 紀錄（決定）改用滑動視窗"),
        "{}",
        lines[26]
    );
    assert!(lines[28].ends_with("#1 由 #3 延伸"), "{}", lines[28]);
    press(&mut app, &[Key::Char('s')]);
    let text = render_text(&app, 120, 30);
    let lines: Vec<&str> = text.lines().collect();
    assert!(!text.contains("EPIC"));
    assert!(
        lines[26].ends_with("#3 紀錄（決定）改用滑動視窗"),
        "{}",
        lines[26]
    );
    assert!(lines[28].ends_with("#1 由 #3 延伸"), "{}", lines[28]);
    assert!(lines[29].contains("s 側欄"));
    // Under 80 columns the panel is hidden and `s` is not offered.
    let app = app_with(v2_snapshot(), (79, 30));
    let text = render_text(&app, 79, 30);
    assert!(!text.contains("EPIC"));
    assert!(!text.lines().nth(29).unwrap().contains("s 側欄"));
}

#[test]
fn lanes_still_align_when_narrow_and_scrolling() {
    // 80 x 24 without the side panel: every lane edge in every row.
    let lanes = vec![
        (
            lane(1, 1, "待辦", LaneKind::Todo),
            vec![card(1, 0, "中文標題很長很長很長很長很長很長很長", 1)],
        ),
        (
            lane(2, 2, "處理中", LaneKind::Doing),
            vec![card(2, 0, "短", 2)],
        ),
        (lane(3, 3, "驗證", LaneKind::Doing), vec![]),
        (
            lane(4, 4, "完成", LaneKind::Done),
            vec![card(3, 0, "完成的卡片", 2)],
        ),
    ];
    let mut app = app_with(snapshot("", lanes), (80, 24));
    press(&mut app, &[Key::Char('s')]);
    let text = render_text(&app, 80, 24);
    let lines: Vec<&str> = text.lines().collect();
    let strip = app.lane_strip();
    assert!(strip.scrolling && strip.more_right);
    for (y, line) in lines.iter().enumerate().take(19).skip(2) {
        let cols = border_columns(line);
        for s in &strip.slots {
            assert!(cols.contains(&(s.x as usize)), "row {y}: {line}");
            assert!(
                cols.contains(&((s.x + s.width - 1) as usize)),
                "row {y}: {line}"
            );
        }
    }
    assert!(text.contains('›'));
}
