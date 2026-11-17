//! A long open question keeps its text on screen while the cursor moves over its options
//! (docs/questions.md §7.2, card #58): the text is pinned at the top of the question
//! section when the text and the cursor row fit together; otherwise `↑` on the first option
//! scrolls up into the text, and the key hints say so.

use super::v2_state::v2_snapshot;
use super::*;
use crate::layout::{self, Section};
use crate::run::render_text;
use crate::text::wrap;

const W: u16 = 80;

/// Card #3, not blocked, no body, with `questions`.
fn card_with(questions: Vec<QuestionView>) -> CardDetail {
    let mut d = detail(3, "", vec![], vec![]);
    d.card.blocked_reason = None;
    d.questions = questions;
    d
}

/// The card screen of `d` at height `h`, focus on the questions, cursor on the first option.
fn open(d: CardDetail, h: u16) -> App {
    let mut app = app_with(v2_snapshot(), (W, h));
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, d);
    let v = app.card.as_ref().unwrap();
    assert_eq!(v.focus, Section::Questions);
    app
}

fn cursor(app: &App) -> Option<(i64, usize)> {
    app.card.as_ref().unwrap().q_cursor
}

/// The rendered screen's rows.
fn rows(app: &App, h: u16) -> Vec<String> {
    render_text(app, W, h)
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect()
}

/// The non-blank rows of the question section: between its title and the 內容 title.
fn section(app: &App, h: u16) -> Vec<String> {
    let all = rows(app, h);
    let start = all
        .iter()
        .position(|l| l.trim_start().starts_with("提問 ("))
        .expect("question title");
    all[start + 1..]
        .iter()
        .take_while(|l| !l.trim_start().starts_with("內容"))
        .filter(|l| !l.trim().is_empty())
        .cloned()
        .collect()
}

fn footer(app: &App, h: u16) -> String {
    rows(app, h).last().cloned().unwrap_or_default()
}

/// The wrapped lines of question `id`'s text, as the question section shows them.
fn text_lines(id: i64, text: &str) -> Vec<String> {
    wrap(
        &format!("提問 {id}：{text}"),
        layout::card_text_width(W),
    )
}

fn shown(section: &[String], line: &str) -> bool {
    section.iter().any(|l| l.trim() == line.trim())
}

/// About 90 characters: three lines at width 80.
const LONG_TEXT: &str = "這張卡片的回答方式要怎麼定？目前的做法是把每個選項的說明寫得很完整，\
讓人不用回到對話就能判斷，但是說明一長，選項就會占掉好幾行，提問的文字也會換成好幾行，\
畫面不高的時候，游標往下移，提問就看不到了，請選一個做法。";

/// Three options whose descriptions wrap to a second line.
fn long_options() -> [&'static str; 3] {
    [
        "甲方案::把提問的文字固定在提問區的最上面，選項在下面捲動，游標移到哪裡都看得到提問的文字",
        "乙方案::讓游標可以移到提問的文字上，按上鍵就能回到文字，但是游標會停在不能勾選的地方",
        "丙方案::維持現在的做法，提問的文字跟著選項一起捲動，人要記得提問的內容再往下選擇答案",
    ]
}

/// Height 29: the question section gets 5 rows.
const H: u16 = 29;

#[test]
fn the_reported_case_keeps_the_question_text_on_screen() {
    let lines = text_lines(7, LONG_TEXT);
    assert_eq!(lines.len(), 3, "the text wraps to three lines");
    let mut app = open(
        card_with(vec![question(7, 3, LONG_TEXT, false, &long_options())]),
        H,
    );
    assert_eq!(cursor(&app), Some((7, 0)));
    // The section is smaller than the question: it really scrolls.
    let s = section(&app, H);
    assert_eq!(s.len(), 5, "{s:#?}");
    assert!(!shown(&s, "( ) 在 session 裡說明"), "{s:#?}");
    // Down to the last option, then the chat row and the input row: the text stays.
    for want in [(7, 1), (7, 2), (7, 3)] {
        press(&mut app, &[Key::Down]);
        assert_eq!(cursor(&app), Some(want));
        let s = section(&app, H);
        for l in &lines {
            assert!(shown(&s, l), "{want:?}: {l} missing in {s:#?}");
        }
        assert!(s[0].contains("提問 7："), "the text is at the top: {s:#?}");
        assert!(s.iter().any(|l| l.contains('▶')), "the cursor: {s:#?}");
    }
    // On the last option, the option is fully shown under the text.
    press(&mut app, &[Key::Up]);
    assert_eq!(cursor(&app), Some((7, 2)));
    let s = section(&app, H);
    assert!(s[3].contains("▶") && s[3].contains("丙方案"), "{s:#?}");
    assert!(s[4].contains("答案"), "{s:#?}");
    // No extra key hint: nothing is hidden.
    assert!(footer(&app, H).contains("↑↓ 選擇"));
    assert!(!footer(&app, H).contains("看提問"));
    // Back up to the first option: the question is shown in its place again.
    press(&mut app, &[Key::Up, Key::Up]);
    assert_eq!(cursor(&app), Some((7, 0)));
    let s = section(&app, H);
    assert!(s[0].contains("提問 7："), "{s:#?}");
    assert!(s[3].contains("▶") && s[3].contains("甲方案"), "{s:#?}");
}

#[test]
fn a_question_that_cannot_fit_can_be_read_with_the_arrow_keys() {
    // A text of nine lines in a five-row section.
    let text = LONG_TEXT.repeat(3);
    let lines = text_lines(7, &text);
    assert!(lines.len() > 6, "{}", lines.len());
    let mut app = open(
        card_with(vec![question(7, 3, &text, false, &long_options())]),
        H,
    );
    let mut seen: Vec<String> = Vec::new();
    // Down to the input row.
    for _ in 0..4 {
        press(&mut app, &[Key::Down]);
        seen.extend(section(&app, H));
    }
    assert_eq!(cursor(&app), Some((7, 4)));
    // Up to the first option: the cursor stays visible; the footer names the keys.
    for _ in 0..4 {
        press(&mut app, &[Key::Up]);
        let s = section(&app, H);
        assert!(s.iter().any(|l| l.contains('▶')), "{s:#?}");
        seen.extend(s);
    }
    assert_eq!(cursor(&app), Some((7, 0)));
    let s = section(&app, H);
    assert!(!shown(&s, &lines[0]), "the top of the text is scrolled out");
    assert!(footer(&app, H).contains("↑↓ 選擇／看提問"), "{}", footer(&app, H));
    // ↑ on the first option scrolls up into the text, line by line, to its first line.
    let mut ups = 0;
    while !shown(&section(&app, H), &lines[0]) {
        press(&mut app, &[Key::Up]);
        assert_eq!(cursor(&app), Some((7, 0)), "the cursor stays");
        seen.extend(section(&app, H));
        ups += 1;
        assert!(ups <= lines.len(), "the first line is never reached");
    }
    assert!(section(&app, H)[0].contains("提問 7："));
    for l in &lines {
        assert!(shown(&seen, l), "{l} never shown");
    }
    // ↓ scrolls back until the cursor row is fully shown, then moves the cursor.
    let mut downs = 0;
    while !section(&app, H).iter().any(|l| l.contains('▶')) {
        press(&mut app, &[Key::Down]);
        assert_eq!(cursor(&app), Some((7, 0)));
        downs += 1;
        assert!(downs <= lines.len());
    }
    press(&mut app, &[Key::Down]);
    assert_eq!(cursor(&app), Some((7, 0)), "the second line of the row");
    press(&mut app, &[Key::Down]);
    assert_eq!(cursor(&app), Some((7, 1)));
    // Other keys work as before after scrolling into the text.
    press(&mut app, &[Key::Up, Key::Up, Key::Up, Key::Char(' ')]);
    assert_eq!(cursor(&app), Some((7, 0)));
    assert!(app.marks[&7].options.contains(&71));
    let s = section(&app, H);
    assert!(
        s.iter().any(|l| l.contains('▶') && l.contains("甲方案")),
        "Space shows the cursor again: {s:#?}"
    );
}

#[test]
fn with_two_questions_the_text_of_the_question_under_the_cursor_is_kept() {
    let second = "第二題：選項的說明要不要也顯示在已回答的提問裡？這會讓已回答的提問變成好幾行，\
但是人回頭看的時候比較清楚當初選的是什麼，請選一個做法。";
    let mut app = open(
        card_with(vec![
            question(7, 3, LONG_TEXT, false, &long_options()),
            question(8, 3, second, false, &long_options()),
        ]),
        H,
    );
    let first = text_lines(7, LONG_TEXT);
    let lines = text_lines(8, second);
    // Down to question 8's last option.
    for _ in 0..7 {
        press(&mut app, &[Key::Down]);
    }
    assert_eq!(cursor(&app), Some((8, 2)));
    let s = section(&app, H);
    for l in &lines {
        assert!(shown(&s, l), "{l} missing in {s:#?}");
    }
    assert!(!shown(&s, &first[0]), "question 7's text is not kept: {s:#?}");
    // Back up into question 7: its text again.
    for _ in 0..5 {
        press(&mut app, &[Key::Up]);
    }
    assert_eq!(cursor(&app), Some((7, 2)));
    let s = section(&app, H);
    for l in &first {
        assert!(shown(&s, l), "{l} missing in {s:#?}");
    }
    assert!(!shown(&s, &lines[0]), "{s:#?}");
}

#[test]
fn a_short_question_renders_as_before() {
    let mut app = open(
        card_with(vec![
            question(7, 3, "token 的有效期？", false, &["15 分鐘", "30 分鐘"]),
            question(8, 3, "哪些守則維持阻擋？", true, &["R1", "R2", "R4"]),
        ]),
        40,
    );
    let want = |c: usize| -> Vec<String> {
        let rows = [
            "    提問 7：token 的有效期？",
            "    ( ) 15 分鐘",
            "    ( ) 30 分鐘",
            "    ( ) 在 session 裡說明",
            "    輸入：（按 i 或移到這裡打字）",
            "    提問 8：哪些守則維持阻擋？（可複選）",
            "    [ ] R1",
            "    [ ] R2",
            "    [ ] R4",
            "    [ ] 在 session 裡說明",
            "    輸入：（按 i 或移到這裡打字）",
        ];
        rows.iter()
            .enumerate()
            .map(|(i, r)| {
                if i == c {
                    r.replacen("    ", "  ▶ ", 1)
                } else {
                    r.to_string()
                }
            })
            .collect()
    };
    assert_eq!(section(&app, 40), want(1));
    press(&mut app, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(cursor(&app), Some((8, 0)));
    assert_eq!(section(&app, 40), want(6));
    assert!(!footer(&app, 40).contains("看提問"));
}

#[test]
fn closed_questions_and_other_sections_are_unchanged() {
    let mut closed = question(9, 3, LONG_TEXT, false, &long_options());
    closed.question.status = kb_core::model::QuestionStatus::Answered;
    let body = (1..=20)
        .map(|i| format!("第 {i} 行"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut d = card_with(vec![closed]);
    d.card.body = body;
    let mut app = open_any(d, H);
    // A closed question is one dim line; the focus is not on the questions.
    let s = section(&app, H);
    assert_eq!(s.len(), 1, "{s:#?}");
    assert!(s[0].contains("提問 9："), "{s:#?}");
    // The body still scrolls line by line.
    while app.card.as_ref().unwrap().focus != Section::Body {
        press(&mut app, &[Key::Tab]);
    }
    press(&mut app, &[Key::Down, Key::Down]);
    assert_eq!(app.card.as_ref().unwrap().body_scroll, 2);
    assert!(footer(&app, H).contains("↑↓ 捲動"));
}

/// Like [`open`], without a focus check.
fn open_any(d: CardDetail, h: u16) -> App {
    let mut app = app_with(v2_snapshot(), (W, h));
    press(&mut app, &[Key::Right, Key::Enter]);
    deliver_card(&mut app, d);
    app
}
