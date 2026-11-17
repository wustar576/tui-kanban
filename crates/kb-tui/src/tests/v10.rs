//! The model judge on screen (docs/tui.md section 12): the live feed lines of
//! `judge.finding` and `judge.run`, the tags 等回覆 and 建議未記錄 on cards and in the epic
//! list, the header's 等回覆, and the card screen's explanation rows. Which findings are
//! active is decided by the read query (`kb-core/tests/judge_read.rs`); here the TUI gets
//! the active ones and draws them.

use kb_core::read::{ActiveFinding, JudgeTag};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use serde_json::json;

use super::v2_state::v2_snapshot;
use super::*;
use crate::layout::{self, CardJudge};
use crate::run::render_text;
use crate::text::{activity_text, width};
use crate::view;

const SIZE: (u16, u16) = (120, 30);

fn buffer(app: &App) -> Buffer {
    let area = Rect::new(0, 0, SIZE.0, SIZE.1);
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

fn lines(app: &App) -> Vec<String> {
    render_text(app, SIZE.0, SIZE.1)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A finding on card `seq` (card ids are `100 + seq`), or on the board.
fn finding(id: i64, seq: Option<i32>, tag: JudgeTag, p: f64) -> ActiveFinding {
    ActiveFinding {
        id,
        card_id: seq.map(|s| 100 + s as i64),
        tag,
        p,
    }
}

fn load(app: &mut App, snapshot: BoardSnapshot, findings: Vec<ActiveFinding>, feed: Vec<Activity>) {
    let mut d = data(snapshot);
    d.findings = findings;
    if !feed.is_empty() {
        d.activity = feed;
    }
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(d),
        at: Instant::now(),
    });
}

fn app_judged(snapshot: BoardSnapshot, findings: Vec<ActiveFinding>) -> App {
    let mut app = App::new(SIZE, today(), Scope::All, Some(BOARD_ID));
    load(&mut app, snapshot, findings, Vec::new());
    app
}

fn judge_finding(id: i64, seq: Option<i32>, detail: serde_json::Value) -> Activity {
    let mut a = activity(id, "judge.finding", detail);
    a.actor = "hook".into();
    a.card_id = seq.map(|s| 100 + s as i64);
    a
}

// ----- live feed ----------------------------------------------------------------------

#[test]
fn feed_lines_of_findings_and_failed_runs() {
    let advice = judge_finding(
        1,
        Some(3),
        json!({"question": "advice", "p": 0.82, "band": "record", "woke": false,
               "text": "card #3 \"x\": ...",
               "message": "#3 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄"}),
    );
    assert_eq!(
        activity_text(&advice),
        "評測：#3 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄（0.82）"
    );
    let woke = judge_finding(
        2,
        Some(3),
        json!({"question": "advice", "p": 0.9, "band": "wake", "woke": true,
               "text": "card #3 \"x\": ...",
               "message": "#3 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄"}),
    );
    assert_eq!(
        activity_text(&woke),
        "評測：#3 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄（0.90，已喚醒 agent）"
    );
    // A wake-band finding that did not wake (the turn was woken before) says nothing more.
    let not_woken = judge_finding(
        3,
        None,
        json!({"question": "waiting", "p": 0.934, "band": "wake", "woke": false,
               "text": "your last message ...",
               "message": "最後的回覆在等使用者回答，看板上沒有提問，也沒有卡片標為阻擋"}),
    );
    assert_eq!(
        activity_text(&not_woken),
        "評測：最後的回覆在等使用者回答，看板上沒有提問，也沒有卡片標為阻擋（0.93）"
    );

    let mut failed = activity(
        4,
        "judge.run",
        json!({"questions": 4, "findings": 0, "ms": 8001, "input_chars": 300,
               "model": "gemini-decision-1", "error": "timeout after 8000 ms"}),
    );
    failed.actor = "hook".into();
    assert_eq!(activity_text(&failed), "評測失敗：timeout after 8000 ms");
    // Without an error the feed does not load the row (read::recent_feed); the sentence
    // exists for completeness.
    let quiet = activity(
        5,
        "judge.run",
        json!({"questions": 4, "findings": 1, "ms": 900, "input_chars": 300,
               "model": "gemini-decision-1"}),
    );
    assert_eq!(activity_text(&quiet), "評測：4 題、1 個發現");

    // On the board screen: newest first, with the time.
    let mut app = App::new(SIZE, today(), Scope::All, Some(BOARD_ID));
    load(
        &mut app,
        demo_snapshot(),
        Vec::new(),
        vec![failed, woke, advice],
    );
    let text = lines(&app);
    let strip: Vec<&String> = text.iter().filter(|l| l.contains("評測")).collect();
    assert_eq!(strip.len(), 3, "{text:#?}");
    assert!(
        strip[0].ends_with("評測失敗：timeout after 8000 ms"),
        "{}",
        strip[0]
    );
    assert!(strip[1].ends_with("（0.90，已喚醒 agent）"), "{}", strip[1]);
    assert!(strip[2].ends_with("卡片上沒有紀錄（0.82）"), "{}", strip[2]);
}

// ----- tags ---------------------------------------------------------------------------

#[test]
fn cards_show_the_judge_tags_without_a_border_color() {
    // #1 waits in the chat; #2 has unrecorded advice; #3 (blocked) has both.
    let app = app_judged(
        demo_snapshot(),
        vec![
            finding(1, Some(1), JudgeTag::Waiting, 0.93),
            finding(2, Some(2), JudgeTag::Advice, 0.82),
            finding(3, Some(3), JudgeTag::Waiting, 0.6),
            finding(4, Some(3), JudgeTag::Advice, 0.5),
        ],
    );
    let text = render_text(&app, SIZE.0, SIZE.1);
    let buf = buffer(&app);

    // #1: only the 等回覆 tag, black on light gray; the border keeps the default color.
    let (x1, y1) = find_any(&buf, "#1 補上逾時測試").unwrap();
    let (tx, ty) = find_any(&buf, "等回覆").unwrap();
    assert_eq!(ty, y1 + 1, "{text}");
    assert_eq!(
        (buf[(tx, ty)].fg, buf[(tx, ty)].bg),
        (Color::Black, Color::Gray)
    );
    assert_eq!(
        buf[(tx - 1, ty)].bg,
        Color::Gray,
        "the tag is padded like 提問"
    );
    assert_eq!(buf[(x1 - 2, y1)].fg, Color::Reset, "default border");

    // #2: 建議未記錄, underlined in the default color.
    let (_, y2) = find_any(&buf, "#2 更新文件").unwrap();
    let ay = y2 + 1;
    let ax = find(&buf, ay, "建議未記錄").unwrap_or_else(|| panic!("{text}"));
    assert!(buf[(ax, ay)].modifier.contains(Modifier::UNDERLINED));
    assert_eq!(
        (buf[(ax, ay)].fg, buf[(ax, ay)].bg),
        (Color::Reset, Color::Reset)
    );

    // #3: after 阻塞, in order; the border stays red.
    assert!(text.contains("阻塞   等回覆  建議未記錄"), "{text}");
    let (x3, y3) = find_any(&buf, "#3 修正 token").unwrap();
    assert_eq!(buf[(x3 - 2, y3)].fg, Color::Red);

    // #4 (done) has no finding and no tag.
    assert_eq!(text.matches("建議未記錄").count(), 2, "{text}");
}

#[test]
fn a_judge_tag_alone_adds_the_tags_row() {
    let c = &demo_snapshot().lanes[0].cards[0];
    let none = CardJudge::default();
    let waiting = CardJudge {
        waiting: Some(0.9),
        advice: None,
    };
    let advice = CardJudge {
        waiting: None,
        advice: Some(0.9),
    };
    assert!(!layout::has_tags_row(c, none));
    let before = layout::card_height(c, 40, none);
    assert_eq!(layout::card_height(c, 40, waiting), before + 1);
    assert_eq!(layout::card_height(c, 40, advice), before + 1);
    // card_heights reads the board's findings.
    let findings = [finding(1, Some(1), JudgeTag::Advice, 0.5)];
    let cards = &demo_snapshot().lanes[0].cards;
    assert_eq!(
        layout::card_heights(cards, 40, false, &findings),
        [before + 1, layout::card_height(&cards[1], 40, none)]
    );
}

#[test]
fn the_latest_finding_per_question_gives_the_probability() {
    let findings = [
        finding(1, Some(1), JudgeTag::Advice, 0.4),
        finding(2, Some(2), JudgeTag::Advice, 0.9),
        finding(3, Some(1), JudgeTag::Advice, 0.7),
        finding(4, None, JudgeTag::Waiting, 0.8),
    ];
    assert_eq!(
        CardJudge::of(&findings, 101),
        CardJudge {
            waiting: None,
            advice: Some(0.7)
        }
    );
    assert_eq!(CardJudge::of(&findings, 103), CardJudge::default());
}

#[test]
fn a_reload_without_the_finding_removes_the_tag() {
    let mut app = app_judged(
        demo_snapshot(),
        vec![finding(1, Some(1), JudgeTag::Waiting, 0.93)],
    );
    assert!(render_text(&app, SIZE.0, SIZE.1).contains("等回覆"));
    // The user replied: the query no longer returns the finding.
    load(&mut app, demo_snapshot(), Vec::new(), Vec::new());
    assert!(!render_text(&app, SIZE.0, SIZE.1).contains("等回覆"));
}

#[test]
fn the_epic_list_shows_the_judge_tags() {
    let mut app = app_judged(
        v2_snapshot(),
        vec![finding(1, Some(3), JudgeTag::Advice, 0.82)],
    );
    press(&mut app, &[Key::Left, Key::Enter]);
    let text = render_text(&app, SIZE.0, SIZE.1);
    let row3 = text.lines().find(|l| l.contains("#3 ")).unwrap();
    assert!(row3.ends_with("提問  建議未記錄"), "{row3}");
}

// ----- header -------------------------------------------------------------------------

#[test]
fn a_board_level_waiting_finding_shows_in_the_header() {
    let app = app_judged(
        v2_snapshot(),
        vec![finding(1, None, JudgeTag::Waiting, 0.7)],
    );
    let l = lines(&app);
    assert!(
        l[0].ends_with("提問 3  討論 1  等回覆  ● 已連線"),
        "{}",
        l[0]
    );
    let buf = buffer(&app);
    let (x, y) = find_any(&buf, "等回覆").unwrap();
    assert_eq!(y, 0);
    assert_eq!(
        (buf[(x, 0)].fg, buf[(x, 0)].bg),
        (Color::Black, Color::Gray)
    );
    assert!(buf[(x, 0)].modifier.contains(Modifier::BOLD));

    // A card's waiting finding is a tag, not the header indicator; no finding, nothing.
    let app = app_judged(
        v2_snapshot(),
        vec![finding(1, Some(1), JudgeTag::Waiting, 0.7)],
    );
    assert!(!lines(&app)[0].contains("等回覆"));
    let app = app_judged(v2_snapshot(), Vec::new());
    assert!(!lines(&app)[0].contains("等回覆"));
}

// ----- card screen --------------------------------------------------------------------

#[test]
fn the_card_screen_explains_each_active_tag_under_the_blocked_row() {
    // #3 is blocked and has both findings.
    let mut app = app_judged(
        demo_snapshot(),
        vec![
            finding(1, Some(3), JudgeTag::Advice, 0.82),
            finding(2, Some(3), JudgeTag::Waiting, 0.93),
        ],
    );
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, detail(3, "內容", vec![], vec![]));
    let l = lines(&app);
    let blocked = l.iter().position(|s| s.contains("阻塞：")).unwrap();
    assert_eq!(
        l[blocked + 1].trim(),
        "等回覆：agent 在對話裡等你回覆（評測 0.93）",
        "{l:#?}"
    );
    assert_eq!(
        l[blocked + 2].trim(),
        "建議未記錄：agent 的回覆對這張卡片提出建議或決定，卡片沒有紀錄（評測 0.82）"
    );
    // The sections start below them: the head has the two rows more.
    let body = l
        .iter()
        .position(|s| s.trim_start().starts_with("內容"))
        .unwrap();
    assert!(body > blocked + 2, "{l:#?}");
    let d = app.card.as_ref().unwrap().detail.clone().unwrap();
    assert_eq!(app.card_status_rows(&d), 3);
    let buf = buffer(&app);
    let (x, y) = find_any(&buf, "等回覆：").unwrap();
    assert_eq!(
        (buf[(x, y)].fg, buf[(x, y)].bg),
        (Color::Black, Color::Gray)
    );
    let (x, y) = find_any(&buf, "建議未記錄：").unwrap();
    assert!(buf[(x, y)].modifier.contains(Modifier::UNDERLINED));

    // A card that is not blocked: the rows follow the info row directly.
    let mut app = app_judged(
        demo_snapshot(),
        vec![finding(1, Some(1), JudgeTag::Advice, 0.82)],
    );
    press(&mut app, &[Key::Enter]);
    deliver_card(&mut app, detail(1, "內容", vec![], vec![]));
    let l = lines(&app);
    let info = l
        .iter()
        .position(|s| s.contains("待辦 · 約定"))
        .unwrap_or_else(|| panic!("{l:#?}"));
    assert!(
        l[info + 1].trim().starts_with("建議未記錄：agent 的回覆"),
        "{l:#?}"
    );
    assert!(!l.iter().any(|s| s.contains("等回覆")));
    // Without findings, no row.
    let mut app = app_judged(demo_snapshot(), Vec::new());
    press(&mut app, &[Key::Enter]);
    deliver_card(&mut app, detail(1, "內容", vec![], vec![]));
    assert!(!render_text(&app, SIZE.0, SIZE.1).contains("評測"));
}
