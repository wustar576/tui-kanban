//! Rendering through ratatui's `TestBackend` with Chinese content.

use kb_core::model::{LaneKind, LinkKind, NoteKind};
use kb_core::read::LinkDirection;

use super::*;
use crate::run::render_text;
use crate::text::width;

/// Display columns at which `ch` appears in a text line.
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

#[test]
fn board_screen_shows_header_side_panel_lanes_and_keys() {
    let app = app_with(demo_snapshot(), (120, 30));
    let text = render_text(&app, 120, 30);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with(" Acme_DevOps › 修正登入逾時"),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with("  ● 已連線"), "{}", lines[0]);
    assert!(
        !lines[0].contains("完成"),
        "no completion ratio: {}",
        lines[0]
    );
    // No goal row: the side panel and the lanes start right under the header.
    assert!(lines[1].starts_with("┌ EPIC ─"), "{}", lines[1]);
    assert!(lines[1].contains("待辦 (2)"), "{}", lines[1]);
    assert!(
        lines[1].contains("┌ 捨棄 0 ─"),
        "narrow lane title: {}",
        lines[1]
    );
    assert!(text.contains("▶#1 補上逾時測試"), "{text}");
    // The side panel holds the epics only. The live feed is the three rows at the
    // bottom, newest first, over the full width.
    assert!(lines[2].starts_with("│ （沒有 epic）"), "{}", lines[2]);
    assert!(!text.contains("即時"), "{text}");
    assert!(
        lines[25].starts_with("└"),
        "panel ends above the feed: {}",
        lines[25]
    );
    assert!(
        lines[26].ends_with("#3 紀錄（決定）改用滑動視窗"),
        "{}",
        lines[26]
    );
    assert_eq!(
        lines[29],
        " ←→ 泳道/側欄  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  e epic  o 排序  s 側欄  w b ?  q 離開"
    );
    for l in &lines {
        assert!(width(l) <= 120, "line wider than the screen: {l}");
    }
}

#[test]
fn lane_borders_stay_aligned_with_chinese_titles() {
    let long = "這是一個非常非常長的中文卡片標題，一定會被截斷成三行以內，後面的字都看不到了，真的看不到，再多寫一點讓它超過三行";
    let lanes = vec![
        (
            lane(1, 1, "待辦", LaneKind::Todo),
            vec![card(1, 0, long, 0), card(2, 0, "英文 mixed 標題 title", 1)],
        ),
        (
            lane(2, 2, "非常長的泳道名稱會被截斷喔", LaneKind::Doing),
            vec![card(3, 0, "短", 2)],
        ),
        (
            lane(3, 3, "完成", LaneKind::Done),
            vec![card(4, 0, long, 3)],
        ),
    ];
    let (w, h) = (90, 24);
    let mut app = app_with(snapshot("", lanes), (w, h));
    press(&mut app, &[Key::Char('s')]); // no side panel: lanes from column 0
    let strip = app.lane_strip();
    assert!(!strip.scrolling);
    let widths: Vec<u16> = strip.slots.iter().map(|s| s.width).collect();
    assert_eq!(widths, vec![38, 38, 14]);
    let text = render_text(&app, w, h);
    let lines: Vec<&str> = text.lines().collect();
    // Lane edges at the same display columns in every row of the lane band.
    let edges = [0, 37, 38, 75, 76, 89];
    for (y, line) in lines.iter().enumerate().take(19).skip(2) {
        let mut cols = columns_of(line, '│');
        cols.extend(columns_of(line, '┃'));
        for e in edges {
            assert!(cols.contains(&e), "row {y} edge {e}: {line}");
        }
    }
    // The long title takes three lines, the third ending with …
    assert!(
        lines[3].contains("▶#1 這是一個非常非常長的中文卡片 "),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].contains(" 標題，一定會被截斷成三行以內，"),
        "{}",
        lines[4]
    );
    assert!(lines[5].contains("…"), "{}", lines[5]);
    assert!(
        lines[1].contains("┌ 非常長的泳道名稱會被截斷喔 (1) ─"),
        "{}",
        lines[1]
    );
    assert!(lines[1].contains("┌ 完成 1 ────┐"), "{}", lines[1]);
}

#[test]
fn horizontal_scrolling_shows_the_selected_lane_and_arrows() {
    let lanes: Vec<_> = (1..=6)
        .map(|i| {
            (
                lane(i, i as i32, &format!("泳道{i}"), LaneKind::Doing),
                vec![card(i as i32, 0, &format!("卡片{i}"), 2)],
            )
        })
        .collect();
    let (w, h) = (80, 24);
    let mut app = app_with(snapshot("", lanes), (w, h));
    press(&mut app, &[Key::Char('s')]);
    // Only the lane band (the header uses › as a separator).
    let render_text = |app: &App, w: u16, h: u16| -> String {
        let text = render_text(app, w, h);
        text.lines()
            .skip(1)
            .take(h as usize - 5)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = render_text(&app, w, h);
    assert!(text.contains("泳道1") && text.contains("泳道3"));
    assert!(!text.contains("泳道4"));
    assert!(text.contains('›') && !text.contains('‹'));

    press(&mut app, &[Key::Right; 5]);
    let text = render_text(&app, w, h);
    assert!(
        text.contains("泳道6") && text.contains("▶#6 卡片6"),
        "{text}"
    );
    assert!(!text.contains("泳道3"));
    assert!(text.contains('‹') && !text.contains('›'));

    press(&mut app, &[Key::Left; 3]);
    let text = render_text(&app, w, h);
    assert!(text.contains("泳道3") && text.contains("▶#3"));
    assert!(text.contains('‹') && text.contains('›'));
}

#[test]
fn too_small_window_shows_only_the_message() {
    let app = app_with(demo_snapshot(), (59, 30));
    for (w, h) in [(59, 30), (120, 14), (50, 10)] {
        let text = render_text(&app, w, h);
        assert_eq!(text.trim(), "視窗太小", "{w}x{h}");
    }
    let text = render_text(&app, 60, 15);
    assert!(text.contains("Acme_DevOps"));
}

#[test]
fn card_screen_shows_sections_and_links() {
    let mut app = app_with(demo_snapshot(), (80, 30));
    press(&mut app, &[Key::Right, Key::Enter]);
    let mut d = detail(
        3,
        "token 更新的時間點晚於過期時間。\n排程器每 60 秒執行一次，但是 token 的有效期只有 45 秒。",
        vec![
            note(1, NoteKind::Progress, "找到原因：排程間隔大於有效期"),
            note(2, NoteKind::Decision, "改用滑動視窗更新 token"),
        ],
        vec![
            card_link(
                1,
                LinkKind::Spawned,
                LinkDirection::Incoming,
                1,
                "補上逾時測試",
            ),
            card_link(2, LinkKind::Blocks, LinkDirection::Outgoing, 2, "更新文件"),
            ref_link(3, "src/auth/token.rs:120"),
        ],
    );
    d.card.result = Some("改好了".into());
    deliver_card(&mut app, d);
    let text = render_text(&app, 80, 30);
    let lines: Vec<&str> = text.lines().collect();
    // The open layout (docs/board-v2.md section 6.0): no frame, two-cell margins, a
    // blank row above the title and between the title and the info row.
    assert_eq!(lines[0], "");
    assert!(
        lines[1].starts_with("  #3 修正 token 更新時間 "),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with("● 已連線"), "{}", lines[1]);
    assert_eq!(lines[2], "");
    assert!(
        lines[3].starts_with("  實作 · 約定 · P1 · agent · 建立 "),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].starts_with("  阻塞：等待確認 API 行為"),
        "{}",
        lines[4]
    );
    assert_eq!((lines[5], lines[6]), ("", ""));
    assert!(lines[7].starts_with("  內容 ─"), "{}", lines[7]);
    assert_eq!(lines[8], "");
    assert_eq!(lines[9], "    token 更新的時間點晚於過期時間。");
    assert!(text.contains("\n    結果：改好了\n"), "{text}");
    let links = lines
        .iter()
        .position(|l| l.starts_with("  連結 (3) ─"))
        .unwrap();
    let notes = lines
        .iter()
        .position(|l| l.starts_with("  紀錄 (2) ─"))
        .unwrap();
    assert!(links < notes, "連結 comes before 紀錄");
    assert_eq!(lines[links + 2], "  ▶ 延伸自  #1 補上逾時測試");
    assert_eq!(lines[links + 3], "    阻擋    #2 更新文件");
    assert_eq!(lines[links + 4], "    參照    src/auth/token.rs:120");
    // Newest first (docs/board-v2.md section 19.3).
    assert!(lines[notes + 2].starts_with("    ") && lines[notes + 2].ends_with(" 決定"));
    assert_eq!(lines[notes + 3], "      改用滑動視窗更新 token");
    assert_eq!(
        lines[29],
        "  ↑↓ 選連結  Enter 前往  i 留言  Tab 切換區塊  ← 上一層  Esc 看板"
    );
    for l in &lines {
        assert!(width(l) <= 78, "right margin kept: {l}");
    }
}

#[test]
fn menus_and_help_render_over_the_board() {
    let mut app = app_with(demo_snapshot(), (100, 30));
    press(&mut app, &[Key::Char('?')]);
    let text = render_text(&app, 100, 30);
    assert!(text.contains("看板畫面") && text.contains("卡片畫面"));
    assert!(text.contains("第一張或最後一張卡片"));
    assert!(text.contains("跳到有待回答提問的卡片"));
    assert!(text.contains("勾選提問的選擇"));
    // Side by side even at 80 columns, and every key fits at 80 x 24.
    let text = render_text(&app, 80, 24);
    assert!(
        text.contains("q Ctrl+C") && text.contains("直接回到看板"),
        "{text}"
    );
    press(&mut app, &[Key::Esc, Key::Char('w')]);
    assert!(render_text(&app, 100, 30).contains("載入中…"));
}

#[test]
fn snapshot_text_of_the_empty_screen() {
    let mut app = App::new((80, 20), today(), Scope::All, None);
    app.update(Event::BoardLoaded {
        target: BoardTarget::Latest(Scope::All),
        data: None,
        at: std::time::Instant::now(),
    });
    let text = render_text(&app, 80, 20);
    assert!(text.contains("目前沒有進行中的看板"));
}
