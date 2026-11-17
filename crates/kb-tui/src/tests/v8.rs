//! What a decision follows from (docs/board-v2.md section 18.6): the note header, the
//! result's causes, the 依據 and 引出 rows of the 連結 section with Enter navigation, and
//! the live feed.

use kb_core::model::{Cause, CauseKind, DecisionKind, LinkKind, NoteKind};
use kb_core::read::{Consequence, LinkDirection};
use ratatui::style::Color;

use super::*;
use crate::layout::{LinkRowKey, Section, link_rows};
use crate::run::render_text;
use crate::text::activity_text;

const SIZE: (u16, u16) = (80, 40);

fn cause(kind: CauseKind, id: Option<i64>, seq: Option<i32>, title: Option<&str>) -> Cause {
    Cause {
        kind,
        id,
        card_seq: seq,
        card_title: title.map(str::to_string),
    }
}

fn led(
    via: CauseKind,
    via_id: Option<i64>,
    seq: i32,
    title: &str,
    decision: DecisionKind,
    note_kind: Option<NoteKind>,
) -> Consequence {
    Consequence {
        via,
        via_id,
        card_seq: seq,
        card_title: title.into(),
        decision,
        note_id: note_kind.map(|_| 50),
        note_kind,
        text: "文字".into(),
        at: at(20),
    }
}

/// Card #3 「修正 token 更新時間」: one spawned link to #1; a decision note following from
/// #1, question 5 (on #4) and the user; the result follows from #2; it led to #4's drop,
/// #2's decision (through its question 7) and its own decision (left out of 連結).
fn detail_3() -> CardDetail {
    let mut n = note(1, NoteKind::Decision, "改用滑動視窗");
    n.because = vec![
        cause(CauseKind::Card, None, Some(1), Some("補上逾時測試")),
        cause(
            CauseKind::Question,
            Some(5),
            Some(4),
            Some("重現登入逾時的問題"),
        ),
        cause(CauseKind::User, None, None, None),
    ];
    let mut d = detail(
        3,
        "內容",
        vec![n],
        vec![card_link(
            1,
            LinkKind::Spawned,
            LinkDirection::Incoming,
            1,
            "補上逾時測試",
        )],
    );
    d.card.result = Some("改好了".into());
    d.result_because = vec![cause(CauseKind::Card, None, Some(2), Some("更新文件"))];
    d.led_to = vec![
        led(
            CauseKind::Card,
            None,
            4,
            "重現登入逾時的問題",
            DecisionKind::Drop,
            None,
        ),
        led(
            CauseKind::Question,
            Some(7),
            2,
            "更新文件",
            DecisionKind::Note,
            Some(NoteKind::Decision),
        ),
        // A question of #3 led to #3's own decision: shown in 紀錄 only.
        led(
            CauseKind::Question,
            Some(8),
            3,
            "修正 token 更新時間",
            DecisionKind::Note,
            Some(NoteKind::Decision),
        ),
        // The same row twice is shown once.
        led(
            CauseKind::Card,
            None,
            4,
            "重現登入逾時的問題",
            DecisionKind::Drop,
            None,
        ),
    ];
    d
}

fn open_3(app: &mut App) {
    press(app, &[Key::Right, Key::Enter]);
    deliver_card(app, detail_3());
}

#[test]
fn link_rows_list_links_then_causes_then_what_the_card_led_to() {
    let rows = link_rows(&detail_3());
    let text: Vec<(String, String, Option<i32>)> = rows
        .iter()
        .map(|r| (r.dir.clone(), r.target.clone(), r.card))
        .collect();
    let row = |d: &str, t: &str, c: i32| (d.to_string(), t.to_string(), Some(c));
    assert_eq!(
        text,
        vec![
            row("延伸自", "#1 補上逾時測試", 1),
            row("依據", "#1 補上逾時測試", 1),
            row("依據", "#4 的提問 5  重現登入逾時的問題", 4),
            row("依據", "#2 更新文件", 2),
            row("引出", "#4 的捨棄  重現登入逾時的問題", 4),
            row("引出", "#2 的決定  更新文件", 2),
        ]
    );
    assert_eq!(rows[0].key, LinkRowKey::Link(1));
    // A comment cause and a done.
    let mut d = detail(1, "", vec![], vec![]);
    d.result_because = vec![cause(
        CauseKind::Comment,
        Some(75),
        Some(2),
        Some("更新文件"),
    )];
    d.led_to = vec![led(
        CauseKind::Comment,
        Some(9),
        4,
        "重現",
        DecisionKind::Done,
        None,
    )];
    let rows = link_rows(&d);
    assert_eq!(rows[0].target, "#2 的留言 75  更新文件");
    assert_eq!(rows[1].target, "#4 的完成  重現");
    // Only a user cause: no row.
    let mut d = detail(1, "", vec![], vec![]);
    d.result_because = vec![cause(CauseKind::User, None, None, None)];
    assert!(link_rows(&d).is_empty());
}

#[test]
fn card_screen_shows_causes_in_notes_result_and_links() {
    let mut app = app_with(demo_snapshot(), SIZE);
    open_3(&mut app);
    let text = render_text(&app, SIZE.0, SIZE.1);
    let lines: Vec<&str> = text.lines().collect();

    // 內容: the result's causes, dim, aligned with the result text.
    let result = lines.iter().position(|l| *l == "    結果：改好了").unwrap();
    assert_eq!(lines[result + 1], "          依據 #2", "{text}");

    // 連結: six rows, the count includes them.
    let links = lines
        .iter()
        .position(|l| l.starts_with("  連結 (6) ─"))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(lines[links + 2], "  ▶ 延伸自  #1 補上逾時測試");
    assert_eq!(lines[links + 3], "    依據    #1 補上逾時測試");
    assert_eq!(
        lines[links + 4],
        "    依據    #4 的提問 5  重現登入逾時的問題"
    );
    assert_eq!(lines[links + 5], "    依據    #2 更新文件");
    assert_eq!(
        lines[links + 6],
        "    引出    #4 的捨棄  重現登入逾時的問題"
    );
    assert_eq!(lines[links + 7], "    引出    #2 的決定  更新文件");

    // 紀錄: the note header shows the causes after the kind.
    let notes = lines
        .iter()
        .position(|l| l.starts_with("  紀錄 (1) ─"))
        .unwrap();
    assert!(
        lines[notes + 2].ends_with(" 決定  依據 #1、提問 5、使用者的指示"),
        "{}",
        lines[notes + 2]
    );
    assert_eq!(lines[notes + 3], "      改用滑動視窗");

    // The causes are dim; the kind keeps its color.
    let area = ratatui::layout::Rect::new(0, 0, SIZE.0, SIZE.1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut buf);
    let y = (notes + 2) as u16;
    let row: Vec<(String, Color)> = (0..SIZE.0)
        .map(|x| {
            let c = &buf[(x, y)];
            (c.symbol().to_string(), c.fg)
        })
        .collect();
    let at = |s: &str| row.iter().position(|(sym, _)| sym == s).unwrap();
    assert_eq!(row[at("決")].1, Color::Cyan);
    assert_eq!(row[at("依")].1, Color::DarkGray);
}

#[test]
fn enter_on_a_cause_or_led_to_row_opens_that_card_and_back_returns_to_the_row() {
    let mut app = app_with(demo_snapshot(), SIZE);
    open_3(&mut app);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Links);

    // 依據 #4 的提問 5 → #4.
    press(&mut app, &[Key::Down, Key::Down]);
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 4
        }]
    );
    assert_eq!(app.history.len(), 1);
    deliver_card(&mut app, detail(4, "", vec![], vec![]));

    // Back on #3, the same row stays selected after the reload.
    press(&mut app, &[Key::Left]);
    deliver_card(&mut app, detail_3());
    assert_eq!(app.card.as_ref().unwrap().link_sel, 2);

    // 引出 #2 的決定 → #2.
    press(&mut app, &[Key::Down, Key::Down, Key::Down]);
    let fx = press(&mut app, &[Key::Enter]);
    assert_eq!(
        fx,
        vec![Effect::LoadCard {
            board_id: BOARD_ID,
            seq: 2
        }]
    );
}

#[test]
fn the_selection_follows_its_row_when_rows_are_added_before_it() {
    let mut app = app_with(demo_snapshot(), SIZE);
    open_3(&mut app);
    press(&mut app, &[Key::Down, Key::Down, Key::Down, Key::Down]); // 引出 #4 的捨棄
    assert_eq!(app.card.as_ref().unwrap().link_sel, 4);
    let mut d = detail_3();
    d.links.push(card_link(
        9,
        LinkKind::Relates,
        LinkDirection::Outgoing,
        2,
        "更新文件",
    ));
    deliver_card(&mut app, d);
    assert_eq!(app.card.as_ref().unwrap().link_sel, 5);
}

#[test]
fn a_card_with_only_causes_starts_on_the_link_section() {
    let mut app = app_with(demo_snapshot(), SIZE);
    press(&mut app, &[Key::Enter]); // #1
    let mut d = detail(1, "內容", vec![], vec![]);
    d.led_to = vec![led(
        CauseKind::Card,
        None,
        3,
        "修正",
        DecisionKind::Drop,
        None,
    )];
    deliver_card(&mut app, d);
    assert_eq!(app.card.as_ref().unwrap().focus, Section::Links);
}

#[test]
fn feed_lines_name_the_causes() {
    use serde_json::json;
    let line = |verb: &str, detail: serde_json::Value| activity_text(&activity(1, verb, detail));
    assert_eq!(
        line(
            "card.drop",
            json!({"seq": 31, "reason": "x", "because": [{"kind": "card", "seq": 30}]})
        ),
        "#31 捨棄（依據 #30）"
    );
    assert_eq!(
        line(
            "card.note",
            json!({"seq": 24, "kind": "decision", "excerpt": "採用方案 B",
                   "because": [{"kind": "question", "id": 5, "seq": 24}]})
        ),
        "#24 決定（依據 提問 5）採用方案 B"
    );
    assert_eq!(
        line(
            "card.done",
            json!({"seq": 31, "lane": "完成", "result": "x",
                   "because": [{"kind": "card", "seq": 30}, {"kind": "comment", "id": 75, "seq": 26},
                               {"kind": "user"}]})
        ),
        "#31 完成（依據 #30、留言 75、使用者的指示）"
    );
    assert_eq!(
        line(
            "card.drop",
            json!({"seq": 31, "reason": "x", "because": [{"kind": "user"}]})
        ),
        "#31 捨棄（依據 使用者的指示）"
    );
    // Without causes nothing changes.
    assert_eq!(
        line("card.drop", json!({"seq": 31, "reason": "x"})),
        "#31 捨棄"
    );
    assert_eq!(
        line("card.done", json!({"seq": 31, "lane": "完成"})),
        "#31 完成"
    );
    assert_eq!(
        line(
            "card.note",
            json!({"seq": 5, "kind": "decision", "excerpt": "改用滑動視窗"})
        ),
        "#5 紀錄（決定）改用滑動視窗"
    );
}

#[test]
fn the_live_feed_on_the_board_screen_shows_the_causes() {
    let mut app = App::new(SIZE, today(), crate::app::Scope::All, Some(BOARD_ID));
    app.update(Event::BoardLoaded {
        target: crate::app::BoardTarget::Id(BOARD_ID),
        data: Some(BoardData {
            snapshot: demo_snapshot(),
            activity: vec![activity(
                1,
                "card.drop",
                serde_json::json!({"seq": 2, "reason": "x", "because": [{"kind": "card", "seq": 1}]}),
            )],
            findings: Vec::new(),
        }),
        at: std::time::Instant::now(),
    });
    let text = render_text(&app, SIZE.0, SIZE.1);
    assert!(text.contains("#2 捨棄（依據 #1）"), "{text}");
}
