//! The 紀錄 section of the card screen lists notes newest first (docs/board-v2.md section
//! 19.3), the live feed line of rule R11 (section 19.4) and that of the turn `kb board
//! start` starts (section 12.4).

use kb_core::model::{Cause, CauseKind, NoteKind};
use serde_json::json;

use super::*;
use crate::layout::{Section, notes_newest_first};
use crate::run::render_text;
use crate::text::activity_text;

/// Row of the 紀錄 section title (`  紀錄 (N) ─…`).
fn notes_title(lines: &[&str], n: usize) -> usize {
    let head = format!("  紀錄 ({n}) ─");
    lines
        .iter()
        .position(|l| l.starts_with(&head))
        .unwrap_or_else(|| panic!("no {head}:\n{}", lines.join("\n")))
}

#[test]
fn notes_are_ordered_newest_first_by_time_then_id() {
    let mut n1 = note(1, NoteKind::Progress, "一");
    let mut n2 = note(2, NoteKind::Progress, "二");
    let mut n3 = note(3, NoteKind::Progress, "三");
    let mut n4 = note(4, NoteKind::Progress, "四");
    // Creation time decides, not the id; equal times put the larger id first.
    n1.created_at = at(5);
    n2.created_at = at(3);
    n3.created_at = at(5);
    n4.created_at = at(1);
    let notes = vec![n1, n2, n3, n4];
    let ids: Vec<i64> = notes_newest_first(&notes).iter().map(|n| n.id).collect();
    assert_eq!(ids, [3, 1, 2, 4]);
    // The input (the shared read API's order) is untouched.
    let input: Vec<i64> = notes.iter().map(|n| n.id).collect();
    assert_eq!(input, [1, 2, 3, 4]);
}

#[test]
fn the_card_screen_shows_the_newest_note_first_with_its_markers() {
    let comment = |id: i64, body: &str| {
        let mut n = note(id, NoteKind::Comment, body);
        n.author = "user".into();
        n
    };
    let mut decision = note(2, NoteKind::Decision, "改用滑動視窗");
    decision.because = vec![Cause {
        kind: CauseKind::User,
        id: None,
        card_seq: None,
        card_title: None,
    }];
    let mut d = detail(
        3,
        "內容",
        vec![
            note(1, NoteKind::Progress, "開始"),
            decision,
            note(3, NoteKind::Reply, "好"),
            comment(4, "再加一個測試"),
        ],
        vec![],
    );
    d.unreplied_comments = vec![4];
    let mut app = app_with(demo_snapshot(), (100, 40));
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, d);
    let text = render_text(&app, 100, 40);
    let lines: Vec<&str> = text.lines().collect();
    // The count is unchanged.
    let t = notes_title(&lines, 4);
    // Newest first: the unreplied comment, the reply, the decision with its causes, then
    // the oldest progress note; one blank row between notes.
    assert!(lines[t + 2].ends_with("  留言   未回覆"), "{text}");
    assert_eq!(lines[t + 3], "      再加一個測試");
    assert_eq!(lines[t + 4], "");
    assert!(lines[t + 5].ends_with(" 回覆"), "{text}");
    assert!(
        lines[t + 8].ends_with(" 決定  依據 使用者的指示"),
        "{}",
        lines[t + 8]
    );
    assert_eq!(lines[t + 9], "      改用滑動視窗");
    assert!(lines[t + 11].ends_with(" 進度"), "{text}");
    assert_eq!(lines[t + 12], "      開始");
}

#[test]
fn the_notes_section_scrolls_from_the_newest_note() {
    let notes: Vec<_> = (1..=30)
        .map(|i| note(i, NoteKind::Progress, &format!("紀錄 {i}")))
        .collect();
    let mut app = app_with(demo_snapshot(), (80, 24));
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, detail(3, "內容", notes, vec![]));
    // 內容 → 連結 → 紀錄.
    press(&mut app, &[Key::Tab, Key::Tab]);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Notes);
    let text = render_text(&app, 80, 24);
    let lines: Vec<&str> = text.lines().collect();
    let t = notes_title(&lines, 30);
    assert!(
        lines[t].contains("1-"),
        "the range starts at the top: {}",
        lines[t]
    );
    assert!(lines[t + 2].ends_with(" 進度"), "{text}");
    assert_eq!(lines[t + 3], "      紀錄 30");
    // One line down: the newest note's header leaves the top.
    press(&mut app, &[Key::Down]);
    assert_eq!(app.card.as_ref().unwrap().notes_scroll, 1);
    let text = render_text(&app, 80, 24);
    let lines: Vec<&str> = text.lines().collect();
    let t = notes_title(&lines, 30);
    assert!(lines[t].contains("2-"), "{}", lines[t]);
    assert_eq!(lines[t + 2], "      紀錄 30");
    assert_eq!(lines[t + 3], "");
    assert!(lines[t + 4].ends_with(" 進度"), "{text}");
    assert_eq!(lines[t + 5], "      紀錄 29");
}

#[test]
fn feed_line_of_the_turn_the_board_start_starts() {
    // docs/hooks.md section 7.2: the user.prompt of `kb board start`.
    let start = activity(
        9,
        "user.prompt",
        json!({"excerpt": "kb board start", "source": "board.start"}),
    );
    assert_eq!(activity_text(&start), "看板開始，這一輪起檢查守則");
    let prompt = activity(10, "user.prompt", json!({"excerpt": "幫我修正登入逾時"}));
    assert_eq!(activity_text(&prompt), "使用者：幫我修正登入逾時");
}

#[test]
fn feed_line_of_r11() {
    let warn = activity(
        9,
        "check.warn",
        json!({"rule": "R11", "message": "#7 已經結束，提問 19 仍然待回答"}),
    );
    assert_eq!(
        activity_text(&warn),
        "守則 R11 警告：#7 已經結束，提問 19 仍然待回答"
    );
}
