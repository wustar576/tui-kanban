//! Pure state transitions of the redesign: epics, side panel, lane widths, the `a` key
//! and the question section (docs/board-v2.md sections 5-7, docs/questions.md 7).

use kb_core::model::{LaneKind, Origin, QuestionStatus};
use ratatui::layout::Rect;

use super::*;
use crate::app::{AnswerRequest, EpicKey, MSG_MARK_FIRST, MSG_NO_QUESTIONS, MSG_SENDING, MSG_SENT};
use crate::layout::{Section, lane_strip};

/// 待辦 (#1 question + discussing, #2 blocked + derived), 處理中 (#3 question),
/// 完成 (#4), 捨棄 (#5); epics 1 「P1 看板」, 2 「P2 hook 與守則」 (with summary),
/// 3 「P0 資料庫」 (finished).
pub fn v2_snapshot() -> BoardSnapshot {
    let in_epic = |mut c: CardSummary, id: i64, name: &str| {
        c.card.epic_id = Some(id);
        c.epic_name = Some(name.into());
        c
    };
    let mut c1 = in_epic(card(1, 0, "TUI 回答提問", 1), 1, "P1 看板");
    c1.open_question_count = 1;
    c1.card.origin = Origin::Discussing;
    c1.spawned_count = 1;
    let mut c2 = in_epic(
        card(2, 0, "用真實的 session 驗證 hook", 2),
        2,
        "P2 hook 與守則",
    );
    c2.card.blocked_reason = Some("等待使用者".into());
    c2.card.origin = Origin::Derived;
    c2.spawned_from = Some(1);
    c2.blocked_by = vec![3];
    let mut c3 = in_epic(card(3, 0, "看板模型加入 epic", 0), 1, "P1 看板");
    c3.open_question_count = 2;
    let c4 = in_epic(card(4, 0, "建立資料表", 2), 3, "P0 資料庫");
    let c5 = card(5, 0, "標籤改用 256 色", 3);
    let mut snap = snapshot(
        "",
        vec![
            (lane(1, 1, "待辦", LaneKind::Todo), vec![c1, c2]),
            (lane(2, 2, "處理中", LaneKind::Doing), vec![c3]),
            (lane(3, 3, "完成", LaneKind::Done), vec![c4]),
            (lane(4, 4, "捨棄", LaneKind::Dropped), vec![c5]),
        ],
    );
    snap.epics = vec![
        epic(1, "P1 看板", None, counts(2, 0, 0)),
        epic(
            2,
            "P2 hook 與守則",
            Some("約定：hook 三種\n完成：R1-R5"),
            counts(1, 0, 1),
        ),
        epic(3, "P0 資料庫", Some("全部完成"), counts(1, 1, 0)),
    ];
    snap.no_epic = None;
    snap.open_question_count = 3;
    snap.discussing_count = 1;
    snap.board.terminal_id = Some("t9".into());
    snap
}

const SIZE: (u16, u16) = (120, 30);

fn reload(app: &mut App, snap: BoardSnapshot) {
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snap)),
        at: Instant::now(),
    });
}

// ----- epics ----------------------------------------------------------------------------

#[test]
fn e_and_shift_e_cycle_epics_then_back_to_none() {
    let mut app = app_with(v2_snapshot(), SIZE);
    assert_eq!(app.sel_epic, None);
    let mut seen = Vec::new();
    for _ in 0..4 {
        press(&mut app, &[Key::Char('e')]);
        seen.push(app.sel_epic);
    }
    assert_eq!(
        seen,
        vec![
            Some(EpicKey::Epic(1)),
            Some(EpicKey::Epic(2)),
            Some(EpicKey::Epic(3)),
            None
        ]
    );
    press(&mut app, &[Key::Char('E')]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(3)));
    press(&mut app, &[Key::Char('E'), Key::Char('E'), Key::Char('E')]);
    assert_eq!(app.sel_epic, None);

    // A selected epic that disappears on reload is unselected; others stay selected.
    press(&mut app, &[Key::Char('e'), Key::Char('e')]);
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
    reload(&mut app, v2_snapshot());
    assert_eq!(app.sel_epic, Some(EpicKey::Epic(2)));
    let mut snap = v2_snapshot();
    snap.epics.remove(1);
    reload(&mut app, snap);
    assert_eq!(app.sel_epic, None);

    // No epics: nothing to select.
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Char('e')]);
    assert_eq!(app.sel_epic, None);
}

// ----- side panel -----------------------------------------------------------------------

#[test]
fn s_toggles_the_side_panel_which_hides_under_80_columns() {
    let mut app = app_with(v2_snapshot(), SIZE);
    assert!(app.side_shown());
    press(&mut app, &[Key::Char('s')]);
    assert!(!app.side_shown());
    press(&mut app, &[Key::Char('s')]);
    assert!(app.side_shown());

    // Under 80 columns: hidden, and `s` does nothing.
    app.update(Event::Resize(79, 30));
    assert!(!app.side_shown());
    press(&mut app, &[Key::Char('s')]);
    assert!(app.show_side, "s has no effect under 80 columns");
    app.update(Event::Resize(80, 30));
    assert!(
        app.side_shown(),
        "comes back when the screen is wide enough"
    );
}

// ----- lane widths ----------------------------------------------------------------------

#[test]
fn closed_lanes_are_narrow_unless_selected() {
    use LaneKind::*;
    let kinds = [Todo, Doing, Done, Dropped];
    let widths = |s: &crate::layout::LaneStrip| -> Vec<(usize, u16, bool)> {
        s.slots
            .iter()
            .map(|x| (x.index, x.width, x.narrow))
            .collect()
    };
    // Everything fits: the open lanes share what the narrow ones leave.
    let s = lane_strip(Rect::new(0, 0, 96, 20), &kinds, 0, 0);
    assert!(!s.scrolling);
    assert_eq!(
        widths(&s),
        vec![(0, 34, false), (1, 34, false), (2, 14, true), (3, 14, true)]
    );
    assert_eq!(
        s.slots.iter().map(|x| x.x).collect::<Vec<_>>(),
        vec![0, 34, 68, 82]
    );
    // The selected done lane has the normal width.
    let s = lane_strip(Rect::new(0, 0, 96, 20), &kinds, 2, 0);
    assert_eq!(
        widths(&s),
        vec![
            (0, 24 + 4, false),
            (1, 27, false),
            (2, 27, false),
            (3, 14, true)
        ]
    );
    assert_eq!(s.slots.iter().map(|x| x.width as usize).sum::<usize>(), 96);

    // 90 columns with the side panel leave 66: two open lanes and one narrow lane.
    let s = lane_strip(Rect::new(24, 1, 66, 37), &kinds, 0, 0);
    assert!(s.scrolling && s.more_right && !s.more_left);
    assert_eq!(
        widths(&s),
        vec![(0, 26, false), (1, 26, false), (2, 14, true)]
    );
    let s = lane_strip(Rect::new(24, 1, 66, 37), &kinds, 2, 0);
    assert!(s.more_left && !s.more_right);
    assert_eq!(
        widths(&s),
        vec![(1, 26, false), (2, 26, false), (3, 14, true)]
    );

    // The app keeps the selected lane on screen with the same rule.
    let mut app = app_with(v2_snapshot(), (90, 39));
    press(&mut app, &[Key::Right, Key::Right]);
    let strip = app.lane_strip();
    let slot = strip.slots.iter().find(|s| s.index == 2).unwrap();
    assert!(!slot.narrow);
    assert_eq!(app.lane_offset, 1);
}

// ----- the `a` key ----------------------------------------------------------------------

#[test]
fn a_opens_the_next_card_with_an_open_question_and_wraps() {
    let mut app = app_with(v2_snapshot(), SIZE);
    assert_eq!(selected_seq(&app), Some(1));
    let fx = press(&mut app, &[Key::Char('a')]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 3
        }],
        "#1 is selected, so the next one is #3"
    );
    assert_eq!(app.card.as_ref().unwrap().seq, 3);
    assert_eq!(selected_seq(&app), Some(3));
    let mut d = detail(3, "", vec![], vec![]);
    d.questions = vec![question(7, 3, "多久？", false, &["15 分鐘", "30 分鐘"])];
    deliver_card(&mut app, d);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Questions);

    press(&mut app, &[Key::Esc]);
    let fx = press(&mut app, &[Key::Char('a')]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 1
        }],
        "wraps around to the first one"
    );

    // No open question anywhere: a message, nothing opens; the next key clears it.
    let mut app = app_with(demo_snapshot(), SIZE);
    assert!(press(&mut app, &[Key::Char('a')]).is_empty());
    assert!(app.card.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_NO_QUESTIONS));
    press(&mut app, &[Key::Down]);
    assert_eq!(app.flash, None);
}

// ----- questions ------------------------------------------------------------------------

/// Card #3 with an open single-choice question 7 (options 71, 72) and an open multi
/// question 8 (options 81, 82, 83).
fn question_detail() -> CardDetail {
    let mut d = detail(3, "內容", vec![], vec![]);
    d.questions = vec![
        question(
            7,
            3,
            "token 的有效期？",
            false,
            &["15 分鐘::與 refresh 一致", "30 分鐘"],
        ),
        question(8, 3, "哪些守則維持阻擋？", true, &["R1", "R2", "R4"]),
    ];
    d
}

fn open_questions(app: &mut App, d: CardDetail) {
    press(app, &[Key::Right, Key::Enter]);
    deliver_card(app, d);
}

fn marks(app: &App, q: i64) -> (Vec<i64>, bool) {
    app.marks
        .get(&q)
        .map(|m| (m.options.iter().copied().collect(), m.chat))
        .unwrap_or_default()
}

fn cursor(app: &App) -> Option<(i64, usize)> {
    app.card.as_ref().unwrap().q_cursor
}

#[test]
fn space_marks_choices_with_single_choice_and_chat_exclusivity() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open_questions(&mut app, question_detail());
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Questions);
    assert_eq!(cursor(&app), Some((7, 0)));

    // Enter before marking anything: nothing is sent.
    assert!(press(&mut app, &[Key::Enter]).is_empty());
    assert_eq!(app.flash.as_deref(), Some(MSG_MARK_FIRST));

    // Single choice: Space replaces the previous mark.
    press(&mut app, &[Key::Char(' ')]);
    assert_eq!(marks(&app, 7), (vec![71], false));
    press(&mut app, &[Key::Down, Key::Char(' ')]);
    assert_eq!(marks(&app, 7), (vec![72], false));
    // The last choice, 「在 session 裡說明」, replaces options and is replaced by them.
    press(&mut app, &[Key::Down, Key::Char(' ')]);
    assert_eq!(cursor(&app), Some((7, 2)));
    assert_eq!(marks(&app, 7), (vec![], true));
    press(&mut app, &[Key::Up, Key::Char(' ')]);
    assert_eq!(marks(&app, 7), (vec![72], false));

    // The cursor crosses the input row into the next question.
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&app), Some((8, 0)));
    // Multiple choice: Space toggles.
    press(&mut app, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    assert_eq!(marks(&app, 8), (vec![81, 82], false));
    press(&mut app, &[Key::Char(' ')]);
    assert_eq!(marks(&app, 8), (vec![81], false));
    // Chat cancels the options; an option cancels chat.
    press(&mut app, &[Key::Down, Key::Down, Key::Char(' ')]);
    assert_eq!(cursor(&app), Some((8, 3)));
    assert_eq!(marks(&app, 8), (vec![], true));
    press(&mut app, &[Key::Up, Key::Char(' ')]);
    assert_eq!(marks(&app, 8), (vec![83], false));
    // The cursor stops at the last choice, the input row; question 7's marks are
    // untouched.
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&app), Some((8, 4)));
    assert_eq!(marks(&app, 7), (vec![72], false));
    // Space outside the question section does nothing.
    press(&mut app, &[Key::Tab, Key::Char(' ')]);
    assert_eq!(marks(&app, 8), (vec![83], false));
}

#[test]
fn enter_sends_the_cursor_question_and_the_result_clears_its_marks() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open_questions(&mut app, question_detail());
    // Mark 81 and 83 on question 8, then send with the cursor on question 8.
    press(
        &mut app,
        &[
            Key::Down,
            Key::Down,
            Key::Down,
            Key::Down,
            Key::Char(' '),
            Key::Down,
            Key::Down,
            Key::Char(' '),
        ],
    );
    assert_eq!(cursor(&app), Some((8, 2)));
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::Answer(AnswerRequest {
            board_id: BOARD_ID,
            seq: 3,
            question_id: 8,
            option_ids: vec![81, 83],
            chat: false,
            text: None,
            terminal: Some("t9".into()),
        })]
    );
    assert_eq!(app.submitting, Some(8));
    assert_eq!(app.flash.as_deref(), Some(MSG_SENDING));
    assert!(press(&mut app, &[Key::Enter]).is_empty(), "no double send");

    let fx = app.update(Event::Answered {
        question_id: 8,
        error: None,
    });
    assert!(fx.contains(&Effect::LoadBoard(BoardTarget::Id(BOARD_ID))));
    assert!(fx.contains(&Effect::LoadCard {
        board_id: BOARD_ID,
        seq: 3
    }));
    assert_eq!(app.submitting, None);
    assert!(!app.marks.contains_key(&8));
    assert_eq!(app.flash.as_deref(), Some(MSG_SENT));
    // The pane focus after 「在 session 裡說明」 replaces the line when it worked.
    app.update(Event::Notified(crate::notify::FOCUSED.into()));
    assert_eq!(app.flash.as_deref(), Some(crate::notify::FOCUSED));

    // Chat on question 7 sends no option ids.
    press(&mut app, &[Key::Home]);
    let up = [Key::Up; 6];
    press(&mut app, &up);
    press(&mut app, &[Key::Down, Key::Down, Key::Char(' ')]);
    assert_eq!(cursor(&app), Some((7, 2)));
    let fx = press(&mut app, &[Key::Enter]);
    match &fx[..] {
        [Effect::Answer(r)] => {
            assert_eq!((r.question_id, r.chat), (7, true));
            assert!(r.option_ids.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // A refused answer shows the reason and keeps the marks.
    app.update(Event::Answered {
        question_id: 7,
        error: Some("提問已經不是待回答".into()),
    });
    assert_eq!(app.flash.as_deref(), Some("送出失敗：提問已經不是待回答"));
    assert_eq!(marks(&app, 7), (vec![], true));
}

#[test]
fn marks_and_cursor_survive_reloads_until_the_question_closes() {
    let mut app = app_with(v2_snapshot(), SIZE);
    open_questions(&mut app, question_detail());
    press(&mut app, &[Key::Char(' ')]); // 71
    press(
        &mut app,
        &[
            Key::Down,
            Key::Down,
            Key::Down,
            Key::Down,
            Key::Down,
            Key::Char(' '),
        ],
    ); // 82 (past question 7's input row)
    assert_eq!(cursor(&app), Some((8, 1)));

    // A live reload with the same questions keeps everything.
    deliver_card(&mut app, question_detail());
    assert_eq!(marks(&app, 7), (vec![71], false));
    assert_eq!(marks(&app, 8), (vec![82], false));
    assert_eq!(cursor(&app), Some((8, 1)));

    // Question 8 got answered elsewhere: its marks go, the cursor moves to the first
    // open choice; question 7 keeps its marks.
    let mut d = question_detail();
    d.questions[1].question.status = QuestionStatus::Answered;
    d.questions[1].options[2].selected = true;
    deliver_card(&mut app, d);
    assert!(!app.marks.contains_key(&8));
    assert_eq!(marks(&app, 7), (vec![71], false));
    assert_eq!(cursor(&app), Some((7, 0)));

    // The agent cancelled question 7 too: nothing is open, nothing is marked.
    let mut d = question_detail();
    d.questions[0].question.status = QuestionStatus::Cancelled;
    d.questions[1].question.status = QuestionStatus::Answered;
    deliver_card(&mut app, d);
    assert!(app.marks.is_empty());
    assert_eq!(cursor(&app), None);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Questions);
    assert!(press(&mut app, &[Key::Enter]).is_empty());
}

#[test]
fn initial_focus_prefers_open_questions_then_links_then_body() {
    let link = card_link(
        1,
        kb_core::model::LinkKind::Spawned,
        kb_core::read::LinkDirection::Incoming,
        1,
        "甲",
    );
    // Only an answered question and a link: the links.
    let mut app = app_with(v2_snapshot(), SIZE);
    let mut d = detail(3, "", vec![], vec![link.clone()]);
    let mut q = question(7, 3, "答過了", false, &["是", "否"]);
    q.question.status = QuestionStatus::Answered;
    d.questions = vec![q];
    open_questions(&mut app, d);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Links);
    // An open question wins over links.
    let mut app = app_with(v2_snapshot(), SIZE);
    let mut d = question_detail();
    d.links = vec![link];
    open_questions(&mut app, d);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Questions);
}
