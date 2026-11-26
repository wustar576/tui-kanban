//! Screen geometry shared by `update` (to keep selections visible and clamp scrolling)
//! and `view` (to draw). Pure functions of the screen size, the data and the state.

use std::collections::HashMap;

use chrono::NaiveDate;
use kb_core::model::{CauseKind, DecisionKind, LaneKind, Note, NoteKind, Origin, QuestionStatus};
use kb_core::read::{ActiveFinding, CardDetail, CardSummary, JudgeTag, QuestionView};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::Marks;
use crate::markdown;
use crate::text::{
    causes_text, fit, fmt_time, link_label, note_kind_label, one_line, pad, truncate, width, wrap,
    wrap_max,
};

/// Below this size only 「視窗太小」 is shown (docs/tui.md section 9).
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 15;
/// Side panel width, and the screen width below which it hides (docs/board-v2.md 5.2).
pub const SIDE_WIDTH: u16 = 24;
pub const SIDE_MIN_SCREEN: u16 = 80;
/// Width of a `done` / `dropped` lane that is not selected (docs/board-v2.md 5.3).
pub const NARROW_WIDTH: u16 = 14;
/// Minimum width of the other lanes.
pub const MIN_LANE_WIDTH: u16 = 26;
/// Activity rows at the bottom of the board screen.
pub const STRIP_ROWS: u16 = 3;
/// Title lines of a card box at most (docs/board-v2.md 5.4).
pub const TITLE_LINES: usize = 3;
/// Width of the connection status in the card screen's top border (`" ● 已連線 "`).
pub const STATUS_WIDTH: usize = 10;
/// Blue (not bright blue, which marks the selected epic) is used only for comments
/// (docs/board-v2.md section 13.16): the 留言 tag and note kind, 未回覆, the header count.
pub const COMMENT_COLOR: Color = Color::Blue;
pub const COMMENT_TAG_STYLE: Style = Style::new().fg(Color::White).bg(COMMENT_COLOR);
/// The model judge's tags (docs/tui.md section 12.2) carry no hue: every hue already has a
/// meaning. 等回覆 is black on light gray (also the header indicator); 建議未記錄 is
/// underlined text in the default color. Both stay readable on light and dark themes.
pub const WAITING_STYLE: Style = Style::new().fg(Color::Black).bg(Color::Gray);
pub const ADVICE_STYLE: Style = Style::new().add_modifier(Modifier::UNDERLINED);

/// The model judge's active findings on one card (docs/tui.md section 12): the
/// probability per question, `None` when that tag is not shown.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CardJudge {
    pub waiting: Option<f64>,
    pub advice: Option<f64>,
}

impl CardJudge {
    /// The active findings of card `card_id` among a board's.
    pub fn of(findings: &[ActiveFinding], card_id: i64) -> CardJudge {
        let mut out = CardJudge::default();
        // Oldest first, so the latest wins (there is at most one per question).
        for f in findings.iter().filter(|f| f.card_id == Some(card_id)) {
            match f.tag {
                JudgeTag::Waiting => out.waiting = Some(f.p),
                JudgeTag::Advice => out.advice = Some(f.p),
            }
        }
        out
    }

    pub fn any(&self) -> bool {
        self.waiting.is_some() || self.advice.is_some()
    }

    /// The lines the card screen shows under the blocked row: label and explanation.
    pub fn detail_lines(&self) -> Vec<(String, Style, String)> {
        let mut out = Vec::new();
        if let Some(p) = self.waiting {
            out.push((
                format!("{}：", judge_label(JudgeTag::Waiting)),
                WAITING_STYLE.add_modifier(Modifier::BOLD),
                judge_explanation(JudgeTag::Waiting, p),
            ));
        }
        if let Some(p) = self.advice {
            out.push((
                format!("{}：", judge_label(JudgeTag::Advice)),
                ADVICE_STYLE.add_modifier(Modifier::BOLD),
                judge_explanation(JudgeTag::Advice, p),
            ));
        }
        out
    }
}

/// The label of a model-judge tag (docs/tui.md section 12.2). Also in
/// `kb board show --json --findings` (docs/ui-contract.md section 5).
pub fn judge_label(tag: JudgeTag) -> &'static str {
    match tag {
        JudgeTag::Waiting => "等回覆",
        JudgeTag::Advice => "建議未記錄",
    }
}

/// The explanation after the label on the card screen, with the probability (docs/tui.md
/// section 12.4). Also in `kb board show --json --findings` (docs/ui-contract.md section 5).
pub fn judge_explanation(tag: JudgeTag, p: f64) -> String {
    match tag {
        JudgeTag::Waiting => format!("agent 在對話裡等你回覆（評測 {p:.2}）"),
        JudgeTag::Advice => {
            format!("agent 的回覆對這張卡片提出建議或決定，卡片沒有紀錄（評測 {p:.2}）")
        }
    }
}

pub fn too_small(size: (u16, u16)) -> bool {
    size.0 < MIN_WIDTH || size.1 < MIN_HEIGHT
}

/// Whether the side panel is on screen: wanted, and the screen is wide enough.
pub fn side_shown(size: (u16, u16), wanted: bool) -> bool {
    wanted && size.0 >= SIDE_MIN_SCREEN
}

/// The board screen below the header row and above the key bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardAreas {
    pub side: Option<Rect>,
    pub lanes: Rect,
    /// The last activity entries: always the full width, under the side panel and lanes.
    pub strip: Option<Rect>,
}

pub fn board_areas(size: (u16, u16), side: bool) -> BoardAreas {
    let (w, h) = size;
    let mid = h.saturating_sub(2);
    let lanes_h = mid.saturating_sub(STRIP_ROWS);
    let strip = Some(Rect::new(0, 1 + lanes_h, w, mid - lanes_h));
    if side {
        BoardAreas {
            side: Some(Rect::new(0, 1, SIDE_WIDTH, lanes_h)),
            lanes: Rect::new(SIDE_WIDTH, 1, w.saturating_sub(SIDE_WIDTH), lanes_h),
            strip,
        }
    } else {
        BoardAreas {
            side: None,
            lanes: Rect::new(0, 1, w, lanes_h),
            strip,
        }
    }
}

// ----- lanes ----------------------------------------------------------------------------

/// One lane on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneSlot {
    pub index: usize,
    pub x: u16,
    pub width: u16,
    /// A `done` / `dropped` lane that is not selected: 14 columns, one row per card.
    pub narrow: bool,
}

/// The lanes that fit side by side, left to right.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneStrip {
    pub slots: Vec<LaneSlot>,
    /// Index of the first visible lane.
    pub first: usize,
    /// Not all lanes fit: `‹` / `›` on the outer borders show hidden lanes.
    pub scrolling: bool,
    pub more_left: bool,
    pub more_right: bool,
}

/// Lane widths (docs/board-v2.md section 5.3): `done` and `dropped` lanes are 14 columns
/// unless selected; the other lanes share the rest, at least 26 columns each. When they
/// do not fit, the strip scrolls so that the selected lane is visible, starting from
/// `offset` (the previous first lane) when possible.
pub fn lane_strip(area: Rect, kinds: &[LaneKind], sel: usize, offset: usize) -> LaneStrip {
    let n = kinds.len();
    if n == 0 {
        return LaneStrip::default();
    }
    let sel = sel.min(n - 1);
    let narrow: Vec<bool> = kinds
        .iter()
        .enumerate()
        .map(|(i, k)| k.is_closed() && i != sel)
        .collect();
    let base = |i: usize| -> usize {
        if narrow[i] {
            NARROW_WIDTH as usize
        } else {
            MIN_LANE_WIDTH as usize
        }
    };
    let sum = |a: usize, b: usize| (a..=b).map(base).sum::<usize>();
    let w = area.width as usize;
    // When scrolling, `‹` / `›` are drawn on the outer lane borders, so no columns are
    // reserved for them.
    let avail = w;
    let (first, last, scrolling) = if sum(0, n - 1) <= w {
        (0, n - 1, false)
    } else {
        let mut first = offset.min(sel);
        while first < sel && sum(first, sel) > avail {
            first += 1;
        }
        let mut last = sel;
        while last + 1 < n && sum(first, last + 1) <= avail {
            last += 1;
        }
        while first > 0 && sum(first - 1, last) <= avail {
            first -= 1;
        }
        (first, last, true)
    };
    let x0 = area.x;
    let used = sum(first, last);
    let normals: Vec<usize> = (first..=last).filter(|i| !narrow[*i]).collect();
    let extra = avail.saturating_sub(used);
    let (each, rem) = match normals.len() {
        0 => (0, 0),
        k => (extra / k, extra % k),
    };
    let mut slots = Vec::new();
    let mut x = x0;
    for (i, &is_narrow) in narrow.iter().enumerate().take(last + 1).skip(first) {
        let mut wi = base(i);
        if let Some(k) = normals.iter().position(|j| *j == i) {
            wi += each + usize::from(k < rem);
        }
        if used > avail {
            // Only the selected lane, and even it does not fit: give it everything.
            wi = avail;
        }
        slots.push(LaneSlot {
            index: i,
            x,
            width: wi as u16,
            narrow: is_narrow,
        });
        x += wi as u16;
    }
    LaneStrip {
        slots,
        first,
        scrolling,
        more_left: first > 0,
        more_right: last + 1 < n,
    }
}

// ----- card boxes -----------------------------------------------------------------------

/// Text width of a card's title in a lane `lane_w` wide: lane border (2), card border (2),
/// the `▶` column and a right margin.
pub fn card_text_width_in(lane_w: u16) -> usize {
    (lane_w as usize).saturating_sub(6).max(1)
}

/// The title lines of a card box: `#編號 標題`, wrapped, at most three lines.
pub fn card_title_lines(card: &CardSummary, lane_w: u16) -> Vec<String> {
    let c = &card.card;
    wrap_max(
        &format!("#{} {}", c.seq, one_line(&c.title)),
        card_text_width_in(lane_w),
        TITLE_LINES,
    )
}

/// The tags row shows the epic name and the tags; it is left out when it would be empty.
pub fn has_tags_row(card: &CardSummary, judge: CardJudge) -> bool {
    card.epic_name.is_some()
        || card.open_question_count > 0
        || card.unreplied_comment_count > 0
        || card.card.blocked_reason.is_some()
        || card.card.origin != Origin::Agreed
        || judge.any()
}

/// A relation shown on a card (docs/board-v2.md section 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// `↳ 延伸自 #n`
    SpawnedFrom,
    /// `↳ 被 #n 阻擋` (`…` when more cards block it)
    BlockedBy,
    /// `→ N`
    Spawns,
}

/// The relation items of a card, in order.
pub fn relation_items(card: &CardSummary) -> Vec<(Relation, String)> {
    let mut out = Vec::new();
    if let Some(from) = card.spawned_from {
        out.push((Relation::SpawnedFrom, format!("↳ 延伸自 #{from}")));
    }
    if let Some(b) = card.blocked_by.first() {
        let more = if card.blocked_by.len() > 1 { "…" } else { "" };
        out.push((Relation::BlockedBy, format!("↳ 被 #{b} 阻擋{more}")));
    }
    if card.spawned_count > 0 {
        out.push((Relation::Spawns, format!("→ {}", card.spawned_count)));
    }
    out
}

pub fn has_relation_row(card: &CardSummary) -> bool {
    !relation_items(card).is_empty()
}

/// The relation items packed into rows of the card's text width, two spaces apart. An
/// item that does not fit after another one starts a new row, so no item is cut (unless
/// it is wider than a row by itself).
pub fn relation_lines(card: &CardSummary, lane_w: u16) -> Vec<Vec<(Relation, String)>> {
    let w = card_text_width_in(lane_w);
    let mut lines: Vec<Vec<(Relation, String)>> = Vec::new();
    let mut used = 0;
    for (rel, text) in relation_items(card) {
        let tw = width(&text);
        match lines.last_mut() {
            Some(line) if used + 2 + tw <= w => {
                used += 2 + tw;
                line.push((rel, text));
            }
            _ => {
                used = tw;
                lines.push(vec![(rel, text)]);
            }
        }
    }
    lines
}

/// Rows of a card box in a lane `lane_w` wide (borders included).
pub fn card_height(card: &CardSummary, lane_w: u16, judge: CardJudge) -> u16 {
    2 + card_title_lines(card, lane_w).len() as u16
        + u16::from(has_tags_row(card, judge))
        + relation_lines(card, lane_w).len() as u16
}

/// Heights of the cards of a lane, as drawn; `findings` are the board's active model-judge
/// findings.
pub fn card_heights(
    cards: &[CardSummary],
    lane_w: u16,
    narrow: bool,
    findings: &[ActiveFinding],
) -> Vec<u16> {
    cards
        .iter()
        .map(|c| {
            if narrow {
                1
            } else {
                card_height(c, lane_w, CardJudge::of(findings, c.card.id))
            }
        })
        .collect()
}

/// The cards that fit in `room` rows: `(first, end)`, end exclusive. The selected card is
/// always included; the window starts at `offset` when possible and never leaves empty
/// room at the bottom while cards are hidden above.
pub fn card_window(
    heights: &[u16],
    room: u16,
    sel: Option<usize>,
    offset: usize,
) -> (usize, usize) {
    let n = heights.len();
    if n == 0 {
        return (0, 0);
    }
    let room = room as usize;
    let sum = |a: usize, b: usize| heights[a..b].iter().map(|h| *h as usize).sum::<usize>();
    let mut first = offset.min(n - 1);
    if let Some(s) = sel {
        let s = s.min(n - 1);
        if s < first {
            first = s;
        }
        while first < s && sum(first, s + 1) > room {
            first += 1;
        }
    }
    let mut end = first + 1;
    while end < n && sum(first, end + 1) <= room {
        end += 1;
    }
    while first > 0 && sum(first - 1, end) <= room {
        first -= 1;
    }
    (first, end)
}

// ----- card screen ----------------------------------------------------------------------

/// The focusable sections of the card screen, top to bottom (docs/board-v2.md 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Questions,
    Body,
    Links,
    Notes,
}

impl Section {
    pub const ORDER: [Section; 4] = [
        Section::Questions,
        Section::Body,
        Section::Links,
        Section::Notes,
    ];

    /// The sections on screen: 提問 only when the card has questions.
    pub fn shown(with_questions: bool) -> &'static [Section] {
        if with_questions {
            &Section::ORDER
        } else {
            &Section::ORDER[1..]
        }
    }

    fn step(self, with_questions: bool, delta: isize) -> Section {
        let shown = Section::shown(with_questions);
        let n = shown.len() as isize;
        let i = shown.iter().position(|s| *s == self).unwrap_or(0) as isize;
        shown[((i + delta).rem_euclid(n)) as usize]
    }

    pub fn next(self, with_questions: bool) -> Section {
        self.step(with_questions, 1)
    }

    pub fn prev(self, with_questions: bool) -> Section {
        self.step(with_questions, -1)
    }
}

/// What the question section needs from the screen state.
#[derive(Debug, Clone, Copy)]
pub struct QuestionUi<'a> {
    pub marks: &'a HashMap<i64, Marks>,
    /// (question id, choice index); the last choice of a question is 「在 session 裡說明」.
    pub cursor: Option<(i64, usize)>,
    pub focused: bool,
}

/// A choice of an open question and the lines it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChoiceRow {
    pub question: i64,
    pub index: usize,
    pub first_line: usize,
    pub lines: usize,
    /// First line of the question's text, above its options.
    pub head: usize,
    /// Lines of the question's text.
    pub text: usize,
}

impl ChoiceRow {
    /// Whether the question's text and this row fit in a window of `h` lines together.
    pub fn fits_with_text(&self, h: usize) -> bool {
        self.text + self.lines <= h.max(1)
    }

    /// Whether the question's text is pinned at the top of a window of `h` lines whose
    /// first line is `offset` (docs/questions.md §7.2): the text and this row fit together,
    /// and the top of the text is scrolled out. The window then shows the text, then the
    /// lines from `offset + text`.
    pub fn pinned(&self, offset: usize, h: usize) -> bool {
        offset > self.head && self.fits_with_text(h)
    }
}

/// The question section's first visible line with the cursor on `row`, from the previous
/// first line `offset`, in a window of `h` lines out of `total` (docs/questions.md §7.2).
/// The cursor row is always visible. When the question's text and the cursor row fit in
/// the window together, the text is visible too: in its place when the question fits up
/// to the cursor row, otherwise pinned at the top of the window (see
/// [`ChoiceRow::pinned`]). `peek`: the person scrolled up into the text with `↑` on the
/// question's first option; the offset is kept.
pub fn question_scroll(
    offset: usize,
    row: &ChoiceRow,
    h: usize,
    total: usize,
    peek: bool,
) -> usize {
    let h = h.max(1);
    let mut off = offset;
    if !peek {
        let end = row.first_line + row.lines;
        // With the text pinned, the rows below it start at `off + text`.
        let top = if row.fits_with_text(h) { row.text } else { 0 };
        if row.first_line < off + top {
            off = row.first_line - top;
        } else if end > off + h {
            off = end - h;
        }
        if end - row.head <= h {
            off = off.min(row.head);
        }
    }
    off.min(total.saturating_sub(h))
}

/// Indexes of the lines the question section shows, `h` rows from `offset`, with the
/// cursor on `row` (docs/questions.md §7.2).
pub fn question_window(
    offset: usize,
    row: Option<&ChoiceRow>,
    h: usize,
    total: usize,
) -> Vec<usize> {
    match row.filter(|r| r.pinned(offset, h)) {
        Some(r) => (r.head..r.head + r.text)
            .chain((offset + r.text..offset + h).take_while(|i| *i < total))
            .collect(),
        None => (offset..offset + h).take_while(|i| *i < total).collect(),
    }
}

/// Lines of the card screen's sections, wrapped to the box width.
pub struct CardContent {
    /// The full title, wrapped, when it does not fit in the top border.
    pub title: Vec<String>,
    pub questions: Vec<Line<'static>>,
    pub choices: Vec<ChoiceRow>,
    pub body: Vec<Line<'static>>,
    /// One entry per link: (direction text, target text, label).
    pub links: Vec<(String, String, Option<String>)>,
    pub notes: Vec<Line<'static>>,
}

impl CardContent {
    /// Natural number of lines of a section.
    pub fn lines(&self, s: Section) -> usize {
        match s {
            Section::Questions => self.questions.len(),
            Section::Body => self.body.len(),
            Section::Links => self.links.len().max(1),
            Section::Notes => self.notes.len(),
        }
    }
}

const DIM: Style = Style::new().fg(Color::DarkGray);

// ----- open layout of the card and epic screens (docs/board-v2.md section 6.0) ----------
//
// ALL MARGINS of the open layout live here, so they can be changed in one place.

/// Empty columns at the left and right edges. Titles, rules, `▶` and the key hints start
/// in this column; rules end this many cells before the right edge; the connection
/// status is right-aligned with the same margin.
pub const SIDE_MARGIN: u16 = 2;
/// Indent of section content from the side margin (content starts in column
/// `SIDE_MARGIN + CONTENT_INDENT`).
pub const CONTENT_INDENT: u16 = 2;
/// Blank rows above the screen title.
pub const TOP_PADDING: u16 = 1;
/// Blank rows between the screen title and the dim info row.
pub const TITLE_GAP: u16 = 1;
/// Blank rows above and below each section title row.
pub const SECTION_GAP_ABOVE: u16 = 2;
pub const SECTION_GAP_BELOW: u16 = 1;
/// Screens shorter than this use the short values below: no top padding and one blank
/// row above section titles; every other blank row stays (docs/board-v2.md 6.0).
pub const SHORT_SCREEN: u16 = 24;
pub const SHORT_TOP_PADDING: u16 = 0;
pub const SHORT_TITLE_GAP: u16 = 1;
pub const SHORT_SECTION_GAP_ABOVE: u16 = 1;
pub const SHORT_SECTION_GAP_BELOW: u16 = 1;
/// Display width of the connection status (`● 已連線`).
pub const STATUS_TEXT_WIDTH: usize = 8;

/// The vertical gaps for a screen of `size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gaps {
    pub top: u16,
    pub title: u16,
    pub above: u16,
    pub below: u16,
}

pub fn gaps(size: (u16, u16)) -> Gaps {
    if size.1 < SHORT_SCREEN {
        Gaps {
            top: SHORT_TOP_PADDING,
            title: SHORT_TITLE_GAP,
            above: SHORT_SECTION_GAP_ABOVE,
            below: SHORT_SECTION_GAP_BELOW,
        }
    } else {
        Gaps {
            top: TOP_PADDING,
            title: TITLE_GAP,
            above: SECTION_GAP_ABOVE,
            below: SECTION_GAP_BELOW,
        }
    }
}

/// Width between the side margins.
pub fn inner_width(screen_w: u16) -> usize {
    screen_w.saturating_sub(2 * SIDE_MARGIN).max(1) as usize
}

/// Width of section content: between the side margins, after the content indent.
pub fn card_text_width(screen_w: u16) -> usize {
    inner_width(screen_w)
        .saturating_sub(CONTENT_INDENT as usize)
        .max(1)
}

/// The content indent; [`marker`] takes its place on a selected row.
pub fn indent() -> String {
    " ".repeat(CONTENT_INDENT as usize)
}

/// `▶` at the side margin, then the rest of the content indent.
pub fn marker() -> String {
    format!(
        "▶{}",
        " ".repeat((CONTENT_INDENT as usize).saturating_sub(1))
    )
}

/// THE SWITCH for section titles on the card and epic screens: `true` draws a dim `─`
/// rule after the title, to the right margin (the user's choice).
pub const SECTION_RULE: bool = true;

/// The look of the open card and epic screens: the section-title rule and the table
/// style. `App::look` starts from [`SECTION_RULE`] and [`markdown::TABLE_STYLE`]; tests
/// set other values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub section_rule: bool,
    pub tables: markdown::TableStyle,
}

impl Default for Look {
    fn default() -> Look {
        Look {
            section_rule: SECTION_RULE,
            tables: markdown::TABLE_STYLE,
        }
    }
}

/// Title rows of the open card and epic screens: bold, wrapped (at most three lines),
/// leaving room for the connection status (and two blanks) at the right of the first row.
pub fn screen_title_lines(title: &str, screen_w: u16) -> Vec<String> {
    let w = inner_width(screen_w)
        .saturating_sub(STATUS_TEXT_WIDTH + 2)
        .max(1);
    wrap_max(&one_line(title), w, TITLE_LINES)
}

/// Rows a section title takes: the blank rows above it, the title, the blank rows
/// below it.
pub fn section_title_rows(size: (u16, u16)) -> usize {
    let g = gaps(size);
    (g.above + 1 + g.below) as usize
}

/// The choice index of a question's input row (docs/board-v2.md section 7.1): after the
/// options and 「在 session 裡說明」.
pub fn input_index(q: &QuestionView) -> usize {
    q.options.len() + 1
}

/// The choices of the open questions, in order: (question id, choice index). The options,
/// then 「在 session 裡說明」, then the input row, which the cursor cannot reach while
/// 「在 session 裡說明」 is marked.
pub fn choices(questions: &[QuestionView], marks: &HashMap<i64, Marks>) -> Vec<(i64, usize)> {
    questions
        .iter()
        .filter(|q| q.is_open())
        .flat_map(|q| {
            let id = q.question.id;
            let chat = marks.get(&id).is_some_and(|m| m.chat);
            let last = if chat {
                q.options.len()
            } else {
                input_index(q)
            };
            (0..=last).map(move |i| (id, i))
        })
        .collect()
}

pub const CHAT_LABEL: &str = "在 session 裡說明";
/// The input row (docs/board-v2.md section 7.1).
pub const INPUT_LABEL: &str = "輸入：";
pub const INPUT_PLACEHOLDER: &str = "（按 i 或移到這裡打字）";
pub const INPUT_DISABLED: &str = "（改在 session 裡說明）";
/// The text cursor shown after (or inside) the typed text.
pub const INPUT_CURSOR: &str = "▏";
/// The most characters an answer text may have.
pub const TEXT_LIMIT: usize = 500;

/// Whether choice `index` of question `q` is marked (the input row never is).
pub fn is_marked(q: &QuestionView, marks: Option<&Marks>, index: usize) -> bool {
    let Some(m) = marks else {
        return false;
    };
    match q.options.get(index) {
        Some(o) => m.options.contains(&o.id),
        None => index == q.options.len() && m.chat,
    }
}

/// The lines of a question's input row, `tw` cells wide after the content indent:
/// `輸入：` and the text, wrapped by display width character by character (spaces kept),
/// continuation lines aligned with the first text cell. On the cursor, `▶` sits at the
/// side margin and `▏` marks the text cursor.
fn input_lines(m: Option<&Marks>, on_cursor: bool, tw: usize) -> Vec<Line<'static>> {
    let prefix = if on_cursor { marker() } else { indent() };
    let label_style = if on_cursor {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    let chat = m.is_some_and(|m| m.chat);
    let text = m.map_or("", |m| m.text.as_str());
    if chat || (text.is_empty() && !on_cursor) {
        let note = if chat {
            INPUT_DISABLED
        } else {
            INPUT_PLACEHOLDER
        };
        return vec![Line::from(vec![
            Span::raw(prefix),
            Span::styled(format!("{INPUT_LABEL}{note}"), DIM),
        ])];
    }
    let label_w = width(INPUT_LABEL);
    let w = tw.saturating_sub(label_w).max(2);
    let cursor = m.map_or(0, |m| m.text_cursor.min(text.chars().count()));
    // Pieces: the characters, with the cursor glyph at its position when on the row.
    let mut pieces: Vec<(String, Style)> = Vec::new();
    let cursor_style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    for (i, c) in text.chars().enumerate() {
        if on_cursor && i == cursor {
            pieces.push((INPUT_CURSOR.to_string(), cursor_style));
        }
        pieces.push((c.to_string(), Style::new()));
    }
    if on_cursor && cursor >= text.chars().count() {
        pieces.push((INPUT_CURSOR.to_string(), cursor_style));
    }
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    for (s, style) in pieces {
        let sw = width(&s);
        if used + sw > w && used > 0 {
            rows.push(Vec::new());
            used = 0;
        }
        let row = rows.last_mut().expect("at least one row");
        match row.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(&s),
            _ => row.push(Span::styled(s, style)),
        }
        used += sw;
    }
    rows.into_iter()
        .enumerate()
        .map(|(k, row)| {
            let mut spans = if k == 0 {
                vec![
                    Span::raw(prefix.clone()),
                    Span::styled(INPUT_LABEL, label_style),
                ]
            } else {
                vec![Span::raw(format!("{}{}", indent(), " ".repeat(label_w)))]
            };
            spans.extend(row);
            Line::from(spans)
        })
        .collect()
}

/// The answer of a closed question as one line: `已回答：選項｜文字`.
fn answered_text(q: &QuestionView) -> String {
    let labels = q.selected_labels().join("、");
    let text = q
        .question
        .answer_text
        .as_deref()
        .map(one_line)
        .filter(|t| !t.trim().is_empty());
    match (labels.is_empty(), text) {
        (_, None) => format!("已回答：{labels}"),
        (true, Some(t)) => format!("已回答：{}", t.trim()),
        (false, Some(t)) => format!("已回答：{labels}｜{}", t.trim()),
    }
}

fn question_lines(
    qs: &[QuestionView],
    ui: QuestionUi,
    tw: usize,
) -> (Vec<Line<'static>>, Vec<ChoiceRow>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows = Vec::new();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    for q in qs {
        let id = q.question.id;
        if !q.is_open() {
            let status = match q.question.status {
                QuestionStatus::Answered => answered_text(q),
                QuestionStatus::Chat => CHAT_LABEL.to_string(),
                QuestionStatus::Cancelled => "已取消".to_string(),
                QuestionStatus::Open => String::new(),
            };
            let text = format!(
                "{}提問 {id}：{}  {status}",
                indent(),
                one_line(&q.question.text)
            );
            lines.push(Line::from(Span::styled(
                truncate(&text, tw + CONTENT_INDENT as usize),
                DIM,
            )));
            continue;
        }
        let head = lines.len();
        let multi = if q.question.multi {
            "（可複選）"
        } else {
            ""
        };
        for l in wrap(
            &format!("提問 {id}：{}{multi}", one_line(&q.question.text)),
            tw,
        ) {
            lines.push(Line::from(vec![
                Span::raw(indent()),
                Span::styled(l, bold.fg(Color::Yellow)),
            ]));
        }
        let text_rows = lines.len() - head;
        let marks = ui.marks.get(&id);
        for index in 0..=q.options.len() {
            let marked = is_marked(q, marks, index);
            let mark = match (q.question.multi, marked) {
                (true, true) => "[x]",
                (true, false) => "[ ]",
                (false, true) => "(•)",
                (false, false) => "( )",
            };
            let (label, desc) = match q.options.get(index) {
                Some(o) => (
                    one_line(&o.label),
                    o.description
                        .as_deref()
                        .map(one_line)
                        .filter(|d| !d.is_empty()),
                ),
                None => (CHAT_LABEL.to_string(), None),
            };
            let text = match &desc {
                Some(d) => format!("{label} — {d}"),
                None => label.clone(),
            };
            let on_cursor = ui.focused && ui.cursor == Some((id, index));
            let base = if on_cursor {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            let label_w = width(&label);
            let wrapped = wrap(&text, tw.saturating_sub(4).max(1));
            let first_line = lines.len();
            for (k, l) in wrapped.iter().enumerate() {
                // `▶` at the side margin marks the cursor; the reverse video covers the text.
                let mut spans = if k == 0 {
                    vec![
                        Span::raw(if on_cursor { marker() } else { indent() }),
                        Span::styled(
                            format!("{mark} "),
                            if marked {
                                base.fg(Color::Yellow).add_modifier(Modifier::BOLD)
                            } else {
                                base
                            },
                        ),
                    ]
                } else {
                    vec![Span::raw(indent()), Span::styled("    ", base)]
                };
                // The label part in the default color, the description dim.
                if k == 0 && width(l) > label_w {
                    let (a, b) = split_at_width(l, label_w);
                    spans.push(Span::styled(
                        a,
                        if marked {
                            base.add_modifier(Modifier::BOLD)
                        } else {
                            base
                        },
                    ));
                    spans.push(Span::styled(b, base.patch(DIM)));
                } else if k == 0 {
                    spans.push(Span::styled(
                        l.clone(),
                        if marked {
                            base.add_modifier(Modifier::BOLD)
                        } else {
                            base
                        },
                    ));
                } else if desc.is_some() {
                    spans.push(Span::styled(l.clone(), base.patch(DIM)));
                } else {
                    spans.push(Span::styled(l.clone(), base));
                }
                lines.push(Line::from(spans));
            }
            rows.push(ChoiceRow {
                question: id,
                index,
                first_line,
                lines: wrapped.len(),
                head,
                text: text_rows,
            });
        }
        // The input row, the question's last row.
        let index = input_index(q);
        let on_cursor = ui.focused && ui.cursor == Some((id, index));
        let first_line = lines.len();
        let input = input_lines(marks, on_cursor, tw);
        let n = input.len();
        lines.extend(input);
        rows.push(ChoiceRow {
            question: id,
            index,
            first_line,
            lines: n,
            head,
            text: text_rows,
        });
    }
    (lines, rows)
}

/// `s` split after `w` cells.
fn split_at_width(s: &str, w: usize) -> (String, String) {
    let head = crate::text::take_width(s, w);
    let tail = s[head.len()..].to_string();
    (head, tail)
}

pub fn card_content(
    d: &CardDetail,
    screen_w: u16,
    today: NaiveDate,
    ui: QuestionUi,
    tables: markdown::TableStyle,
) -> CardContent {
    let tw = card_text_width(screen_w);
    let title = screen_title_lines(&format!("#{} {}", d.card.seq, d.card.title), screen_w);
    let (questions, choices) = question_lines(&d.questions, ui, tw);

    let margin = |l: Line<'static>, m: &str| -> Line<'static> {
        let mut spans = vec![Span::raw(m.to_string())];
        spans.extend(l.spans);
        Line::from(spans)
    };
    let mut body: Vec<Line<'static>> = Vec::new();
    if !d.card.body.trim().is_empty() {
        body.extend(
            markdown::render_with(d.card.body.trim_end(), tw, tables)
                .into_iter()
                .map(|l| margin(l, &indent())),
        );
    }
    if let Some(result) = d.card.result.as_deref().filter(|r| !r.trim().is_empty()) {
        if !body.is_empty() {
            body.push(Line::default());
        }
        let label = "結果：";
        let lw = width(label);
        for (i, l) in wrap(result.trim(), tw.saturating_sub(lw).max(1))
            .into_iter()
            .enumerate()
        {
            let head = if i == 0 {
                Span::styled(
                    format!("{}{label}", indent()),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(format!("{}{}", indent(), pad("", lw)))
            };
            body.push(Line::from(vec![head, Span::raw(l)]));
        }
        // What the current result follows from, dim, under its text (board-v2 18.6).
        if !d.result_because.is_empty() {
            let text = format!("依據 {}", causes_text(&d.result_because));
            for l in wrap(&text, tw.saturating_sub(lw).max(1)) {
                body.push(Line::from(vec![
                    Span::raw(format!("{}{}", indent(), pad("", lw))),
                    Span::styled(l, DIM),
                ]));
            }
        }
    }
    if body.is_empty() {
        body.push(Line::from(Span::styled(
            format!("{}（沒有內容）", indent()),
            DIM,
        )));
    }

    let mut notes: Vec<Line<'static>> = Vec::new();
    for (i, n) in notes_newest_first(&d.notes).into_iter().enumerate() {
        if i > 0 {
            notes.push(Line::default());
        }
        let time = Span::styled(
            format!("{}{} ", indent(), fmt_time(n.created_at, today)),
            DIM,
        );
        let head = if n.kind == NoteKind::Comment {
            // A comment from the human: the 留言 tag, no author; 未回覆 until the agent
            // replies (docs/board-v2.md section 13.16).
            let mut head = vec![
                time,
                Span::styled(
                    format!(" {} ", note_kind_label(n.kind)),
                    COMMENT_TAG_STYLE.add_modifier(Modifier::BOLD),
                ),
            ];
            if d.unreplied_comments.contains(&n.id) {
                head.push(Span::styled(
                    format!("  {UNREPLIED}"),
                    Style::new().fg(COMMENT_COLOR).add_modifier(Modifier::BOLD),
                ));
            }
            head
        } else {
            let kind_style = match n.kind {
                NoteKind::Blocker => Style::new().fg(Color::Red),
                NoteKind::Decision => Style::new().fg(Color::Cyan),
                NoteKind::Result => Style::new().fg(Color::Green),
                _ => Style::new(),
            }
            .add_modifier(Modifier::BOLD);
            let mut head = vec![
                time,
                Span::styled(note_kind_label(n.kind).to_string(), kind_style),
            ];
            // What the note follows from, dim, after the kind (docs/board-v2.md 18.6).
            if !n.because.is_empty() {
                head.push(Span::styled(
                    format!("  依據 {}", causes_text(&n.because)),
                    DIM,
                ));
            }
            if n.author != "agent" {
                head.push(Span::styled(format!("  {}", one_line(&n.author)), DIM));
            }
            head
        };
        notes.push(Line::from(head));
        notes.extend(
            markdown::render_with(n.body.trim_end(), tw.saturating_sub(2).max(1), tables)
                .into_iter()
                .map(|l| margin(l, &indent().repeat(2))),
        );
    }
    if notes.is_empty() {
        notes.push(Line::from(Span::styled(
            format!("{}（沒有紀錄）", indent()),
            DIM,
        )));
    }

    let links = link_rows(d)
        .into_iter()
        .map(|r| (r.dir, r.target, r.label))
        .collect();
    CardContent {
        title,
        questions,
        choices,
        body,
        links,
        notes,
    }
}

/// The order of the card screen's 紀錄 section (docs/board-v2.md section 19.3): newest
/// first by creation time, ties by id descending. Only the TUI reorders; the read API and
/// `kb card show` keep the oldest-first order agents read.
pub fn notes_newest_first(notes: &[Note]) -> Vec<&Note> {
    let mut sorted: Vec<&Note> = notes.iter().collect();
    sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    sorted
}

/// What a row of the 連結 section stands for; it keeps the selection across reloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRowKey {
    /// A link, by id.
    Link(i64),
    /// A cause of this card's decisions (依據), by its text.
    Cause(String),
    /// A decision that follows from this card (引出), by its text.
    LedTo(String),
}

/// One row of the card screen's 連結 section (docs/board-v2.md sections 6.1 and 18.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    pub key: LinkRowKey,
    /// Direction text, e.g. 延伸自, 依據, 引出.
    pub dir: String,
    pub target: String,
    pub label: Option<String>,
    /// The card `Enter` opens.
    pub card: Option<i32>,
    /// The external target `Enter` shows (`ref` links).
    pub external: Option<String>,
}

/// The rows of the 連結 section: the links in creation order, then the causes of this
/// card's decisions (依據), then the decisions that follow from this card (引出). Rows that
/// point at this card itself, `user` causes and repeated rows are left out.
pub fn link_rows(d: &CardDetail) -> Vec<LinkRow> {
    let mut rows: Vec<LinkRow> = d
        .links
        .iter()
        .map(|l| LinkRow {
            key: LinkRowKey::Link(l.id),
            dir: link_label(l.kind, l.direction).to_string(),
            target: match (&l.card, &l.target) {
                (Some(c), _) => format!("#{} {}", c.seq, one_line(&c.title)),
                (None, Some(t)) => one_line(t),
                (None, None) => "?".to_string(),
            },
            label: l.label.as_deref().map(one_line),
            card: l.card.as_ref().map(|c| c.seq),
            external: if l.card.is_none() {
                l.target.clone()
            } else {
                None
            },
        })
        .collect();
    let this = d.card.seq;
    let causes = d
        .notes
        .iter()
        .flat_map(|n| n.because.iter())
        .chain(d.result_because.iter());
    let mut extra: Vec<LinkRow> = Vec::new();
    for c in causes {
        let Some(seq) = c.card_seq.filter(|s| *s != this) else {
            continue;
        };
        let title = one_line(c.card_title.as_deref().unwrap_or(""));
        let target = match c.kind {
            CauseKind::Question => format!("#{seq} 的提問 {}  {title}", c.id.unwrap_or(0)),
            CauseKind::Comment => format!("#{seq} 的留言 {}  {title}", c.id.unwrap_or(0)),
            _ => format!("#{seq} {title}"),
        };
        let key = LinkRowKey::Cause(target.clone());
        if extra.iter().any(|r| r.key == key) {
            continue;
        }
        extra.push(LinkRow {
            key,
            dir: "依據".to_string(),
            target,
            label: None,
            card: Some(seq),
            external: None,
        });
    }
    for c in &d.led_to {
        if c.card_seq == this {
            continue;
        }
        let decision = match c.decision {
            DecisionKind::Drop => "捨棄",
            DecisionKind::Done => "完成",
            DecisionKind::Note => c.note_kind.map_or("紀錄", note_kind_label),
        };
        let target = format!("#{} 的{decision}  {}", c.card_seq, one_line(&c.card_title));
        let key = LinkRowKey::LedTo(target.clone());
        if extra.iter().any(|r| r.key == key) {
            continue;
        }
        extra.push(LinkRow {
            key,
            dir: "引出".to_string(),
            target,
            label: None,
            card: Some(c.card_seq),
            external: None,
        });
    }
    rows.extend(extra);
    rows
}

/// The mark of a comment the agent has not replied to yet.
pub const UNREPLIED: &str = "未回覆";

/// The comment row's label: `留言 #12：`.
pub fn comment_label(seq: i32) -> String {
    format!("留言 #{seq}：")
}

/// The visible part of a one-line text `room` cells wide with the text cursor (one cell,
/// `▏`) at character `cursor` (docs/board-v2.md section 13.15): `(cut_left, before,
/// after)`. The text scrolls sideways so that the cursor is always visible; when the start
/// is scrolled out, `cut_left` is true and one cell is kept for `…`; when the end does not
/// fit, `after` ends with `…`.
pub fn comment_view(text: &str, cursor: usize, room: usize) -> (bool, String, String) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();
    if width(text) < room {
        return (false, before, after);
    }
    let (cut_left, before) = if width(&before) < room {
        (false, before)
    } else {
        // The tail of the text before the cursor that fits with `…` and the cursor.
        let budget = room.saturating_sub(2);
        let mut tail: Vec<char> = Vec::new();
        let mut used = 0;
        for c in chars[..cursor].iter().rev() {
            let cw = crate::text::char_width(*c);
            if used + cw > budget {
                break;
            }
            used += cw;
            tail.push(*c);
        }
        tail.reverse();
        (true, tail.into_iter().collect())
    };
    let used = usize::from(cut_left) + width(&before) + 1;
    let after = truncate(&after, room.saturating_sub(used));
    (cut_left, before, after)
}

/// Rows of the message at the bottom of the card screen.
pub fn message_lines(message: Option<&str>, screen_w: u16) -> Vec<String> {
    match message {
        None => Vec::new(),
        Some(m) => {
            let mut lines = wrap(m, card_text_width(screen_w));
            lines.truncate(3);
            lines
        }
    }
}

/// Rows above the sections: top padding, the title rows, the title gap, the info row,
/// the status rows (the blocked row and the model judge's rows, docs/tui.md section 12.4).
pub fn card_head_rows(size: (u16, u16), title_rows: usize, status_rows: usize) -> usize {
    let g = gaps(size);
    g.top as usize + title_rows.max(1) + g.title as usize + 1 + status_rows
}

/// Splits the free rows between the sections on screen (each also has its title rows;
/// the key hints and the message take the bottom): each gets one row at a time, in turn,
/// until it has all its lines or the rows run out.
pub fn section_heights(
    size: (u16, u16),
    head_rows: usize,
    natural: &[usize],
    message_rows: usize,
) -> Vec<usize> {
    let fixed = head_rows + natural.len() * section_title_rows(size) + 1 + message_rows;
    let mut free = (size.1 as usize).saturating_sub(fixed);
    let want: Vec<usize> = natural.iter().map(|n| (*n).max(1)).collect();
    let mut got = vec![0usize; natural.len()];
    while free > 0 {
        let mut gave = false;
        for i in 0..got.len() {
            if free > 0 && got[i] < want[i] {
                got[i] += 1;
                free -= 1;
                gave = true;
            }
        }
        if !gave {
            break;
        }
    }
    got
}

// ----- epic screen ----------------------------------------------------------------------

/// The epic screen's title: `epic：<名稱>`, or `未歸類`.
pub fn epic_screen_title(snap: &kb_core::read::BoardSnapshot, key: crate::app::EpicKey) -> String {
    match key {
        crate::app::EpicKey::Epic(id) => snap
            .epics
            .iter()
            .find(|e| e.epic.id == id)
            .map(|e| format!("epic：{}", one_line(&e.epic.name)))
            .unwrap_or_else(|| "epic".to_string()),
        crate::app::EpicKey::NoEpic => "未歸類".to_string(),
    }
}

/// The first visible index after making `sel` visible in a window of `count` items out
/// of `total`, starting from the previous first index `offset`.
pub fn scroll_to_show(offset: usize, sel: usize, count: usize, total: usize) -> usize {
    let count = count.max(1);
    let mut off = offset;
    if sel < off {
        off = sel;
    } else if sel >= off + count {
        off = sel + 1 - count;
    }
    off.min(total.saturating_sub(count))
}

/// Screen rows of the open epic screen's scrolling list (the 卡片 rows, then the 已捨棄
/// block). Fixed rows: the top padding, the title, the title gap, the counts row, the 卡片
/// title (with its blank rows), the message and the key hints.
pub fn epic_layout(size: (u16, u16), name_rows: usize, message_rows: usize) -> usize {
    let g = gaps(size);
    let head = g.top as usize + name_rows.max(1) + g.title as usize + 1;
    let fixed = head + section_title_rows(size) + message_rows + 1;
    (size.1 as usize).saturating_sub(fixed).max(1)
}

/// Line of row `i` in the epic screen's scrolling list (docs/board-v2.md section 5.8):
/// the `n_cards` card rows (or the one 「（沒有卡片）」 line), then the 已捨棄 title with
/// its blank rows (section 6.0), then the dropped rows.
pub fn epic_row_line(i: usize, n_cards: usize, size: (u16, u16)) -> usize {
    if i < n_cards {
        i
    } else {
        n_cards.max(1) + section_title_rows(size) + (i - n_cards)
    }
}

/// Lines of the epic screen's scrolling list; the 已捨棄 block only when there is a
/// dropped card.
pub fn epic_total_lines(n_cards: usize, n_dropped: usize, size: (u16, u16)) -> usize {
    if n_dropped > 0 {
        epic_row_line(n_cards + n_dropped, n_cards, size)
    } else {
        n_cards.max(1)
    }
}

/// A link row: direction padded to 6 cells, target, optional label.
pub fn link_row(dir: &str, target: &str, label: Option<&str>, w: usize) -> (String, String) {
    let head = format!("{}  ", fit(dir, 6));
    let rest_w = w.saturating_sub(width(&head));
    let target = match label {
        Some(l) if !l.is_empty() => format!("{target}  （{l}）"),
        _ => target.to_string(),
    };
    (head, truncate(&target, rest_w))
}
