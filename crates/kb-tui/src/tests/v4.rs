//! Typing an answer in a question's input row (docs/board-v2.md section 7.1), and `Esc` on
//! the board clearing the epic selection (5.7).

use kb_core::model::QuestionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::v2_state::v2_snapshot;
use super::*;
use crate::app::{EpicKey, MSG_MARK_FIRST};
use crate::layout::{Section, TEXT_LIMIT};
use crate::run::render_text;
use crate::text::width;
use crate::view;

const SIZE: (u16, u16) = (100, 40);

fn buffer(app: &App, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    view::render(app, area, &mut buf);
    buf
}

/// Card #3 with an open single-choice question 7 (options 71, 72) and an open multi
/// question 8 (options 81, 82, 83). Choice indexes: question 7 has 0, 1, chat 2, input 3;
/// question 8 has 0, 1, 2, chat 3, input 4.
fn detail_q() -> CardDetail {
    let mut d = detail(3, "內容", vec![], vec![]);
    d.questions = vec![
        question(7, 3, "token 的有效期？", false, &["15 分鐘", "30 分鐘"]),
        question(8, 3, "哪些守則維持阻擋？", true, &["R1", "R2", "R4"]),
    ];
    d
}

fn open(app: &mut App) {
    press(app, &[Key::Right, Key::Enter]);
    deliver_card(app, detail_q());
}

/// Opens the card and puts the cursor on question 7's input row.
fn on_input() -> App {
    let mut app = app_with(v2_snapshot(), SIZE);
    open(&mut app);
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(app.card.as_ref().unwrap().q_cursor, Some((7, 3)));
    assert_eq!(app.input_target(), Some(7));
    app
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        app.update(Event::Key(Key::Char(c)));
    }
}

fn draft(app: &App, q: i64) -> (String, usize) {
    app.marks
        .get(&q)
        .map(|m| (m.text.clone(), m.text_cursor))
        .unwrap_or_default()
}

// ----- the key table ----------------------------------------------------------------

#[test]
fn printable_characters_including_command_letters_and_space_are_typed() {
    let mut app = on_input();
    // q a e s h j k l ? and Space type; nothing quits, opens or moves.
    typed(&mut app, "qaeshjkl? ");
    assert_eq!(draft(&app, 7), ("qaeshjkl? ".to_string(), 10));
    assert!(!app.quit);
    assert!(app.overlay.is_none(), "? typed, no help");
    assert!(app.card.is_some());
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Questions);
    assert!(app.marks[&7].options.is_empty(), "Space marks nothing");
    // Chinese, full-width punctuation and other scalars an IME commits.
    typed(&mut app, "中文，「好」！é😀");
    assert_eq!(draft(&app, 7).0, "qaeshjkl? 中文，「好」！é😀");
    // Control characters are not text.
    app.update(Event::Key(Key::Char('\u{7}')));
    assert_eq!(draft(&app, 7).0.chars().count(), 19);
}

#[test]
fn backspace_delete_and_cursor_keys_work_by_characters() {
    let mut app = on_input();
    typed(&mut app, "ab中文cd");
    assert_eq!(draft(&app, 7), ("ab中文cd".to_string(), 6));
    press(&mut app, &[Key::Left, Key::Left]);
    assert_eq!(draft(&app, 7).1, 4, "before c");
    press(&mut app, &[Key::Backspace]);
    assert_eq!(
        draft(&app, 7),
        ("ab中cd".to_string(), 3),
        "a whole Chinese character"
    );
    press(&mut app, &[Key::Delete]);
    assert_eq!(draft(&app, 7), ("ab中d".to_string(), 3));
    press(&mut app, &[Key::Home]);
    assert_eq!(draft(&app, 7).1, 0);
    press(&mut app, &[Key::Backspace, Key::Left]);
    assert_eq!(
        draft(&app, 7),
        ("ab中d".to_string(), 0),
        "nothing before the start"
    );
    typed(&mut app, "字");
    assert_eq!(draft(&app, 7), ("字ab中d".to_string(), 1));
    press(&mut app, &[Key::End, Key::Delete, Key::Right]);
    assert_eq!(
        draft(&app, 7),
        ("字ab中d".to_string(), 5),
        "nothing after the end"
    );
    // ← and Backspace never leave the card while typing.
    press(&mut app, &[Key::Home, Key::Left, Key::Backspace]);
    assert!(app.card.is_some() && app.history.is_empty());
    // Ctrl+U clears everything.
    press(&mut app, &[Key::CtrlU]);
    assert_eq!(draft(&app, 7), (String::new(), 0));
    assert!(!app.marks.contains_key(&7));
}

#[test]
fn arrows_tab_enter_esc_and_ctrl_c_keep_their_meaning() {
    let mut app = on_input();
    typed(&mut app, "先這樣");
    // ↑ ↓ leave the row.
    press(&mut app, &[Key::Up]);
    assert_eq!(app.card.as_ref().unwrap().q_cursor, Some((7, 2)));
    press(&mut app, &[Key::Down, Key::Down]);
    assert_eq!(app.card.as_ref().unwrap().q_cursor, Some((8, 0)));
    press(&mut app, &[Key::Up]);
    assert_eq!(app.input_target(), Some(7));
    // Tab / Shift+Tab move the focus.
    press(&mut app, &[Key::Tab]);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Body);
    assert_eq!(app.input_target(), None);
    typed(&mut app, "q");
    assert!(app.quit, "off the input row, q quits again");
    app.quit = false;
    press(&mut app, &[Key::BackTab]);
    assert_eq!(app.input_target(), Some(7));
    // Esc: straight to the board; the draft stays.
    press(&mut app, &[Key::Esc]);
    assert!(app.card.is_none());
    assert_eq!(draft(&app, 7).0, "先這樣");
    // Ctrl+C quits even on the input row.
    let mut app = on_input();
    press(&mut app, &[Key::CtrlC]);
    assert!(app.quit);
}

#[test]
fn enter_on_the_input_row_sends_the_text_with_the_marked_options() {
    // Text only.
    let mut app = on_input();
    typed(&mut app, "  refresh 的間隔也要一起改  ");
    let fx = press(&mut app, &[Key::Enter]);
    match &fx[..] {
        [Effect::Answer(r)] => {
            assert_eq!(r.question_id, 7);
            assert!(r.option_ids.is_empty() && !r.chat);
            assert_eq!(
                r.text.as_deref(),
                Some("refresh 的間隔也要一起改"),
                "trimmed"
            );
        }
        other => panic!("{other:?}"),
    }
    // Options and text together, sent from an option row too.
    let mut app = on_input();
    typed(&mut app, "補充");
    press(&mut app, &[Key::Up, Key::Up, Key::Char(' ')]); // 30 分鐘
    let fx = press(&mut app, &[Key::Enter]);
    match &fx[..] {
        [Effect::Answer(r)] => {
            assert_eq!(r.option_ids, vec![72]);
            assert_eq!(r.text.as_deref(), Some("補充"));
        }
        other => panic!("{other:?}"),
    }
    // Nothing marked and only spaces: not sent, the new hint.
    let mut app = on_input();
    typed(&mut app, "   ");
    assert!(press(&mut app, &[Key::Enter]).is_empty());
    assert_eq!(app.flash.as_deref(), Some(MSG_MARK_FIRST));
    assert_eq!(MSG_MARK_FIRST, "先勾選選項或輸入文字");
    // After a successful answer the draft is gone.
    let mut app = on_input();
    typed(&mut app, "好");
    press(&mut app, &[Key::Enter]);
    app.update(Event::Answered {
        question_id: 7,
        error: None,
    });
    assert!(!app.marks.contains_key(&7));
}

#[test]
fn the_text_limit_counts_characters_and_paste_turns_newlines_into_spaces() {
    let mut app = on_input();
    typed(&mut app, &"中".repeat(TEXT_LIMIT - 1));
    typed(&mut app, "ab");
    let (text, cursor) = draft(&app, 7);
    assert_eq!(text.chars().count(), TEXT_LIMIT);
    assert_eq!(cursor, TEXT_LIMIT);
    assert!(text.ends_with("中a"), "the 501st character is refused");
    // Paste: line breaks become spaces, the cut is at the limit.
    let mut app = on_input();
    typed(&mut app, "前");
    press(&mut app, &[Key::Home]);
    app.update(Event::Paste("第一行\r\n第二行\n第三行\t完".into()));
    assert_eq!(
        draft(&app, 7),
        ("第一行 第二行 第三行 完前".to_string(), 13)
    );
    let mut app = on_input();
    app.update(Event::Paste("x".repeat(600)));
    assert_eq!(draft(&app, 7).0.chars().count(), TEXT_LIMIT);
    // Paste off the input row is ignored.
    let mut app = app_with(v2_snapshot(), SIZE);
    open(&mut app);
    app.update(Event::Paste("沒有用".into()));
    assert!(app.marks.is_empty());
}

#[test]
fn marking_session_reply_disables_the_input_row() {
    let mut app = on_input();
    typed(&mut app, "草稿");
    press(&mut app, &[Key::Up, Key::Char(' ')]); // 在 session 裡說明
    assert!(app.marks[&7].chat);
    // The cursor cannot reach the input row now: ↓ goes to the next question.
    press(&mut app, &[Key::Down]);
    assert_eq!(app.card.as_ref().unwrap().q_cursor, Some((8, 0)));
    // Enter sends chat without the text.
    press(&mut app, &[Key::Up]);
    let fx = press(&mut app, &[Key::Enter]);
    match &fx[..] {
        [Effect::Answer(r)] => assert!(r.chat && r.text.is_none()),
        other => panic!("{other:?}"),
    }
    let text = render_text(&app, 100, 40);
    assert!(text.contains("    輸入：（改在 session 裡說明）"), "{text}");
    // Marking an option instead brings the row back, with the draft.
    press(&mut app, &[Key::Up, Key::Char(' '), Key::Down, Key::Down]);
    assert_eq!(app.input_target(), Some(7));
    assert_eq!(draft(&app, 7).0, "草稿");
}

#[test]
fn drafts_survive_reloads_and_leaving_the_card_and_clear_when_closed() {
    let mut app = on_input();
    typed(&mut app, "還沒送出");
    deliver_card(&mut app, detail_q());
    assert_eq!(draft(&app, 7), ("還沒送出".to_string(), 4));
    assert_eq!(
        app.input_target(),
        Some(7),
        "the cursor stays on the input row"
    );
    // Leave the card and come back.
    press(&mut app, &[Key::Esc]);
    press(&mut app, &[Key::Enter]);
    deliver_card(&mut app, detail_q());
    assert_eq!(draft(&app, 7).0, "還沒送出");
    // The question stops being open: the draft goes.
    let mut d = detail_q();
    d.questions[0].question.status = QuestionStatus::Cancelled;
    deliver_card(&mut app, d);
    assert!(!app.marks.contains_key(&7));
}

// ----- rendering ----------------------------------------------------------------------

/// Lines of the question section, without the section title.
fn question_rows(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.contains("提問 7："))
        .take_while(|l| !l.starts_with("  內容"))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_input_row_renders_placeholder_cursor_and_wrapped_text() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open(&mut app);
    let rows = question_rows(&render_text(&app, 100, 40));
    // Off the cursor and empty: the dim placeholder after 「在 session 裡說明」.
    assert_eq!(rows[3], "    ( ) 在 session 裡說明");
    assert_eq!(rows[4], "    輸入：（按 i 或移到這裡打字）");
    let buf = buffer(&app, 100, 40);
    let y = render_text(&app, 100, 40)
        .lines()
        .position(|l| l == "    輸入：（按 i 或移到這裡打字）")
        .unwrap() as u16;
    assert_eq!(buf[(4, y)].fg, Color::DarkGray);
    // On the cursor: ▶ at the side margin, the label in reverse video, the text cursor.
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    let text = render_text(&app, 100, 40);
    assert!(text.contains("\n  ▶ 輸入：▏\n"), "{text}");
    typed(&mut app, "先維持 R4，其他的等試用之後再決定");
    press(&mut app, &[Key::Left, Key::Left]);
    let text = render_text(&app, 100, 40);
    assert!(
        text.contains("\n  ▶ 輸入：先維持 R4，其他的等試用之後再▏決定\n"),
        "{text}"
    );
    let buf = buffer(&app, 100, 40);
    let y = text
        .lines()
        .position(|l| l.starts_with("  ▶ 輸入："))
        .unwrap() as u16;
    assert!(buf[(4, y)].modifier.contains(Modifier::REVERSED));
    assert!(
        !buf[(10, y)].modifier.contains(Modifier::REVERSED),
        "text is not reversed"
    );
    // The key hints on the input row.
    assert!(
        text.lines()
            .last()
            .unwrap()
            .starts_with("  打字輸入  Enter 送出  ↑↓ 離開輸入  Esc 看板"),
        "{text}"
    );

    // Long text wraps; continuation rows align with the first text cell (column 10).
    let mut app = app_with(v2_snapshot(), (60, 40));
    open(&mut app);
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    let long = "中文與 English 混合的一段很長的回答，要換行才放得下，而且每一列都要和第一列的文字對齊才好讀";
    typed(&mut app, long);
    let text = render_text(&app, 60, 40);
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("  ▶ 輸入："))
        .take_while(|l| l.starts_with("  ▶") || l.starts_with("          "))
        .collect();
    assert!(rows.len() >= 2, "{text}");
    for r in &rows {
        assert!(width(r) <= 58, "within the margins: {r}");
    }
    for r in &rows[1..] {
        assert!(
            r.starts_with("          ") && !r.starts_with("           "),
            "{r}"
        );
    }
    let joined: String = rows
        .iter()
        .map(|r| r.trim_start_matches("  ▶ 輸入：").trim_start())
        .collect();
    assert_eq!(joined, format!("{long}▏"));
}

#[test]
fn answered_questions_show_their_text_after_the_options() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open(&mut app);
    let mut d = detail_q();
    d.questions[0].question.status = QuestionStatus::Answered;
    d.questions[0].options[0].selected = true;
    d.questions[0].question.answer_text = Some("但是要標記為過時".into());
    d.questions[1].question.status = QuestionStatus::Answered;
    d.questions[1].question.answer_text = Some("先不要，等這一輪結束再說".into());
    deliver_card(&mut app, d);
    let text = render_text(&app, 100, 40);
    assert!(
        text.contains("    提問 7：token 的有效期？  已回答：15 分鐘｜但是要標記為過時"),
        "{text}"
    );
    assert!(
        text.contains("    提問 8：哪些守則維持阻擋？  已回答：先不要，等這一輪結束再說"),
        "{text}"
    );
    // Too long: cut with …
    let text = render_text(&app, 60, 40);
    let line = text.lines().find(|l| l.contains("提問 7：")).unwrap();
    assert!(line.ends_with('…') && width(line) <= 58, "{line}");
    // The activity sentence.
    let a = activity(
        1,
        "question.answer",
        serde_json::json!({"seq": 5, "labels": ["15 分鐘"], "text": "refresh 的間隔也要一起改"}),
    );
    assert_eq!(
        crate::text::activity_text(&a),
        "#5 回答：15 分鐘｜refresh 的間隔也要一起改"
    );
    let a = activity(
        2,
        "question.answer",
        serde_json::json!({"seq": 7, "labels": [], "text": "先不要"}),
    );
    assert_eq!(crate::text::activity_text(&a), "#7 回答：先不要");
}

// ----- Esc on the board clears the epic selection (5.7) -------------------------------

#[test]
fn esc_on_the_board_clears_the_epic_selection() {
    // From the side panel.
    let mut app = app_with(v2_snapshot(), (120, 30));
    press(&mut app, &[Key::Left, Key::Down]);
    assert!(app.side_focused());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
    press(&mut app, &[Key::Esc]);
    assert_eq!(app.sel_epic, None);
    assert!(!app.side_focused(), "the focus goes back to the lanes");
    assert_eq!((app.sel_lane, selected_seq(&app)), (0, Some(1)));
    // From the lanes, with the selection kept after →.
    press(&mut app, &[Key::Left, Key::Right]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(1)));
    assert!(!app.side_focused());
    let text = render_text(&app, 120, 30);
    assert!(text.contains('◆'));
    press(&mut app, &[Key::Esc]);
    assert_eq!(app.sel_epic, None);
    // The marks and the dimming are gone.
    let text = render_text(&app, 120, 30);
    assert!(!text.contains('◆'), "{text}");
    let buf = buffer(&app, 120, 30);
    let (x, y) = (0..30)
        .find_map(|y| {
            (0..120)
                .find(|&x| buf[(x, y)].symbol() == "#" && buf[(x + 1, y)].symbol() == "2")
                .map(|x| (x, y))
        })
        .unwrap();
    assert_ne!(buf[(x, y)].fg, Color::DarkGray, "#2 is not dim any more");
    // With nothing selected, Esc does nothing.
    let before = (app.sel_lane, selected_seq(&app), app.quit);
    press(&mut app, &[Key::Esc]);
    assert_eq!((app.sel_lane, selected_seq(&app), app.quit), before);
    assert!(app.card.is_none() && app.overlay.is_none());
    // `e` selections are cleared the same way.
    press(&mut app, &[Key::Char('e'), Key::Esc]);
    assert_eq!(app.sel_epic, None);
}

#[test]
fn the_key_bars_offer_esc_while_an_epic_is_selected_within_80_columns() {
    let mut app = app_with(v2_snapshot(), (80, 30));
    let bar = |app: &App| render_text(app, 80, 30).lines().last().unwrap().to_string();
    assert!(!bar(&app).contains("Esc"), "{}", bar(&app));
    press(&mut app, &[Key::Char('e')]);
    let lanes = bar(&app);
    assert_eq!(
        lanes,
        " ←→ 泳道  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  Esc 取消 epic  s 側欄  ?  q 離開"
    );
    assert!(width(&lanes) <= 80);
    press(&mut app, &[Key::Left]);
    let side = bar(&app);
    assert_eq!(
        side,
        " ↑↓ 選 epic  Enter 開啟  → 泳道  e epic  Esc 取消 epic  s 側欄  w b ?  q 離開"
    );
    assert!(width(&side) <= 80);
    // The help screen lists it.
    press(&mut app, &[Key::Char('?')]);
    assert!(render_text(&app, 80, 24).contains("取消 epic 的選取"));
}

#[test]
fn the_tui_keeps_the_snapshot_order_whatever_the_priorities() {
    // docs/board-v2.md section 10.2: the snapshot is already in display order.
    let mut c1 = card(1, 0, "第一", 3);
    let mut c2 = card(2, 0, "第二", 0);
    for c in [&mut c1, &mut c2] {
        c.card.epic_id = Some(1);
    }
    let mut snap = snapshot(
        "",
        vec![
            (
                kb_core::model::Lane {
                    ..lane(1, 1, "待辦", kb_core::model::LaneKind::Todo)
                },
                vec![c1, c2],
            ),
            (lane(2, 2, "完成", kb_core::model::LaneKind::Done), vec![]),
        ],
    );
    snap.epics = vec![epic(1, "E", None, counts(2, 0, 0))];
    let app = app_with(snap, (120, 30));
    assert_eq!(
        selected_seq(&app),
        Some(1),
        "the first card, not the P0 one"
    );
    let order: Vec<i32> =
        crate::app::epic_cards(&app.data.as_ref().unwrap().snapshot, EpicKey::Epic(1))
            .iter()
            .map(|c| c.card.seq)
            .collect();
    assert_eq!(order, vec![1, 2]);
    let text = render_text(&app, 120, 30);
    assert!(text.find("#1 第一").unwrap() < text.find("#2 第二").unwrap());
}
