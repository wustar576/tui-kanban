//! Comments from the human and the screen part of section 13 (docs/board-v2.md sections
//! 13.15 to 13.20): the comment row on every screen, its keys and sending, the 留言 tag,
//! note kind and header count, the feed lines, `i` in the question section, the key
//! hints, the help screen, no more `hc send`, and the narrow write scope.

use kb_core::model::{BoardStatus, NoteKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::v2_state::v2_snapshot;
use super::*;
use crate::app::{
    COMMENT_LIMIT, CommentRequest, EpicKey, MSG_ARCHIVED_NO_COMMENT, MSG_COMMENT_SENT, MSG_SENDING,
    MSG_SENT, MSG_TYPE_FIRST,
};
use crate::layout::{self, Section};
use crate::run::render_text;
use crate::text::{activity_text, width};
use crate::view;

const SIZE: (u16, u16) = (120, 30);
const COMMENT_HINTS: &str = "打字輸入  Enter 送出  Esc 取消";

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

fn typed(app: &mut App, s: &str) -> Vec<Effect> {
    let mut fx = Vec::new();
    for c in s.chars() {
        fx.extend(app.update(Event::Key(Key::Char(c))));
    }
    fx
}

/// (card id, seq, text, cursor, sending) of the open comment row.
fn row(app: &App) -> Option<(i64, i32, String, usize, bool)> {
    app.comment
        .as_ref()
        .map(|c| (c.card_id, c.seq, c.text.clone(), c.cursor, c.sending))
}

fn lines(app: &App, w: u16, h: u16) -> Vec<String> {
    render_text(app, w, h).lines().map(str::to_string).collect()
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

/// Opens card #3 (處理中) with its two open questions: the focus is on the question
/// section, the cursor on question 7's first option.
fn card_with_questions(size: (u16, u16)) -> App {
    let mut app = app_with(v2_snapshot(), size);
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, detail_q());
    let v = app.card.as_ref().unwrap();
    assert_eq!((v.focus, v.q_cursor), (Section::Questions, Some((7, 0))));
    app
}

fn cursor(app: &App) -> Option<(i64, usize)> {
    app.card.as_ref().and_then(|v| v.q_cursor)
}

// ----- A: the comment row ---------------------------------------------------------------

#[test]
fn i_on_the_board_opens_the_comment_row_where_command_keys_are_text() {
    let mut app = app_with(v2_snapshot(), SIZE);
    assert_eq!(selected_seq(&app), Some(1));
    assert!(press(&mut app, &[Key::Char('i')]).is_empty());
    assert_eq!(row(&app), Some((101, 1, String::new(), 0, false)));
    // q a e s h j k l ? w b i and Space type; nothing quits, moves, opens or toggles.
    typed(&mut app, "qaeshjkl?wbi 中文，好");
    assert_eq!(row(&app).unwrap().2, "qaeshjkl?wbi 中文，好");
    assert!(!app.quit);
    assert!(app.overlay.is_none() && app.card.is_none() && app.epic.is_none());
    assert_eq!(app.sel_epic, None, "e did not select an epic");
    assert!(app.show_side, "s did not hide the side panel");
    assert_eq!((app.sel_lane, selected_seq(&app)), (0, Some(1)));
    // Other keys do nothing: ↑ ↓ Tab PageDown.
    press(&mut app, &[Key::Up, Key::Down, Key::Tab, Key::PageDown]);
    assert_eq!((app.sel_lane, selected_seq(&app)), (0, Some(1)));
    assert_eq!(row(&app).unwrap().2, "qaeshjkl?wbi 中文，好");
    // Control characters are not text.
    press(&mut app, &[Key::Char('\u{7}')]);
    assert_eq!(row(&app).unwrap().2.chars().count(), 17);
}

#[test]
fn the_comment_row_uses_the_editing_keys_of_the_input_row() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "ab中文cd");
    assert_eq!(row(&app).unwrap().3, 6);
    press(&mut app, &[Key::Left, Key::Left, Key::Backspace]);
    let r = row(&app).unwrap();
    assert_eq!(
        (r.2.as_str(), r.3),
        ("ab中cd", 3),
        "a whole Chinese character"
    );
    press(&mut app, &[Key::Delete]);
    assert_eq!(row(&app).unwrap().2, "ab中d");
    press(&mut app, &[Key::Home, Key::Backspace, Key::Left]);
    assert_eq!(row(&app).unwrap().3, 0, "nothing before the start");
    typed(&mut app, "字");
    press(&mut app, &[Key::End, Key::Delete, Key::Right]);
    let r = row(&app).unwrap();
    assert_eq!((r.2.as_str(), r.3), ("字ab中d", 5), "nothing after the end");
    press(&mut app, &[Key::CtrlU]);
    let r = row(&app).unwrap();
    assert_eq!((r.2.as_str(), r.3), ("", 0));
    assert!(app.comment.is_some(), "Ctrl+U keeps the row open");
}

#[test]
fn esc_cancels_and_keeps_nothing_and_ctrl_c_quits() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "草稿");
    assert!(press(&mut app, &[Key::Esc]).is_empty());
    assert!(app.comment.is_none());
    assert_eq!(app.sel_epic, None, "Esc only closed the row");
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(row(&app).unwrap().2, "", "the text is gone");
    typed(&mut app, "x");
    press(&mut app, &[Key::CtrlC]);
    assert!(app.quit);
}

#[test]
fn paste_turns_newlines_into_spaces_and_the_limit_is_500_characters() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "前");
    press(&mut app, &[Key::Home]);
    app.update(Event::Paste("第一行\r\n第二行\n第三行\t完".into()));
    let r = row(&app).unwrap();
    assert_eq!((r.2.as_str(), r.3), ("第一行 第二行 第三行 完前", 13));
    assert_eq!(COMMENT_LIMIT, 500);
    app.update(Event::Paste("x".repeat(600)));
    assert_eq!(row(&app).unwrap().2.chars().count(), COMMENT_LIMIT);
    typed(&mut app, "中");
    assert_eq!(
        row(&app).unwrap().2.chars().count(),
        COMMENT_LIMIT,
        "the 501st character is refused"
    );
    // Without the row (and off an input row) paste does nothing.
    let mut app = app_with(v2_snapshot(), SIZE);
    app.update(Event::Paste("沒有用".into()));
    assert!(app.comment.is_none() && app.marks.is_empty());
}

#[test]
fn enter_sends_the_comment_and_success_closes_the_row() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "  這張先不要做，先查 log ");
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::Comment(CommentRequest {
            board_id: BOARD_ID,
            card_id: 101,
            seq: 1,
            text: "  這張先不要做，先查 log ".into(),
        })]
    );
    assert!(row(&app).unwrap().4, "sending");
    assert_eq!(app.flash.as_deref(), Some(MSG_SENDING));
    // While sending, keys do nothing and the message stays; Ctrl+C still quits.
    assert!(press(&mut app, &[Key::Enter, Key::Char('x'), Key::Esc]).is_empty());
    assert_eq!(row(&app).unwrap().2, "  這張先不要做，先查 log ");
    assert_eq!(app.flash.as_deref(), Some(MSG_SENDING));
    app.update(Event::Paste("貼上".into()));
    assert_eq!(row(&app).unwrap().2, "  這張先不要做，先查 log ");

    let fx = app.update(Event::Commented {
        card_id: 101,
        error: None,
    });
    assert!(app.comment.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_COMMENT_SENT));
    assert_eq!(MSG_COMMENT_SENT, "留言已送出");
    assert_eq!(fx, vec![Effect::LoadBoard(BoardTarget::Id(BOARD_ID))]);
    // The next key clears the message and works as a command again.
    press(&mut app, &[Key::Right]);
    assert_eq!((app.flash.as_deref(), selected_seq(&app)), (None, Some(3)));
}

#[test]
fn empty_text_is_not_sent() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    assert!(press(&mut app, &[Key::Enter]).is_empty());
    assert_eq!(app.flash.as_deref(), Some(MSG_TYPE_FIRST));
    assert_eq!(MSG_TYPE_FIRST, "先輸入文字");
    assert!(app.comment.is_some(), "the row stays open");
    // Spaces, including the ideographic space, are not text either.
    typed(&mut app, " \u{3000} ");
    assert!(press(&mut app, &[Key::Enter]).is_empty());
    assert_eq!(app.flash.as_deref(), Some(MSG_TYPE_FIRST));
    assert!(!row(&app).unwrap().4, "not sending");
    // Typing clears the message.
    typed(&mut app, "好");
    assert_eq!(app.flash, None);
}

#[test]
fn an_error_keeps_the_text_so_it_can_be_fixed_and_sent_again() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "先查 log");
    press(&mut app, &[Key::Enter]);
    let fx = app.update(Event::Commented {
        card_id: 101,
        error: Some("無法連線到資料庫".into()),
    });
    assert!(fx.contains(&Effect::LoadBoard(BoardTarget::Id(BOARD_ID))));
    assert_eq!(app.flash.as_deref(), Some("無法連線到資料庫"));
    assert_eq!(
        row(&app),
        Some((101, 1, "先查 log".into(), 6, false)),
        "open, the text kept, not sending"
    );
    // The error shows on the board's bottom row; the comment row stays above it.
    let l = lines(&app, 120, 30);
    assert_eq!(l[29], " 無法連線到資料庫");
    assert_eq!(l[28], " 留言 #1：先查 log▏");
    typed(&mut app, "，再說");
    let fx = press(&mut app, &[Key::Enter]);
    match &fx[..] {
        [Effect::Comment(r)] => assert_eq!(r.text, "先查 log，再說"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_archived_board_does_not_open_the_comment_row() {
    let mut snap = v2_snapshot();
    snap.board.status = BoardStatus::Archived;
    let mut app = app_with(snap, SIZE);
    assert!(press(&mut app, &[Key::Char('i')]).is_empty());
    assert!(app.comment.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_ARCHIVED_NO_COMMENT));
    assert_eq!(MSG_ARCHIVED_NO_COMMENT, "看板已封存，不能留言");
    assert_eq!(lines(&app, 120, 30)[29], " 看板已封存，不能留言");
    // The card screen too (focus not on the questions).
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, detail(3, "內容", vec![], vec![]));
    press(&mut app, &[Key::Char('i')]);
    assert!(app.comment.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_ARCHIVED_NO_COMMENT));
}

#[test]
fn i_on_an_empty_lane_does_nothing() {
    let mut app = app_with(demo_snapshot(), SIZE);
    // 捨棄 is empty.
    press(&mut app, &[Key::End, Key::Right, Key::Right, Key::Right]);
    assert_eq!(selected_seq(&app), None);
    press(&mut app, &[Key::Char('i')]);
    assert!(app.comment.is_none() && app.flash.is_none());
}

#[test]
fn i_on_the_epic_screen_and_with_the_side_panel_focused() {
    // The side panel has the focus: `i` works as on the board (section 5.7).
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Left]);
    assert!(app.side_focused());
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(row(&app).map(|r| (r.0, r.1)), Some((101, 1)));
    press(&mut app, &[Key::Esc]);
    assert!(app.side_focused(), "Esc closed only the row");

    // Epic 1 「P1 看板」: #1, #3. ↓ selects #3.
    press(&mut app, &[Key::Enter, Key::Down]);
    assert_eq!(app.epic.as_ref().unwrap().key, EpicKey::Epic(1));
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(row(&app).map(|r| (r.0, r.1)), Some((103, 3)));
    typed(&mut app, "jk");
    assert_eq!(app.epic.as_ref().unwrap().sel, 1, "j k are text");
    let fx = press(&mut app, &[Key::Enter]);
    assert!(
        matches!(&fx[..], [Effect::Comment(r)] if r.card_id == 103 && r.text == "jk"),
        "{fx:?}"
    );
    app.update(Event::Commented {
        card_id: 103,
        error: None,
    });
    assert!(app.comment.is_none() && app.epic.is_some());
    press(&mut app, &[Key::Esc]);
    assert!(
        app.epic.is_none(),
        "Esc works as before once the row is closed"
    );
}

#[test]
fn i_on_the_card_screen_comments_on_the_open_card() {
    // Before the detail arrives, the card id comes from the board.
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Right, Key::Enter]);
    assert!(app.card.as_ref().unwrap().detail.is_none());
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(row(&app).map(|r| (r.0, r.1)), Some((103, 3)));
    press(&mut app, &[Key::Esc]);
    assert!(app.card.is_some(), "Esc closed only the row");

    // Focus on the body, links or notes: `i` comments.
    let mut app = card_with_questions(SIZE);
    for focus in [Section::Body, Section::Links, Section::Notes] {
        press(&mut app, &[Key::Tab]);
        assert_eq!(app.card.as_ref().unwrap().focus, focus);
        press(&mut app, &[Key::Char('i')]);
        assert_eq!(row(&app).map(|r| (r.0, r.1)), Some((103, 3)), "{focus:?}");
        // ← and Backspace edit; they do not leave the card.
        typed(&mut app, "ab");
        press(&mut app, &[Key::Left, Key::Backspace]);
        assert_eq!(row(&app).unwrap().2, "b");
        assert!(app.card.is_some() && app.history.is_empty());
        press(&mut app, &[Key::Esc]);
        assert!(app.card.is_some());
    }
    // The question section with no open question: `i` comments too.
    let mut d = detail_q();
    for q in &mut d.questions {
        q.question.status = kb_core::model::QuestionStatus::Cancelled;
    }
    deliver_card(&mut app, d);
    press(&mut app, &[Key::Tab]);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Questions);
    assert_eq!(cursor(&app), None);
    press(&mut app, &[Key::Char('i')]);
    assert!(app.comment.is_some());
}

#[test]
fn a_reload_while_typing_keeps_the_row_and_its_text() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "還在打");
    let mut snap = v2_snapshot();
    snap.lanes[0].cards[0].note_count = 4;
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
    assert_eq!(row(&app).unwrap().2, "還在打");
    // Another board (the latest one changed): the row goes with the old board.
    let mut other = v2_snapshot();
    other.board.id = 2;
    app.want = None;
    app.update(Event::BoardLoaded {
        target: BoardTarget::Latest(Scope::All),
        data: Some(data(other)),
        at: Instant::now(),
    });
    assert_eq!(app.data.as_ref().unwrap().id(), 2);
    assert!(app.comment.is_none());
}

// ----- A: rendering of the comment row --------------------------------------------------

#[test]
fn the_board_shows_the_comment_row_above_its_hints() {
    let mut app = app_with(v2_snapshot(), SIZE);
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "這張先不要做");
    press(&mut app, &[Key::Left]);
    let l = lines(&app, 120, 30);
    // The last activity row gives way to the comment row; the hints change.
    assert_eq!(l[28], " 留言 #1：這張先不要▏做");
    assert_eq!(l[29], format!(" {COMMENT_HINTS}"));
    assert!(l[26].contains("#3 紀錄（決定）改用滑動視窗"), "{}", l[26]);
    let buf = buffer(&app, 120, 30);
    assert!(buf[(1, 28)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(1, 28)].fg, layout::COMMENT_COLOR, "the label is blue");
    let (x, _) = find_any(&buf, "▏").unwrap();
    assert_eq!(buf[(x, 28)].fg, Color::Yellow);
}

#[test]
fn the_card_and_epic_screens_show_the_row_above_the_hints_and_below_messages() {
    let mut app = card_with_questions((100, 40));
    press(&mut app, &[Key::Tab, Key::Char('i')]);
    typed(&mut app, "留言內容");
    let l = lines(&app, 100, 40);
    assert_eq!(l[39], format!("  {COMMENT_HINTS}"));
    assert_eq!(l[38], "  留言 #3：留言內容▏");
    // A message sits above the comment row.
    press(&mut app, &[Key::Enter]);
    let l = lines(&app, 100, 40);
    assert_eq!(l[37], format!("  {MSG_SENDING}"));
    assert_eq!(l[38], "  留言 #3：留言內容▏");

    // The epic screen.
    let mut app = app_with(v2_snapshot(), (88, 30));
    press(&mut app, &[Key::Left, Key::Enter, Key::Char('i')]);
    typed(&mut app, "好");
    let l = lines(&app, 88, 30);
    assert_eq!(l[29], format!("  {COMMENT_HINTS}"));
    assert_eq!(l[28], "  留言 #1：好▏");
    assert!(
        l.iter().any(|x| x.starts_with("  ▶ #1 ")),
        "the list is still there"
    );
}

#[test]
fn long_text_scrolls_sideways_and_keeps_the_cursor_visible() {
    // Unit: the window around the cursor.
    assert_eq!(
        layout::comment_view("abc", 1, 10),
        (false, "a".into(), "bc".into())
    );
    let long = "a".repeat(30);
    let (cut, before, after) = layout::comment_view(&long, 30, 10);
    assert!(cut);
    assert_eq!((before.as_str(), after.as_str()), ("aaaaaaaa", ""));
    let (cut, before, after) = layout::comment_view(&long, 0, 10);
    assert!(!cut);
    assert_eq!((before.as_str(), after.as_str()), ("", "aaaaaaaa…"));
    // Wide characters never overflow.
    let (cut, before, after) = layout::comment_view(&"中".repeat(20), 10, 11);
    assert!(cut);
    assert!(1 + width(&before) + 1 + width(&after) <= 11);
    assert!(after.ends_with('…'));

    // On the board at 60 columns: ` 留言 #1：` takes 10 cells, 50 are left.
    let mut app = app_with(v2_snapshot(), (60, 20));
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, &"a".repeat(100));
    let l = lines(&app, 60, 20);
    assert_eq!(l[18], format!(" 留言 #1：…{}▏", "a".repeat(48)));
    assert_eq!(width(&l[18]), 60);
    press(&mut app, &[Key::Home]);
    let l = lines(&app, 60, 20);
    assert_eq!(l[18], format!(" 留言 #1：▏{}…", "a".repeat(48)));
}

// ----- A: tags, header count, notes, feed ---------------------------------------------

#[test]
fn cards_with_unreplied_comments_show_a_blue_tag_that_keeps_the_border() {
    let mut snap = v2_snapshot();
    // #2: 留言, 阻塞, 衍生 — the border follows 阻塞 (red), not the 留言 tag.
    snap.lanes[0].cards[1].unreplied_comment_count = 1;
    // #3: 提問 then 留言.
    snap.lanes[1].cards[0].unreplied_comment_count = 2;
    snap.unreplied_comment_count = 3;
    let app = app_with(snap, SIZE);
    let text = render_text(&app, 120, 30);
    assert!(text.contains("P2 hook…   留言   阻塞  衍生"), "{text}");
    assert!(text.contains("P1 看板   提問   留言 "), "{text}");
    let buf = buffer(&app, 120, 30);
    let (x, y) = find_any(&buf, "留言   阻塞").unwrap();
    assert_eq!(
        (buf[(x, y)].fg, buf[(x, y)].bg),
        (Color::White, Color::Blue)
    );
    let (bx, by) = find_any(&buf, "#2 用真實").unwrap();
    assert_eq!(buf[(bx - 2, by)].fg, Color::Red, "border of #2 stays red");

    // A card whose only tag is 留言: a tags row appears, the border is the default.
    let mut snap = demo_snapshot();
    snap.lanes[0].cards[1].unreplied_comment_count = 1;
    let none = layout::CardJudge::default();
    let before = layout::card_height(&demo_snapshot().lanes[0].cards[1], 40, none);
    assert_eq!(
        layout::card_height(&snap.lanes[0].cards[1], 40, none),
        before + 1
    );
    let app = app_with(snap, SIZE);
    let buf = buffer(&app, 120, 30);
    let (x, y) = find_any(&buf, "#2 更新文件").unwrap();
    assert_eq!(buf[(x - 2, y)].fg, Color::Reset, "default border");
    assert!(find(&buf, y + 1, "留言").is_some());

    // The epic screen's row shows the tag.
    let mut snap = v2_snapshot();
    snap.lanes[1].cards[0].unreplied_comment_count = 1;
    let mut app = app_with(snap, SIZE);
    press(&mut app, &[Key::Left, Key::Enter]);
    let text = render_text(&app, 120, 30);
    let row3 = text.lines().find(|l| l.contains("#3 ")).unwrap();
    assert!(row3.ends_with("提問   留言"), "{row3}");
}

#[test]
fn the_header_counts_unreplied_comments_in_blue() {
    let mut snap = v2_snapshot();
    snap.unreplied_comment_count = 3;
    let app = app_with(snap, SIZE);
    let l = lines(&app, 120, 30);
    assert!(
        l[0].ends_with("提問 3  討論 1  留言 3  ● 已連線"),
        "{}",
        l[0]
    );
    let buf = buffer(&app, 120, 30);
    let x = find(&buf, 0, "留言 3").unwrap();
    assert_eq!(buf[(x, 0)].fg, layout::COMMENT_COLOR);
    assert!(buf[(x, 0)].modifier.contains(Modifier::BOLD));
    // Zero: not shown.
    let app = app_with(v2_snapshot(), SIZE);
    assert!(!lines(&app, 120, 30)[0].contains("留言"));
}

#[test]
fn a_change_of_the_unreplied_count_highlights_the_card() {
    let mut app = app_with(v2_snapshot(), SIZE);
    let mut snap = v2_snapshot();
    snap.lanes[1].cards[0].unreplied_comment_count = 1;
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
    assert!(app.highlights.contains_key(&103));
    assert_eq!(app.highlights.len(), 1);
}

#[test]
fn comments_are_notes_of_their_own_kind_and_unreplied_ones_are_marked() {
    let comment = |id: i64, body: &str| {
        let mut n = note(id, NoteKind::Comment, body);
        n.author = "user".into();
        n
    };
    let mut d = detail(
        3,
        "內容",
        vec![
            note(1, NoteKind::Progress, "開始"),
            comment(2, "這張先不要做"),
            note(3, NoteKind::Reply, "好，先不做"),
            comment(4, "再加一個測試"),
        ],
        vec![],
    );
    d.unreplied_comments = vec![4];
    let mut app = app_with(v2_snapshot(), (100, 40));
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, d);
    let text = render_text(&app, 100, 40);
    let heads: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("    ") && l.contains(':') && l.len() < 40)
        .collect();
    assert_eq!(heads.len(), 4, "{text}");
    // Newest first (docs/board-v2.md section 19.3).
    assert!(heads[0].ends_with("  留言   未回覆"), "{text}");
    assert!(heads[1].ends_with(" 回覆"), "{text}");
    assert!(heads[2].ends_with("  留言"), "no author, no 未回覆: {text}");
    assert!(heads[3].ends_with(" 進度"), "{text}");
    assert!(!text.contains("user"), "{text}");
    assert!(text.contains("      這張先不要做"), "{text}");
    let buf = buffer(&app, 100, 40);
    let (x, y) = find_any(&buf, "留言   未回覆").unwrap();
    assert_eq!(
        (buf[(x, y)].fg, buf[(x, y)].bg),
        (Color::White, Color::Blue)
    );
    assert_eq!(buf[(x - 1, y)].bg, Color::Blue, "a padded tag");
    let ux = find(&buf, y, "未回覆").unwrap();
    assert_eq!(buf[(ux, y)].fg, Color::Blue);
    assert!(buf[(ux, y)].modifier.contains(Modifier::BOLD));
    let (px, py) = find_any(&buf, "進度").unwrap();
    assert_ne!(
        buf[(px, py)].bg,
        Color::Blue,
        "the agent's notes look different"
    );
}

#[test]
fn feed_lines_for_comments_and_adoption() {
    let act = |verb: &str, detail: serde_json::Value| activity(9, verb, detail);
    use serde_json::json;
    assert_eq!(
        activity_text(&act(
            "comment.add",
            json!({"note": 31, "seq": 12, "excerpt": "這張先不要做"})
        )),
        "#12 留言：這張先不要做"
    );
    assert_eq!(
        activity_text(&act("comment.deliver", json!({"note": 31, "seq": 12}))),
        "#12 留言已送達 agent"
    );
    assert_eq!(
        activity_text(&act("board.adopt", json!({"from": "old", "auto": false}))),
        "看板由新的 session 接手"
    );
    assert_eq!(
        activity_text(&act("board.adopt", json!({"from": "old", "auto": true}))),
        "看板由新的 session 自動接手"
    );
    // On the board's feed.
    let mut app = app_with(v2_snapshot(), SIZE);
    let d = app.data.as_mut().unwrap();
    d.activity.insert(
        0,
        act(
            "comment.add",
            json!({"note": 31, "seq": 1, "excerpt": "先查 log"}),
        ),
    );
    assert!(lines(&app, 120, 30)[26].ends_with("#1 留言：先查 log"));
}

// ----- B: `i` in the question section ---------------------------------------------------

#[test]
fn i_on_an_option_row_goes_to_that_questions_input_row() {
    let mut app = card_with_questions((100, 40));
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(cursor(&app), Some((7, 3)));
    assert_eq!(app.input_target(), Some(7));
    assert!(app.comment.is_none(), "no comment row");
    // Now `i` is text.
    typed(&mut app, "ii");
    assert_eq!(app.marks[&7].text, "ii");
    assert!(app.comment.is_none());
    // From 「在 session 裡說明」 of question 8 too.
    press(&mut app, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&app), Some((8, 3)));
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(cursor(&app), Some((8, 4)));
}

#[test]
fn i_unmarks_session_reply_and_keeps_the_draft() {
    let mut app = card_with_questions((100, 40));
    press(&mut app, &[Key::Char('i')]);
    typed(&mut app, "草稿");
    press(&mut app, &[Key::Left]);
    press(&mut app, &[Key::Up, Key::Char(' ')]);
    assert!(app.marks[&7].chat);
    assert_eq!(cursor(&app), Some((7, 2)));
    // ↓ still cannot reach the input row while 「在 session 裡說明」 is marked.
    press(&mut app, &[Key::Down]);
    assert_eq!(cursor(&app), Some((8, 0)));
    press(&mut app, &[Key::Up, Key::Char('i')]);
    assert_eq!(cursor(&app), Some((7, 3)));
    assert!(!app.marks[&7].chat, "unmarked at that moment");
    assert_eq!(
        (app.marks[&7].text.as_str(), app.marks[&7].text_cursor),
        ("草稿", 1)
    );
    assert_eq!(app.input_target(), Some(7));
    // Only chat was marked and no draft: the marks go away entirely.
    let mut app = card_with_questions((100, 40));
    press(&mut app, &[Key::Down, Key::Down, Key::Char(' ')]);
    assert!(app.marks[&7].chat);
    press(&mut app, &[Key::Char('i')]);
    assert!(!app.marks.contains_key(&7));
    assert_eq!(app.input_target(), Some(7));
}

#[test]
fn a_typed_non_ascii_character_on_an_option_row_jumps_and_is_inserted() {
    let mut app = card_with_questions((100, 40));
    typed(&mut app, "中");
    assert_eq!(cursor(&app), Some((7, 3)));
    assert_eq!(app.marks[&7].text, "中");
    typed(&mut app, "文q");
    assert_eq!(app.marks[&7].text, "中文q");
    assert!(!app.quit);
    // From question 8's option with 「在 session 裡說明」 marked: unmarked, inserted at the
    // draft's cursor.
    press(
        &mut app,
        &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Char(' ')],
    );
    assert_eq!(cursor(&app), Some((8, 3)));
    assert!(app.marks[&8].chat);
    press(&mut app, &[Key::Up]);
    typed(&mut app, "「");
    assert_eq!(cursor(&app), Some((8, 4)));
    assert!(!app.marks[&8].chat);
    assert_eq!(app.marks[&8].text, "「");
    // ASCII letters on an option row stay commands.
    let mut app = card_with_questions((100, 40));
    typed(&mut app, "q");
    assert!(app.quit);
    // Control characters do nothing.
    let mut app = card_with_questions((100, 40));
    press(&mut app, &[Key::Char('\u{7}')]);
    assert_eq!(cursor(&app), Some((7, 0)));
}

#[test]
fn the_placeholder_mentions_i() {
    let app = card_with_questions((100, 40));
    let text = render_text(&app, 100, 40);
    assert!(
        text.contains("\n    輸入：（按 i 或移到這裡打字）\n"),
        "{text}"
    );
    assert_eq!(layout::INPUT_PLACEHOLDER, "（按 i 或移到這裡打字）");
}

// ----- C: key hints and help --------------------------------------------------------------

#[test]
fn board_hints_offer_i_and_shorten_to_fit() {
    let bar = |app: &App, w: u16| render_text(app, w, 30).lines().last().unwrap().to_string();
    let mut app = app_with(v2_snapshot(), (120, 30));
    assert_eq!(
        bar(&app, 120),
        " ←→ 泳道/側欄  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  e epic  o 排序  s 側欄  w b ?  q 離開"
    );
    // The user's pane: 89 columns.
    app.update(Event::Resize(89, 30));
    assert_eq!(bar(&app, 89), bar(&app, 120), "it fits at 89");
    // 80 columns: steps 1 and 2 (docs/board-v2.md section 16.4).
    app.update(Event::Resize(80, 30));
    assert_eq!(
        bar(&app, 80),
        " ←→ 泳道  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  e epic  o 排序  s 側欄  ?  q 離開"
    );
    // With an epic selected.
    press(&mut app, &[Key::Char('e')]);
    app.update(Event::Resize(120, 30));
    assert_eq!(
        bar(&app, 120),
        " ←→ 泳道/側欄  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  e epic  Esc 取消 epic  o 排序  s 側欄  w b ?  q 離開"
    );
    app.update(Event::Resize(89, 30));
    assert_eq!(
        bar(&app, 89),
        " ←→ 泳道  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  Esc 取消 epic  o 排序  s 側欄  ?  q 離開"
    );
    app.update(Event::Resize(80, 30));
    let at80 = bar(&app, 80);
    assert_eq!(
        at80,
        " ←→ 泳道  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  Esc 取消 epic  s 側欄  ?  q 離開"
    );
    for w in [60u16, 70, 80, 89, 100, 120] {
        app.update(Event::Resize(w, 30));
        let b = bar(&app, w);
        assert!(width(&b) <= w as usize, "{w}: {b}");
        assert!(b.contains("i 留言"), "{w}: {b}");
    }
    // The side panel's bar is unchanged.
    app.update(Event::Resize(80, 30));
    press(&mut app, &[Key::Left]);
    assert_eq!(
        bar(&app, 80),
        " ↑↓ 選 epic  Enter 開啟  → 泳道  e epic  Esc 取消 epic  s 側欄  w b ?  q 離開"
    );
}

#[test]
fn card_and_epic_hints_offer_i() {
    let last = |app: &App| render_text(app, 80, 40).lines().last().unwrap().to_string();
    let mut app = card_with_questions((80, 40));
    // On an option row: `i 打字`.
    assert_eq!(
        last(&app),
        "  ↑↓ 選擇  Space 勾選  i 打字  Enter 送出  Tab 切換區塊  ← 上一層  Esc 看板"
    );
    assert!(width(&last(&app)) <= 78);
    // On the input row: the input hints, unchanged.
    press(&mut app, &[Key::Char('i')]);
    assert_eq!(last(&app), "  打字輸入  Enter 送出  ↑↓ 離開輸入  Esc 看板");
    // Other sections: `i 留言` before Tab.
    press(&mut app, &[Key::Tab]);
    assert_eq!(
        last(&app),
        "  ↑↓ 捲動  PgUp/PgDn 翻頁  i 留言  Tab 切換區塊  ← 上一層  Esc 看板"
    );
    press(&mut app, &[Key::Tab]);
    assert_eq!(
        last(&app),
        "  ↑↓ 選連結  Enter 前往  i 留言  Tab 切換區塊  ← 上一層  Esc 看板"
    );
    // The epic screen.
    let mut app = app_with(v2_snapshot(), (80, 40));
    press(&mut app, &[Key::Left, Key::Enter]);
    assert_eq!(
        last(&app),
        "  ↑↓ 選卡片  Enter 前往  i 留言  ← 上一層  Esc 看板"
    );
}

#[test]
fn the_help_screen_lists_i_and_still_fits_80_by_24() {
    let mut app = app_with(v2_snapshot(), (80, 24));
    press(&mut app, &[Key::Char('?')]);
    let text = render_text(&app, 80, 24);
    for s in [
        "對選取的卡片留言",
        "留言；選項上是打字",
        "開啟卡片、留言",
        "輸入列與留言列：直接打字",
        // The last rows of both columns are on screen.
        "回到泳道",
        "清除文字",
    ] {
        assert!(text.contains(s), "{s}: {text}");
    }
}

// ----- D: no more `hc send` ---------------------------------------------------------------

#[test]
fn an_answer_shows_answer_sent_and_runs_no_notification() {
    let mut app = card_with_questions(SIZE);
    press(&mut app, &[Key::Char(' ')]);
    let fx = press(&mut app, &[Key::Enter]);
    let [Effect::Answer(r)] = &fx[..] else {
        panic!("{fx:?}");
    };
    assert_eq!(r.terminal.as_deref(), Some("t9"));
    // Nothing runs after a plain answer, even with a terminal.
    assert_eq!(crate::notify::command(r.terminal.as_deref(), false), None);
    app.update(Event::Answered {
        question_id: 7,
        error: None,
    });
    assert_eq!(app.flash.as_deref(), Some(MSG_SENT));
    assert_eq!(MSG_SENT, "回答已送出");
}

// ----- E: the write scope -------------------------------------------------------------------

/// The library's sources (not the tests).
const SOURCES: [(&str, &str); 8] = [
    ("app.rs", include_str!("../app.rs")),
    ("layout.rs", include_str!("../layout.rs")),
    ("lib.rs", include_str!("../lib.rs")),
    ("markdown.rs", include_str!("../markdown.rs")),
    ("notify.rs", include_str!("../notify.rs")),
    ("run.rs", include_str!("../run.rs")),
    ("text.rs", include_str!("../text.rs")),
    ("view.rs", include_str!("../view.rs")),
];

#[test]
fn the_tui_writes_only_through_the_two_functions_on_the_reader_role() {
    for (name, src) in SOURCES {
        assert!(!src.contains("kb_core::write::"), "{name}");
        for line in src
            .lines()
            .filter(|l| l.trim_start().starts_with("use kb_core"))
        {
            assert!(!line.contains("write"), "{name}: {line}");
        }
        for role in ["Role::Writer", "Role::Admin", "connect_role"] {
            assert!(!src.contains(role), "{name} uses {role}");
        }
        // The two writes live in run.rs only.
        if name != "run.rs" {
            assert!(!src.contains("answer_question("), "{name}");
            assert!(!src.contains("add_comment("), "{name}");
        }
    }
    let run = SOURCES.iter().find(|(n, _)| *n == "run.rs").unwrap().1;
    assert_eq!(
        run.matches("Role::").count(),
        run.matches("Role::Reader").count()
    );
    assert_eq!(run.matches("kb_core::answer::answer_question(").count(), 1);
    assert_eq!(run.matches("kb_core::comment::add_comment(").count(), 1);
    assert!(!run.contains("hc send") && !run.contains("\"hc\""));
    let notify = SOURCES.iter().find(|(n, _)| *n == "notify.rs").unwrap().1;
    assert!(!notify.contains("\"hc\""), "no hc send");
}
