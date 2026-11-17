//! Drawing: `draw(frame, &app)` renders the state; it never changes it.
//!
//! Only the 16 ANSI colors are used (docs/board-v2.md section 5.5), so the screen follows
//! the terminal theme. Tag colors: 提問 black on yellow, 留言 white on blue, 阻塞 white on
//! red, 討論 black on cyan, 衍生 magenta; the model judge's 等回覆 black on light gray and
//! 建議未記錄 underlined (no hue, docs/tui.md section 12.2). Green only for "just
//! changed"; dark gray only for finished cards and cards outside the selected epic; blue
//! only for comments.

use kb_core::model::{BoardStatus, LaneKind, Origin};
use kb_core::read::{BoardSummary, CardSummary, EpicCounts, LaneWithCards, WorkspaceSummary};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Widget};

use crate::app::{
    App, BoardData, CardView, CommentInput, EpicKey, EpicView, Overlay, Picker, SortMode,
    epic_card_groups, epic_cards,
};
use crate::layout::{self, CardJudge, QuestionUi, Relation, Section};
use crate::text::{
    ELLIPSIS, activity_text, fit, fmt_time, lane_kind_label, one_line, origin_label,
    priority_label, take_width, truncate, width,
};

const ACCENT: Color = Color::Cyan;
const DIM: Style = Style::new().fg(Color::DarkGray);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
/// The mark of the selected epic's cards (docs/board-v2.md section 5.7). Bright blue is
/// used for nothing else.
pub const EPIC_MARK: &str = "◆";
pub const MARK_STYLE: Style = Style::new()
    .fg(Color::LightBlue)
    .add_modifier(Modifier::BOLD);

/// Whether `card` is outside the selected side-panel row (shown dim), and whether it
/// belongs to it (marked with `◆`).
fn epic_membership(app: &App, card: &CardSummary) -> (bool, bool) {
    match app.sel_epic {
        None => (false, false),
        Some(k) => {
            let member = k.contains(card);
            (!member, member)
        }
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    render(app, area, buf);
}

/// Renders the whole screen into `buf`.
pub fn render(app: &App, area: Rect, buf: &mut Buffer) {
    if layout::too_small((area.width, area.height)) {
        let text = "視窗太小";
        let x = area.x + area.width.saturating_sub(width(text) as u16) / 2;
        let y = area.y + area.height / 2;
        buf.set_stringn(x, y, text, area.width as usize, Style::new());
        return;
    }
    match (&app.card, &app.epic) {
        (Some(view), _) => draw_card(app, view, area, buf),
        (None, Some(view)) => draw_epic(app, view, area, buf),
        (None, None) => draw_board(app, area, buf),
    }
    match &app.overlay {
        None => {}
        Some(Overlay::Help) => draw_help(area, buf),
        Some(Overlay::Workspaces(p)) => draw_workspaces(app, p, area, buf),
        Some(Overlay::Boards {
            workspace_name,
            picker,
            ..
        }) => draw_boards(app, workspace_name.as_deref(), picker, area, buf),
    }
}

// ----- helpers ------------------------------------------------------------------------

/// Spans cut to `w` cells (the last visible one ends with `…`) and padded to exactly `w`.
fn fit_spans(spans: Vec<Span<'static>>, w: usize, pad_style: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    let total: usize = spans.iter().map(|s| width(&s.content)).sum();
    if total <= w {
        out.extend(spans);
        used = total;
    } else if w > 0 {
        // Keep w - 1 cells of text, then `…`.
        let budget = w - 1;
        for s in spans {
            let sw = width(&s.content);
            if used + sw <= budget {
                used += sw;
                out.push(s);
                continue;
            }
            let part = take_width(&s.content, budget - used);
            used += width(&part) + 1;
            out.push(Span::styled(format!("{part}{ELLIPSIS}"), s.style));
            break;
        }
    }
    if used < w {
        out.push(Span::styled(" ".repeat(w - used), pad_style));
    }
    out
}

fn put(buf: &mut Buffer, x: u16, y: u16, w: u16, spans: Vec<Span<'static>>) {
    let line = Line::from(spans);
    buf.set_line(x, y, &line, w);
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| width(&s.content)).sum()
}

fn status_spans(app: &App) -> Vec<Span<'static>> {
    if app.connected() {
        vec![Span::styled("● 已連線", Style::new().fg(Color::Green))]
    } else {
        vec![Span::styled("○ 重連中", Style::new().fg(Color::Yellow))]
    }
}

fn key_bar(pairs: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, d)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        if k.is_empty() {
            // A plain description without a key (`打字輸入`).
            spans.push(Span::raw(d.to_string()));
            continue;
        }
        spans.push(Span::styled(k.to_string(), BOLD.fg(ACCENT)));
        if !d.is_empty() {
            spans.push(Span::raw(format!(" {d}")));
        }
    }
    spans
}

/// Key hints while the comment row is open (docs/board-v2.md section 13.15).
const COMMENT_HINTS: &[(&str, &str)] = &[("", "打字輸入"), ("Enter", "送出"), ("Esc", "取消")];

/// The comment row, `w` cells from its first column: one blank cell, the bold label
/// `留言 #12：`, then the text scrolled sideways around the text cursor `▏`.
fn comment_spans(c: &CommentInput, w: usize) -> Vec<Span<'static>> {
    let label = layout::comment_label(c.seq);
    let room = w.saturating_sub(1 + width(&label));
    let (cut_left, before, after) = layout::comment_view(&c.text, c.cursor, room);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(label, BOLD.fg(layout::COMMENT_COLOR)),
    ];
    if cut_left {
        spans.push(Span::styled(ELLIPSIS, DIM));
    }
    spans.push(Span::raw(before));
    spans.push(Span::styled(
        layout::INPUT_CURSOR,
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(after));
    spans
}

fn flash_spans(text: &str) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!(" {text}"),
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )]
}

/// The tags of a card in display order (docs/board-v2.md section 5.5, docs/tui.md section
/// 12.2): text, style, and the border color the tag gives (filled tags only; the model
/// judge's tags give none).
pub fn card_tags(
    card: &CardSummary,
    judge: CardJudge,
) -> Vec<(&'static str, Style, Option<Color>)> {
    let mut tags = Vec::new();
    if card.open_question_count > 0 {
        tags.push((
            " 提問 ",
            Style::new().fg(Color::Black).bg(Color::Yellow),
            Some(Color::Yellow),
        ));
    }
    // Unreplied comments: filled, but the border keeps its color (section 13.16).
    if card.unreplied_comment_count > 0 {
        tags.push((" 留言 ", layout::COMMENT_TAG_STYLE, None));
    }
    if card.card.blocked_reason.is_some() {
        tags.push((
            " 阻塞 ",
            Style::new().fg(Color::White).bg(Color::Red),
            Some(Color::Red),
        ));
    }
    if judge.waiting.is_some() {
        tags.push((" 等回覆 ", layout::WAITING_STYLE, None));
    }
    if judge.advice.is_some() {
        tags.push(("建議未記錄", layout::ADVICE_STYLE, None));
    }
    match card.card.origin {
        Origin::Discussing => tags.push((
            " 討論 ",
            Style::new().fg(Color::Black).bg(Color::Cyan),
            Some(Color::Cyan),
        )),
        Origin::Derived => tags.push(("衍生", Style::new().fg(Color::Magenta), None)),
        Origin::Agreed => {}
    }
    tags
}

/// Replaces colors with dark gray (finished cards, cards outside the selected epic).
fn dimmed(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|s| {
            let style = Style {
                fg: Some(Color::DarkGray),
                bg: None,
                ..s.style
            };
            Span::styled(s.content, style)
        })
        .collect()
}

// ----- board screen -------------------------------------------------------------------

fn draw_board(app: &App, area: Rect, buf: &mut Buffer) {
    let w = area.width as usize;
    let Some(data) = &app.data else {
        let mut right = status_spans(app);
        right.push(Span::raw(" "));
        let rw = spans_width(&right);
        let mut header = fit_spans(vec![Span::raw(" kb tui")], w - rw, Style::new());
        header.extend(right);
        put(buf, area.x, area.y, area.width, header);
        let text = if app.loaded {
            "目前沒有進行中的看板"
        } else {
            "載入中…"
        };
        let x = area.x + area.width.saturating_sub(width(text) as u16) / 2;
        buf.set_stringn(x, area.y + area.height / 2, text, w, DIM);
        let keys = key_bar(&[
            ("w", "workspace"),
            ("b", "看板"),
            ("?", "說明"),
            ("q", "離開"),
        ]);
        put(
            buf,
            area.x,
            area.bottom() - 1,
            area.width,
            fit_spans(keys, w, Style::new()),
        );
        return;
    };
    draw_header(app, data, area, buf);

    let side = app.side_shown();
    let areas = layout::board_areas((area.width, area.height), side);
    let shift = |r: Rect| Rect::new(r.x + area.x, r.y + area.y, r.width, r.height);
    if let Some(r) = areas.side {
        draw_side(app, data, shift(r), buf);
    }
    draw_lanes(app, data, shift(areas.lanes), buf);
    if let Some(r) = areas.strip {
        let r = shift(r);
        for (i, a) in data.activity.iter().take(r.height as usize).enumerate() {
            let spans = vec![
                Span::styled(format!(" {} ", fmt_time(a.at, app.today)), DIM),
                Span::raw(activity_text(a)),
            ];
            put(
                buf,
                r.x,
                r.y + i as u16,
                r.width,
                fit_spans(spans, w, Style::new()),
            );
        }
    }

    // The comment row takes the last activity row (docs/board-v2.md section 13.15).
    if let Some(c) = &app.comment {
        put(
            buf,
            area.x,
            area.bottom().saturating_sub(2),
            area.width,
            fit_spans(comment_spans(c, w), w, Style::new()),
        );
    }
    let bottom = match (&app.flash, &app.comment) {
        (Some(f), _) => flash_spans(f),
        (None, Some(_)) => key_bar(COMMENT_HINTS),
        (None, None) => key_bar(&board_hints(app, w)),
    };
    put(
        buf,
        area.x,
        area.bottom() - 1,
        area.width,
        fit_spans(bottom, w, Style::new()),
    );
}

/// A key and its description in a key bar.
type KeyPair = (&'static str, &'static str);

/// The board screen's key hints (docs/board-v2.md sections 5.7, 13.18 and 16.4). With an
/// epic selected, `Esc 取消 epic` joins; with the lanes focused, `o 排序` follows. When the
/// bar is wider than `w`, it shortens step by step: `w b ?` becomes `?`, `泳道/側欄`
/// becomes `泳道`, `e epic` goes, `o 排序` goes.
pub fn board_hints(app: &App, w: usize) -> Vec<KeyPair> {
    let selected = app.sel_epic.is_some();
    let mut pairs = if app.side_focused() {
        vec![
            ("↑↓", "選 epic"),
            ("Enter", "開啟"),
            ("→", "泳道"),
            ("e", "epic"),
        ]
    } else {
        vec![
            ("←→", "泳道/側欄"),
            ("↑↓", "卡片"),
            ("Enter", "開啟"),
            ("a", "提問"),
            ("i", "留言"),
            ("e", "epic"),
        ]
    };
    if selected {
        pairs.push(("Esc", "取消 epic"));
    }
    if !app.side_focused() {
        pairs.push(("o", "排序"));
    }
    if w >= layout::SIDE_MIN_SCREEN as usize {
        pairs.push(("s", "側欄"));
    }
    pairs.extend([("w b ?", ""), ("q", "離開")]);
    let fits = |p: &[(&str, &str)]| spans_width(&key_bar(p)) <= w;
    let steps: [fn(&mut Vec<KeyPair>); 4] = [
        |p| {
            for x in p.iter_mut().filter(|x| x.0 == "w b ?") {
                x.0 = "?";
            }
        },
        |p| {
            for x in p.iter_mut().filter(|x| **x == ("←→", "泳道/側欄")) {
                x.1 = "泳道";
            }
        },
        |p| p.retain(|x| *x != ("e", "epic")),
        |p| p.retain(|x| *x != ("o", "排序")),
    ];
    for step in steps {
        if fits(&pairs) {
            break;
        }
        step(&mut pairs);
    }
    pairs
}

/// Header: `workspace › title › session key` on the left; `提問 N`, `討論 N`, `留言 N`, `等回覆`,
/// `[已封存]` and the connection status on the right (docs/board-v2.md sections 5.1 and
/// 13.16, docs/tui.md section 12.3).
fn draw_header(app: &App, data: &BoardData, area: Rect, buf: &mut Buffer) {
    let w = area.width as usize;
    let snap = &data.snapshot;
    let board = &snap.board;
    let mut right = Vec::new();
    // A display order other than the default, first, so that it is never mistaken for the
    // agent's queue order (docs/board-v2.md section 16.4).
    if app.sort != SortMode::Default {
        right.push(Span::styled(
            format!("排序：{}", app.sort.label()),
            BOLD.add_modifier(Modifier::REVERSED),
        ));
        right.push(Span::raw("  "));
    }
    if snap.open_question_count > 0 {
        right.push(Span::styled(
            format!("提問 {}", snap.open_question_count),
            BOLD.fg(Color::Yellow),
        ));
        right.push(Span::raw("  "));
    }
    if snap.discussing_count > 0 {
        right.push(Span::styled(
            format!("討論 {}", snap.discussing_count),
            BOLD.fg(Color::Cyan),
        ));
        right.push(Span::raw("  "));
    }
    if snap.unreplied_comment_count > 0 {
        right.push(Span::styled(
            format!("留言 {}", snap.unreplied_comment_count),
            BOLD.fg(layout::COMMENT_COLOR),
        ));
        right.push(Span::raw("  "));
    }
    // The model judge: the agent waits for a reply in the chat, on no card (docs/tui.md
    // section 12.3).
    if data.board_waiting().is_some() {
        right.push(Span::styled(
            "等回覆",
            layout::WAITING_STYLE.add_modifier(Modifier::BOLD),
        ));
        right.push(Span::raw("  "));
    }
    if board.status == BoardStatus::Archived {
        right.push(Span::styled("[已封存]", Style::new().fg(Color::Yellow)));
        right.push(Span::raw("  "));
    }
    right.extend(status_spans(app));
    right.push(Span::raw(" "));
    let rw = spans_width(&right);
    let room = w.saturating_sub(rw);
    let key = one_line(&board.session_key);
    let untitled = board.title.trim().is_empty();
    let title = if untitled {
        key.clone()
    } else {
        one_line(&board.title)
    };
    let base = vec![
        Span::raw(format!(" {} › ", one_line(&snap.workspace.name))),
        Span::styled(title, BOLD),
    ];
    let mut left = base.clone();
    left.push(Span::raw("  "));
    // The session key, dimmed, after the title: in full, else its first 8 characters and
    // `…`, else not at all; the title is cut only once the key is gone. An untitled
    // board already shows the key as its title.
    if !untitled && !key.is_empty() {
        for k in header_key_forms(&key) {
            let mut with_key = base.clone();
            with_key.push(Span::raw(" › "));
            with_key.push(Span::styled(k, DIM));
            with_key.push(Span::raw("  "));
            if spans_width(&with_key) <= room {
                left = with_key;
                break;
            }
        }
    }
    let mut header = fit_spans(left, room, Style::new());
    header.extend(right);
    put(buf, area.x, area.y, area.width, header);
}

/// The forms of the session key the header tries, longest first: the key as it is, then
/// its first 8 characters and `…` when the key is longer than 9 characters.
fn header_key_forms(key: &str) -> Vec<String> {
    let mut forms = vec![key.to_string()];
    if key.chars().count() > 9 {
        forms.push(format!("{}{ELLIPSIS}", key.chars().take(8).collect::<String>()));
    }
    forms
}

// ----- side panel ---------------------------------------------------------------------

/// One EPIC row, `w` cells: marker, name (cut with `…`), `done/total` and `+N`, which are
/// never cut (docs/board-v2.md section 5.2).
pub fn epic_row(name: &str, c: &EpicCounts, selected: bool, w: usize) -> Vec<Span<'static>> {
    let finished = c.is_finished();
    let ratio = format!("{}/{}", c.done, c.total);
    let plus = if c.derived > 0 {
        format!(" +{}", c.derived)
    } else {
        String::new()
    };
    // The selected row shows `◆`, the same mark as its cards (docs/board-v2.md 5.7).
    let marker = if selected {
        EPIC_MARK
    } else if finished {
        "✓"
    } else {
        " "
    };
    let name_w = w.saturating_sub(3 + 1 + width(&ratio) + width(&plus) + 1);
    let base = if finished { DIM } else { Style::new() };
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            marker,
            if selected {
                MARK_STYLE
            } else if finished {
                DIM
            } else {
                base
            },
        ),
        Span::raw(" "),
        Span::styled(
            fit(&one_line(name), name_w),
            if selected {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            },
        ),
        Span::raw(" "),
        Span::styled(ratio, base),
    ];
    if !plus.is_empty() {
        spans.push(Span::styled(
            plus,
            if finished {
                DIM
            } else {
                Style::new().fg(Color::Magenta)
            },
        ));
    }
    spans.push(Span::raw(" "));
    spans
}

/// The side panel: the epics (and 「未歸類」).
fn draw_side(app: &App, data: &BoardData, rect: Rect, buf: &mut Buffer) {
    let snap = &data.snapshot;
    if rect.height < 4 {
        return;
    }
    let iw = rect.width.saturating_sub(2) as usize;
    let focused = app.side_focused();
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut sel_row = None;
    if snap.epics.is_empty() {
        rows.push(vec![Span::styled(" （沒有 epic）", DIM)]);
    }
    let mut push =
        |rows: &mut Vec<Vec<Span<'static>>>, key: EpicKey, name: &str, c: &EpicCounts| {
            let selected = app.sel_epic == Some(key);
            let mut row = epic_row(name, c, selected, iw);
            if selected {
                sel_row = Some(rows.len());
                if focused {
                    // The focused panel shows its selection in reverse video.
                    let row_w = spans_width(&row);
                    row.push(Span::raw(" ".repeat(iw.saturating_sub(row_w))));
                    row = row
                        .into_iter()
                        .map(|s| Span::styled(s.content, s.style.add_modifier(Modifier::REVERSED)))
                        .collect();
                }
            }
            rows.push(row);
        };
    for e in &snap.epics {
        push(&mut rows, EpicKey::Epic(e.epic.id), &e.epic.name, &e.counts);
    }
    if let Some(c) = &snap.no_epic {
        push(&mut rows, EpicKey::NoEpic, "未歸類", c);
    }
    // The epic rows take what the panel has between its borders.
    let max_rows = (rect.height as usize).saturating_sub(2).max(1);
    let shown_rows = rows.len().min(max_rows);
    let first = match sel_row {
        Some(s) if s >= shown_rows => s + 1 - shown_rows,
        _ => 0,
    };

    let (title_style, border_style) = if focused {
        (BOLD.fg(ACCENT), Style::new().fg(ACCENT))
    } else {
        (BOLD, Style::new())
    };
    let block = Block::bordered()
        .border_style(border_style)
        .title(Span::styled(" EPIC ", title_style));
    block.render(rect, buf);
    let x = rect.x + 1;
    for (y, row) in (rect.y + 1..).zip(rows.iter().skip(first).take(shown_rows)) {
        put(
            buf,
            x,
            y,
            iw as u16,
            fit_spans(row.clone(), iw, Style::new()),
        );
    }
}

// ----- lanes --------------------------------------------------------------------------

fn draw_lanes(app: &App, data: &BoardData, area: Rect, buf: &mut Buffer) {
    let lanes = &data.snapshot.lanes;
    if area.height < 3 {
        return;
    }
    if lanes.is_empty() {
        buf.set_stringn(
            area.x + 1,
            area.y + 1,
            "（沒有泳道）",
            area.width as usize,
            DIM,
        );
        return;
    }
    let kinds: Vec<LaneKind> = lanes.iter().map(|l| l.lane.kind).collect();
    let sel = app.sel_lane.min(lanes.len() - 1);
    let strip = layout::lane_strip(area, &kinds, sel, app.lane_offset);
    for slot in &strip.slots {
        let rect = Rect::new(slot.x, area.y, slot.width, area.height);
        let lane = &lanes[slot.index];
        if slot.narrow {
            draw_narrow_lane(app, lane, rect, buf);
        } else {
            draw_lane(app, lane, slot.index == sel, rect, buf);
        }
    }
    if strip.scrolling {
        let mid = area.y + area.height / 2;
        let arrow = BOLD.fg(ACCENT);
        // On the outer lane borders.
        if strip.more_left
            && let Some(s) = strip.slots.first()
        {
            buf.set_stringn(s.x, mid, "‹", 1, arrow);
        }
        if strip.more_right
            && let Some(s) = strip.slots.last()
        {
            buf.set_stringn(s.x + s.width - 1, mid, "›", 1, arrow);
        }
    }
}

/// Lane frame with title and scroll hints; returns the inner area.
fn lane_frame(
    title: String,
    title_style: Style,
    border_style: Style,
    rect: Rect,
    above: usize,
    below: usize,
    buf: &mut Buffer,
) -> Rect {
    let mut block = Block::bordered()
        .border_style(border_style)
        .title(Line::from(Span::styled(title, title_style)));
    if above > 0 {
        block = block.title_top(Line::from(Span::styled(format!("↑{above}"), DIM)).right_aligned());
    }
    if below > 0 {
        block =
            block.title_bottom(Line::from(Span::styled(format!("↓{below}"), DIM)).right_aligned());
    }
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

fn draw_lane(app: &App, lane: &LaneWithCards, selected: bool, rect: Rect, buf: &mut Buffer) {
    let l = &lane.lane;
    let count = lane.cards.len();
    let suffix = match l.wip_limit {
        Some(limit) => format!(" ({count}/{limit})"),
        None => format!(" ({count})"),
    };
    let over_limit = l.wip_limit.is_some_and(|limit| count as i64 > limit as i64);
    let name_max = (rect.width as usize).saturating_sub(4 + width(&suffix));
    let title = format!(" {}{suffix} ", truncate(&one_line(&l.name), name_max));
    let mut title_style = BOLD;
    let mut border_style = Style::new();
    if selected {
        title_style = title_style.fg(ACCENT);
        border_style = border_style.fg(ACCENT);
    }
    if over_limit {
        title_style = title_style.fg(Color::Red);
    }
    let room = rect.height.saturating_sub(2);
    let findings = app.data.as_ref().map_or(&[][..], |d| d.findings.as_slice());
    let heights = layout::card_heights(&lane.cards, rect.width, false, findings);
    let sel_idx = if selected {
        app.card_index_in(lane)
    } else {
        None
    };
    let stored = app.card_offsets.get(&l.id).copied().unwrap_or(0);
    let (first, end) = layout::card_window(&heights, room, sel_idx, stored);
    let inner = lane_frame(
        title,
        title_style,
        border_style,
        rect,
        first,
        count.saturating_sub(end),
        buf,
    );
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if lane.cards.is_empty() {
        put(
            buf,
            inner.x,
            inner.y,
            inner.width,
            fit_spans(
                vec![Span::styled(" （沒有卡片）", DIM)],
                inner.width as usize,
                Style::new(),
            ),
        );
        return;
    }
    let closed = l.kind.is_closed();
    let mut y = inner.y;
    for (idx, (card, h)) in lane
        .cards
        .iter()
        .zip(&heights)
        .enumerate()
        .take(end)
        .skip(first)
    {
        if y >= inner.bottom() {
            break;
        }
        let h = (*h).min(inner.bottom() - y);
        let card_rect = Rect::new(inner.x, y, inner.width, h);
        draw_card_box(app, card, card_rect, sel_idx == Some(idx), closed, buf);
        y += h;
    }
}

/// A card as a box (docs/board-v2.md section 5.4): wrapped title (at most three lines),
/// the epic name and tags, and the relation row when the card has relations.
pub fn draw_card_box(
    app: &App,
    card: &CardSummary,
    rect: Rect,
    selected: bool,
    closed: bool,
    buf: &mut Buffer,
) {
    let c = &card.card;
    let (other_epic, member) = epic_membership(app, card);
    let dim = closed || other_epic;
    let highlighted = app.highlights.contains_key(&c.id);
    let judge = app.card_judge(c.id);
    let tags = card_tags(card, judge);
    let mut border = Style::new();
    if let Some(color) = tags.iter().find_map(|t| t.2) {
        border = border.fg(color);
    }
    if dim {
        border = DIM;
    }
    if highlighted {
        border = Style::new().fg(Color::Green);
    }
    let block = Block::bordered()
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(border);
    let inner = block.inner(rect);
    block.render(rect, buf);
    // A card of the selected epic: `◆` in the top-left corner, replacing one cell of the
    // top border (docs/board-v2.md section 5.7).
    if member && rect.width >= 3 {
        buf.set_stringn(rect.x + 1, rect.y, EPIC_MARK, 1, MARK_STYLE);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let iw = inner.width as usize;
    let text_w = iw.saturating_sub(2);
    // The tags row index and the span index of the epic name in it.
    let mut epic_span: Option<(usize, usize)> = None;
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let title_style = if selected { BOLD } else { Style::new() };
    for (i, l) in layout::card_title_lines(card, rect.width + 2)
        .into_iter()
        .enumerate()
    {
        let marker = if i == 0 && selected {
            Span::styled("▶", BOLD.fg(ACCENT))
        } else {
            Span::raw(" ")
        };
        rows.push(vec![marker, Span::styled(l, title_style)]);
    }
    if layout::has_tags_row(card, judge) {
        let tag_w: usize =
            tags.iter().map(|t| width(t.0)).sum::<usize>() + tags.len().saturating_sub(1);
        let mut spans = vec![Span::raw(" ")];
        if let Some(epic) = &card.epic_name {
            let gap = if tags.is_empty() { 0 } else { 2 };
            let room = text_w.saturating_sub(tag_w + gap);
            if room >= 2 {
                epic_span = Some((rows.len(), spans.len()));
                spans.push(Span::styled(truncate(&one_line(epic), room), DIM));
                if gap > 0 {
                    spans.push(Span::raw("  "));
                }
            }
        }
        for (i, (t, style, _)) in tags.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(*t, *style));
        }
        rows.push(spans);
    }
    for line in layout::relation_lines(card, rect.width + 2) {
        let mut spans = vec![Span::raw(" ")];
        for (i, (rel, text)) in line.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let style = match rel {
                Relation::SpawnedFrom => Style::new(),
                Relation::BlockedBy => Style::new().fg(Color::Red),
                Relation::Spawns => Style::new().fg(Color::Magenta),
            };
            spans.push(Span::styled(text, style));
        }
        rows.push(spans);
    }
    for (i, row) in rows.into_iter().take(inner.height as usize).enumerate() {
        let mut row = if dim { dimmed(row) } else { row };
        // The selected epic's cards show their epic name in the mark's color.
        if member
            && let Some((r, s)) = epic_span
            && r == i
            && let Some(span) = row.get_mut(s)
        {
            span.style = MARK_STYLE;
        }
        let mut spans = fit_spans(row, iw.saturating_sub(1), Style::new());
        spans.push(Span::raw(" "));
        put(buf, inner.x, inner.y + i as u16, inner.width, spans);
    }
}

/// A `done` / `dropped` lane that is not selected: 14 columns, title `名稱 數量`, one dim
/// row per card (docs/board-v2.md section 5.3).
fn draw_narrow_lane(app: &App, lane: &LaneWithCards, rect: Rect, buf: &mut Buffer) {
    let l = &lane.lane;
    let count = lane.cards.len();
    let suffix = format!(" {count}");
    let name_max = (rect.width as usize).saturating_sub(4 + width(&suffix));
    let title = format!(" {}{suffix} ", truncate(&one_line(&l.name), name_max));
    let room = rect.height.saturating_sub(2);
    let heights = vec![1u16; count];
    let stored = app.card_offsets.get(&l.id).copied().unwrap_or(0);
    let (first, end) = layout::card_window(&heights, room, None, stored);
    let inner = lane_frame(
        title,
        BOLD.fg(Color::DarkGray),
        DIM,
        rect,
        first,
        count.saturating_sub(end),
        buf,
    );
    if inner.width == 0 {
        return;
    }
    let iw = inner.width as usize;
    for (slot, idx) in (first..end).enumerate() {
        let c = &lane.cards[idx];
        let (_, member) = epic_membership(app, c);
        let style = if app.highlights.contains_key(&c.card.id) {
            Style::new().fg(Color::Green)
        } else if member {
            // A card of the selected epic is marked and not dim (docs/board-v2.md 5.7).
            Style::new()
        } else {
            DIM
        };
        let text = format!("#{} {}", c.card.seq, one_line(&c.card.title));
        // The mark takes the margin cell before the number, so the numbers stay aligned.
        let mut spans = vec![if member {
            Span::styled(EPIC_MARK, MARK_STYLE)
        } else {
            Span::raw(" ")
        }];
        spans.extend(fit_spans(
            vec![Span::styled(text, style)],
            iw.saturating_sub(2),
            Style::new(),
        ));
        spans.push(Span::raw(" "));
        put(buf, inner.x, inner.y + slot as u16, inner.width, spans);
    }
}

// ----- open screens: card and epic (docs/board-v2.md section 6.0) ----------------------
//
// Every margin comes from the constants in `layout` (SIDE_MARGIN, CONTENT_INDENT,
// TOP_PADDING, TITLE_GAP, SECTION_GAP_ABOVE, SECTION_GAP_BELOW and their short-screen
// values).

/// Left column of titles, rules, `▶` and section lines.
fn margin_x(area: Rect) -> u16 {
    area.x + layout::SIDE_MARGIN
}

/// The first rows of an open screen: the top padding, `title` (bold, wrapped, at most
/// three lines) from the side margin with the connection status right-aligned on its
/// first row, then the title gap. Returns the next row.
fn draw_screen_title(app: &App, title: &[String], area: Rect, buf: &mut Buffer) -> u16 {
    let g = layout::gaps((area.width, area.height));
    let iw = layout::inner_width(area.width);
    let tw = iw.saturating_sub(layout::STATUS_TEXT_WIDTH + 2);
    let top = area.y + g.top;
    let mut y = top;
    for l in title {
        put(
            buf,
            margin_x(area),
            y,
            tw as u16,
            vec![Span::styled(l.clone(), BOLD)],
        );
        y += 1;
    }
    let status = status_spans(app);
    let sw = spans_width(&status) as u16;
    put(
        buf,
        area.right().saturating_sub(layout::SIDE_MARGIN + sw),
        top,
        sw,
        status,
    );
    y.max(top + 1) + g.title
}

/// A section title (docs/board-v2.md section 6.0): the blank rows above, one line from
/// the side margin, the blank rows below. The focused section's title is in reverse
/// video. With the section-rule switch on, a dim `─` rule follows to the right margin,
/// with the scroll range (`1-5/30`, dim) at its right end; off, the range follows the
/// title. Returns the next row.
fn draw_section_title(
    app: &App,
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    title: &str,
    focused: bool,
    range: &str,
) -> u16 {
    let g = layout::gaps((area.width, area.height));
    let y = y + g.above;
    let w = layout::inner_width(area.width);
    put(
        buf,
        margin_x(area),
        y,
        w as u16,
        fit_spans(
            section_title_spans(app, w, title, focused, range),
            w,
            Style::new(),
        ),
    );
    y + 1 + g.below
}

/// The one line of a section title, `w` cells wide (see [`draw_section_title`]).
fn section_title_spans(
    app: &App,
    w: usize,
    title: &str,
    focused: bool,
    range: &str,
) -> Vec<Span<'static>> {
    let style = if focused {
        BOLD.add_modifier(Modifier::REVERSED)
    } else {
        BOLD
    };
    let mut spans = vec![Span::styled(title.to_string(), style)];
    if app.look.section_rule {
        // The rule runs to the right margin; a scroll range sits at its right end.
        let tail = if range.is_empty() {
            0
        } else {
            width(range) + 1
        };
        let used = spans_width(&spans) + 1 + tail;
        spans.push(Span::raw(" "));
        spans.push(Span::styled("─".repeat(w.saturating_sub(used)), DIM));
        if !range.is_empty() {
            spans.push(Span::styled(format!(" {range}"), DIM));
        }
    } else if !range.is_empty() {
        spans.push(Span::styled(format!("  {range}"), DIM));
    }
    spans
}

fn section_range(first: usize, shown: usize, total: usize) -> String {
    if total <= shown {
        String::new()
    } else {
        format!("{}-{}/{}", first + 1, (first + shown).min(total), total)
    }
}

/// The bottom of an open screen: the message lines, then the comment row when it is open,
/// above the key hints, which are the last row; all from the side margin, without any
/// border.
fn draw_screen_bottom(
    app: &App,
    message: &[String],
    hints: &[(&str, &str)],
    area: Rect,
    buf: &mut Buffer,
) {
    let w = layout::inner_width(area.width);
    let last = area.bottom() - 1;
    // `key_bar`, `flash_spans` and `comment_spans` start with one blank cell.
    let x = margin_x(area) - 1;
    let hints = if app.comment.is_some() {
        COMMENT_HINTS
    } else {
        hints
    };
    put(
        buf,
        x,
        last,
        w as u16 + 1,
        fit_spans(key_bar(hints), w + 1, Style::new()),
    );
    let mut above = last;
    if let Some(c) = &app.comment {
        above = last.saturating_sub(1);
        put(
            buf,
            x,
            above,
            w as u16 + 1,
            fit_spans(comment_spans(c, w + 1), w + 1, Style::new()),
        );
    }
    let top = above.saturating_sub(message.len() as u16);
    for (i, l) in message.iter().enumerate() {
        put(
            buf,
            x,
            top + i as u16,
            w as u16 + 1,
            fit_spans(flash_spans(l), w + 1, Style::new()),
        );
    }
}

/// Draws `lines` (which carry their own content indent) from row `y` at the side margin,
/// never at or below `bottom`. Returns the next row.
fn draw_lines(
    buf: &mut Buffer,
    area: Rect,
    lines: &[Line<'static>],
    mut y: u16,
    bottom: u16,
) -> u16 {
    let w = layout::inner_width(area.width) as u16;
    for line in lines {
        if y >= bottom {
            break;
        }
        buf.set_line(margin_x(area), y, line, w);
        y += 1;
    }
    y
}

// ----- epic screen --------------------------------------------------------------------

/// Tags of a card as spans, one space apart, and their width.
fn tag_spans(card: &CardSummary, judge: CardJudge) -> (Vec<Span<'static>>, usize) {
    let mut spans = Vec::new();
    for (i, (t, style, _)) in card_tags(card, judge).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(t, style));
    }
    let w = spans_width(&spans);
    (spans, w)
}

/// One row of the epic screen's card list (docs/board-v2.md section 5.8), `w` cells
/// after the content indent: `▶` (selected) or the indent, then `#編號`, lane name and
/// title in aligned columns, then the tags, which are never cut. The reverse video of
/// the selected row covers its text only.
pub fn epic_list_row(
    card: &CardSummary,
    judge: CardJudge,
    lane_name: &str,
    closed: bool,
    selected: bool,
    cols: (usize, usize, usize),
    w: usize,
) -> Vec<Span<'static>> {
    let (seq_w, lane_w, tags_w) = cols;
    let tags_part = if tags_w > 0 { 2 + tags_w } else { 0 };
    let title_w = w.saturating_sub(seq_w + 2 + lane_w + 2 + tags_part);
    let base = if selected {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    let title = truncate(&one_line(&card.card.title), title_w);
    let title_pad = title_w.saturating_sub(width(&title));
    let mut spans = vec![
        Span::raw(if selected {
            layout::marker()
        } else {
            layout::indent()
        }),
        Span::styled(fit(&format!("#{}", card.card.seq), seq_w + 2), base),
        Span::styled(fit(&one_line(lane_name), lane_w + 2), base),
        Span::styled(title, base),
        Span::raw(" ".repeat(title_pad)),
    ];
    if tags_w > 0 {
        let (tags, _) = tag_spans(card, judge);
        spans.push(Span::raw("  "));
        spans.extend(tags);
    }
    if closed { dimmed(spans) } else { spans }
}

fn draw_epic(app: &App, view: &EpicView, area: Rect, buf: &mut Buffer) {
    let Some(data) = &app.data else {
        return;
    };
    let snap = &data.snapshot;
    let counts = match view.key {
        EpicKey::Epic(id) => snap
            .epics
            .iter()
            .find(|e| e.epic.id == id)
            .map_or_else(EpicCounts::default, |e| e.counts),
        EpicKey::NoEpic => snap.no_epic.unwrap_or_default(),
    };
    let size = (area.width, area.height);
    let tw = layout::card_text_width(area.width);
    let iw = layout::inner_width(area.width);
    let message = layout::message_lines(app.flash.as_deref(), area.width);
    let last = area.bottom() - 1;
    let bottom = last.saturating_sub((message.len() + app.comment_rows()) as u16);

    let name = layout::screen_title_lines(&layout::epic_screen_title(snap, view.key), area.width);
    let mut y = draw_screen_title(app, &name, area, buf);

    // Counts row, dim.
    let mut stats = format!("{}/{} 完成", counts.done, counts.total);
    if counts.derived > 0 {
        stats.push_str(&format!(" · 衍生 +{}", counts.derived));
    }
    if counts.discussing > 0 {
        stats.push_str(&format!(" · 討論 {}", counts.discussing));
    }
    // The list follows the board's display order (docs/board-v2.md section 16.4).
    if app.sort != SortMode::Default {
        stats.push_str(&format!(" · 排序：{}", app.sort.label()));
    }
    put(
        buf,
        margin_x(area),
        y,
        iw as u16,
        fit_spans(vec![Span::styled(stats, DIM)], iw, Style::new()),
    );
    y += 1;

    let list_h = layout::epic_layout(size, name.len(), message.len() + app.comment_rows());

    // Two groups, one selection (docs/board-v2.md section 5.8): the cards not in a
    // dropped lane under 卡片 (N), N = the statistics' total; the dropped ones under
    // 已捨棄 (N), all dim, only when there is one. Both scroll together, in lines.
    let (cards, dropped) = epic_card_groups(snap, view.key);
    let rows = epic_cards(snap, view.key);
    let sel = view.index_in(&rows);
    let total_lines = layout::epic_total_lines(cards.len(), dropped.len(), size);
    let first = sel.map_or(0, |s| {
        layout::scroll_to_show(
            view.first,
            layout::epic_row_line(s, cards.len(), size),
            list_h,
            total_lines,
        )
    });
    // Rows visible in the window, for the range after the 卡片 title.
    let visible: Vec<usize> = (0..rows.len())
        .filter(|&i| {
            let line = layout::epic_row_line(i, cards.len(), size);
            line >= first && line < first + list_h
        })
        .collect();
    let range = match (visible.first(), visible.last()) {
        (Some(a), Some(b)) if visible.len() < rows.len() => {
            format!("{}-{}/{}", a + 1, b + 1, rows.len())
        }
        _ => String::new(),
    };
    if y < bottom {
        y = draw_section_title(
            app,
            buf,
            area,
            y,
            &format!("卡片 ({})", cards.len()),
            true,
            &range,
        );
    }
    let lane_of = |c: &CardSummary| snap.lanes.iter().find(|l| l.lane.id == c.card.lane_id);
    let seq_w = rows
        .iter()
        .map(|c| width(&format!("#{}", c.card.seq)))
        .max()
        .unwrap_or(0);
    let lane_w = rows
        .iter()
        .filter_map(|c| lane_of(c).map(|l| width(&one_line(&l.lane.name))))
        .max()
        .unwrap_or(0);
    let tags_w = rows
        .iter()
        .map(|c| tag_spans(c, data.judge(c.card.id)).1)
        .max()
        .unwrap_or(0);
    let row_line = |i: usize, c: &CardSummary| -> Line<'static> {
        let lane = lane_of(c);
        Line::from(epic_list_row(
            c,
            data.judge(c.card.id),
            lane.map_or("", |l| l.lane.name.as_str()),
            lane.is_some_and(|l| l.lane.kind.is_closed()),
            Some(i) == sel,
            (seq_w, lane_w, tags_w),
            tw,
        ))
    };
    let mut all: Vec<Line<'static>> = Vec::new();
    if cards.is_empty() {
        all.push(Line::from(Span::styled(
            format!("{}（沒有卡片）", layout::indent()),
            DIM,
        )));
    }
    for (i, c) in cards.iter().enumerate() {
        all.push(row_line(i, c));
    }
    if !dropped.is_empty() {
        // The 已捨棄 title scrolls with the rows; never reversed (the 卡片 section has the
        // focus). Its lines match `layout::epic_row_line`.
        let g = layout::gaps(size);
        all.extend((0..g.above).map(|_| Line::default()));
        all.push(Line::from(section_title_spans(
            app,
            iw,
            &format!("已捨棄 ({})", dropped.len()),
            false,
            "",
        )));
        all.extend((0..g.below).map(|_| Line::default()));
        for (k, c) in dropped.iter().enumerate() {
            all.push(row_line(cards.len() + k, c));
        }
    }
    let lines: Vec<Line<'static>> = all.into_iter().skip(first).take(list_h).collect();
    draw_lines(buf, area, &lines, y, bottom);
    draw_screen_bottom(
        app,
        &message,
        &[
            ("↑↓", "選卡片"),
            ("Enter", "前往"),
            ("i", "留言"),
            ("←", "上一層"),
            ("Esc", "看板"),
        ],
        area,
        buf,
    );
}

// ----- card screen --------------------------------------------------------------------

/// Key hints while the cursor is on a question's input row.
const INPUT_HINTS: &[(&str, &str)] = &[
    ("", "打字輸入"),
    ("Enter", "送出"),
    ("↑↓", "離開輸入"),
    ("Esc", "看板"),
];

/// Key hints of the card screen (docs/board-v2.md section 13.18). `on_option`: the
/// question cursor is on an option row, where `i` goes to the input row (`i 打字`);
/// everywhere else `i` comments (`i 留言`).
fn card_hints(focus: Section, on_option: bool) -> Vec<(&'static str, &'static str)> {
    let mut pairs = match focus {
        Section::Questions => vec![
            ("↑↓", "選擇"),
            ("Space", "勾選"),
            ("Enter", "送出"),
            ("Tab", "切換區塊"),
            ("←", "上一層"),
            ("Esc", "看板"),
        ],
        Section::Links => vec![
            ("↑↓", "選連結"),
            ("Enter", "前往"),
            ("Tab", "切換區塊"),
            ("←", "上一層"),
            ("Esc", "看板"),
        ],
        _ => vec![
            ("↑↓", "捲動"),
            ("PgUp/PgDn", "翻頁"),
            ("Tab", "切換區塊"),
            ("←", "上一層"),
            ("Esc", "看板"),
        ],
    };
    if on_option {
        // After `Space 勾選`.
        pairs.insert(2, ("i", "打字"));
    } else {
        let tab = pairs.iter().position(|p| p.0 == "Tab").unwrap_or(0);
        pairs.insert(tab, ("i", "留言"));
    }
    pairs
}

/// The card screen in the open layout (docs/board-v2.md sections 6 and 6.0).
fn draw_card(app: &App, view: &CardView, area: Rect, buf: &mut Buffer) {
    let iw = layout::inner_width(area.width);
    let message = layout::message_lines(app.card_message(), area.width);
    let last = area.bottom() - 1;
    let bottom = last.saturating_sub((message.len() + app.comment_rows()) as u16);
    // On a question's input row keys type (docs/board-v2.md section 7.1).
    let on_option = view.focus == Section::Questions
        && view.q_cursor.is_some_and(|(qid, idx)| {
            view.detail.as_ref().is_some_and(|d| {
                d.questions
                    .iter()
                    .any(|q| q.question.id == qid && q.is_open() && idx < layout::input_index(q))
            })
        });
    let mut card_pairs = card_hints(view.focus, on_option);
    let hints: &[(&str, &str)] = if app.input_target().is_some() {
        INPUT_HINTS
    } else {
        &card_pairs
    };

    let Some(d) = &view.detail else {
        let y = draw_screen_title(app, &[format!("#{}", view.seq)], area, buf);
        buf.set_stringn(margin_x(area), y, "載入中…", iw, DIM);
        draw_screen_bottom(app, &message, hints, area, buf);
        return;
    };
    let c = &d.card;
    let ui = QuestionUi {
        marks: &app.marks,
        cursor: view.q_cursor,
        focused: view.focus == Section::Questions,
    };
    let content = layout::card_content(d, area.width, app.today, ui, app.look.tables);
    let mut y = draw_screen_title(app, &content.title, area, buf);

    // Info row, dim: epic · lane · origin · priority · owner · created · updated.
    let mut info: Vec<String> = Vec::new();
    if let Some(e) = &d.epic_name {
        info.push(one_line(e));
    }
    info.extend([
        one_line(&d.lane.name),
        origin_label(c.origin).to_string(),
        priority_label(c.priority),
        one_line(&c.owner),
        format!("建立 {}", fmt_time(c.created_at, app.today)),
        format!("更新 {}", fmt_time(c.updated_at, app.today)),
    ]);
    put(
        buf,
        margin_x(area),
        y,
        iw as u16,
        fit_spans(vec![Span::styled(info.join(" · "), DIM)], iw, Style::new()),
    );
    y += 1;
    let blocked_by = app.blocked_by(c.seq);
    let blocked = c.blocked_reason.is_some() || !blocked_by.is_empty();
    if blocked {
        let mut spans = vec![Span::styled("阻塞：", BOLD.fg(Color::Red))];
        if let Some(reason) = &c.blocked_reason {
            spans.push(Span::raw(one_line(reason)));
        }
        if !blocked_by.is_empty() {
            let list = blocked_by
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join("、");
            let gap = if c.blocked_reason.is_some() { "  " } else { "" };
            spans.push(Span::styled(
                format!("{gap}被 {list} 阻擋"),
                Style::new().fg(Color::Red),
            ));
        }
        put(
            buf,
            margin_x(area),
            y,
            iw as u16,
            fit_spans(spans, iw, Style::new()),
        );
        y += 1;
    }
    // The model judge's active tags, one row each, like the blocked row (docs/tui.md
    // section 12.4).
    for (label, style, text) in app.card_judge(c.id).detail_lines() {
        let spans = vec![Span::styled(label, style), Span::raw(text)];
        put(
            buf,
            margin_x(area),
            y,
            iw as u16,
            fit_spans(spans, iw, Style::new()),
        );
        y += 1;
    }

    let size = (area.width, area.height);
    let shown = Section::shown(!d.questions.is_empty());
    let natural: Vec<usize> = shown.iter().map(|s| content.lines(*s)).collect();
    let head = layout::card_head_rows(size, content.title.len(), app.card_status_rows(d));
    let heights = layout::section_heights(size, head, &natural, message.len());
    // The row of the question cursor; when the question's text does not fit with it, `↑`
    // scrolls up into the text (docs/questions.md §7.3) and the key hints say so.
    let cursor_row = content
        .choices
        .iter()
        .find(|r| Some((r.question, r.index)) == view.q_cursor);
    let q_height = shown
        .iter()
        .position(|s| *s == Section::Questions)
        .map_or(1, |k| heights[k]);
    if view.focus == Section::Questions
        && cursor_row.is_some_and(|r| !r.fits_with_text(q_height))
        && let Some(p) = card_pairs.iter_mut().find(|p| p.0 == "↑↓")
    {
        p.1 = "選擇／看提問";
    }
    let hints: &[(&str, &str)] = if app.input_target().is_some() {
        INPUT_HINTS
    } else {
        &card_pairs
    };

    for (k, section) in shown.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let (n, h) = (natural[k], heights[k]);
        let focused = view.focus == *section;
        let page = |first: usize, lines: &[Line<'static>]| -> Vec<Line<'static>> {
            lines.iter().skip(first).take(h).cloned().collect()
        };
        match section {
            Section::Questions => {
                let open = d.questions.iter().filter(|q| q.is_open()).count();
                let title = if open > 0 {
                    format!("提問 ({open} 待回答)")
                } else {
                    format!("提問 ({})", d.questions.len())
                };
                let first = view.q_scroll.min(n.saturating_sub(h));
                y = draw_section_title(
                    app,
                    buf,
                    area,
                    y,
                    &title,
                    focused,
                    &section_range(first, h, n),
                );
                // The text of the question under the cursor may be pinned at the top.
                let lines: Vec<Line<'static>> = layout::question_window(first, cursor_row, h, n)
                    .into_iter()
                    .map(|i| content.questions[i].clone())
                    .collect();
                y = draw_lines(buf, area, &lines, y, bottom);
            }
            Section::Body => {
                let first = view.body_scroll.min(n.saturating_sub(h));
                y = draw_section_title(
                    app,
                    buf,
                    area,
                    y,
                    "內容",
                    focused,
                    &section_range(first, h, n),
                );
                y = draw_lines(buf, area, &page(first, &content.body), y, bottom);
            }
            Section::Notes => {
                let first = view.notes_scroll.min(n.saturating_sub(h));
                y = draw_section_title(
                    app,
                    buf,
                    area,
                    y,
                    &format!("紀錄 ({})", d.notes.len()),
                    focused,
                    &section_range(first, h, n),
                );
                y = draw_lines(buf, area, &page(first, &content.notes), y, bottom);
            }
            Section::Links => {
                let links_h = h.max(1);
                let link_first = view.link_sel.saturating_sub(links_h - 1);
                y = draw_section_title(
                    app,
                    buf,
                    area,
                    y,
                    &format!("連結 ({})", content.links.len()),
                    focused,
                    &section_range(link_first, h, content.links.len()),
                );
                let lines: Vec<Line<'static>> = if content.links.is_empty() {
                    vec![Line::from(Span::styled(
                        format!("{}（沒有連結）", layout::indent()),
                        DIM,
                    ))]
                } else {
                    content
                        .links
                        .iter()
                        .enumerate()
                        .skip(link_first)
                        .take(h)
                        .map(|(i, (dir, target, label))| {
                            let is_sel = i == view.link_sel;
                            let (dir, tail) = layout::link_row(
                                dir,
                                target,
                                label.as_deref(),
                                layout::card_text_width(area.width),
                            );
                            // `▶` at the side margin; the reverse video covers the text.
                            let style = if is_sel && focused {
                                Style::new().add_modifier(Modifier::REVERSED)
                            } else {
                                Style::new()
                            };
                            Line::from(vec![
                                Span::raw(if is_sel {
                                    layout::marker()
                                } else {
                                    layout::indent()
                                }),
                                Span::styled(dir, style.fg(ACCENT)),
                                Span::styled(tail, style),
                            ])
                        })
                        .collect()
                };
                y = draw_lines(buf, area, &lines, y, bottom);
            }
        }
    }
    draw_screen_bottom(app, &message, hints, area, buf);
}

// ----- overlays -----------------------------------------------------------------------

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn popup_block(title: &str, hint: &[(&str, &str)]) -> Block<'static> {
    let mut hints = key_bar(hint);
    hints.push(Span::raw(" "));
    Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(format!(" {title} "), BOLD.fg(ACCENT)))
        .title_bottom(Line::from(hints))
}

const PICK_HINTS: &[(&str, &str)] = &[("↑↓", "選擇"), ("Enter", "確認"), ("Esc", "取消")];

/// Draws a list of rows; `rows(i, w, selected)` builds one row.
fn draw_list<T>(
    picker: &Picker<T>,
    inner: Rect,
    buf: &mut Buffer,
    empty: &str,
    row: impl Fn(&T, usize, bool) -> Vec<Span<'static>>,
) {
    let Some(items) = &picker.items else {
        buf.set_stringn(inner.x + 1, inner.y, "載入中…", inner.width as usize, DIM);
        return;
    };
    if items.is_empty() {
        buf.set_stringn(inner.x + 1, inner.y, empty, inner.width as usize, DIM);
        return;
    }
    let h = inner.height as usize;
    let first = picker.sel.saturating_sub(h.saturating_sub(1));
    for (slot, (i, item)) in items.iter().enumerate().skip(first).take(h).enumerate() {
        let selected = i == picker.sel;
        let spans = row(item, inner.width as usize, selected);
        put(buf, inner.x, inner.y + slot as u16, inner.width, spans);
    }
}

fn draw_workspaces(_app: &App, picker: &Picker<WorkspaceSummary>, area: Rect, buf: &mut Buffer) {
    let n = picker.items.as_ref().map_or(1, |v| v.len().max(1));
    let rect = popup(area, 64, n as u16 + 2);
    Clear.render(rect, buf);
    let block = popup_block("workspace", PICK_HINTS);
    let inner = block.inner(rect);
    block.render(rect, buf);
    draw_list(
        picker,
        inner,
        buf,
        " （沒有 workspace）",
        |ws, w, selected| {
            let right = format!("  進行中 {} ", ws.active_boards);
            let style = if selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else if ws.active_boards == 0 {
                DIM
            } else {
                Style::new()
            };
            let left = format!("{}{}", if selected { "▶" } else { " " }, one_line(&ws.name));
            let left_w = w.saturating_sub(width(&right));
            vec![
                Span::styled(fit(&left, left_w), style),
                Span::styled(right, style),
            ]
        },
    );
}

fn draw_boards(
    app: &App,
    workspace: Option<&str>,
    picker: &Picker<BoardSummary>,
    area: Rect,
    buf: &mut Buffer,
) {
    let n = picker.items.as_ref().map_or(1, |v| v.len().max(1));
    let rect = popup(area, 100, n as u16 + 2);
    Clear.render(rect, buf);
    let title = match workspace {
        Some(name) => format!("看板 · {}", one_line(name)),
        None => "看板 · 全部".to_string(),
    };
    let block = popup_block(&title, PICK_HINTS);
    let inner = block.inner(rect);
    block.render(rect, buf);
    let today = app.today;
    draw_list(
        picker,
        inner,
        buf,
        " （沒有看板）",
        |b, w, selected| {
            let archived = b.status == BoardStatus::Archived;
            let mut style = if archived { DIM } else { Style::new() };
            if selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let counts = [
                (LaneKind::Todo, b.cards.todo),
                (LaneKind::Doing, b.cards.doing),
                (LaneKind::Done, b.cards.done),
                (LaneKind::Dropped, b.cards.dropped),
            ]
            .iter()
            .map(|(k, n)| format!("{} {n}", lane_kind_label(*k)))
            .collect::<Vec<_>>()
            .join(" · ");
            let time = fit(&fmt_time(b.updated_at, today), 11);
            let right = format!("  {counts}  {time} ");
            let title = if b.title.trim().is_empty() {
                b.session_key.clone()
            } else {
                one_line(&b.title)
            };
            let left = format!(
                "{}{} {title}",
                if selected { "▶" } else { " " },
                if archived { "○" } else { "●" }
            );
            let left_w = w.saturating_sub(width(&right));
            vec![
                Span::styled(fit(&left, left_w), style),
                Span::styled(fit(&right, w.min(width(&right))), style),
            ]
        },
    );
}

type HelpSection = (&'static str, &'static [(&'static str, &'static str)]);

const HELP_LEFT: &[HelpSection] = &[
    (
        "看板畫面",
        &[
            ("←→ h l", "選泳道"),
            ("← 最左泳道", "焦點移到側欄"),
            ("↑↓ k j", "選卡片"),
            ("Home End", "第一張或最後一張卡片"),
            ("Enter", "開啟卡片"),
            ("a", "跳到有待回答提問的卡片"),
            ("i", "對選取的卡片留言"),
            ("e E", "下一個或上一個 epic"),
            ("o", "切換排序（只影響畫面）"),
            ("s", "顯示或隱藏側欄"),
            ("Esc", "取消 epic 的選取"),
            ("w b", "workspace 或看板選單"),
            ("r ?", "重新載入、說明"),
            ("q Ctrl+C", "離開"),
        ],
    ),
    (
        "側欄",
        &[
            ("↑↓ Home End", "選 epic 或未歸類"),
            ("Enter", "開啟 epic 畫面"),
            ("→ l", "回到泳道"),
        ],
    ),
];

const HELP_RIGHT: &[HelpSection] = &[
    (
        "卡片畫面",
        &[
            ("Tab Shift+Tab", "下一個或上一個區塊"),
            ("↑↓", "捲動，或移動游標"),
            ("PgUp PgDn", "捲動一頁"),
            ("Space", "勾選提問的選擇"),
            ("Enter", "送出提問或前往連結"),
            ("i", "留言；選項上是打字"),
            ("← Backspace", "返回上一層"),
            ("Esc", "直接回到看板"),
        ],
    ),
    (
        "epic 畫面",
        &[
            ("↑↓ PgUp PgDn", "選卡片"),
            ("Enter i", "開啟卡片、留言"),
            ("← Backspace", "返回上一層"),
            ("Esc", "直接回到看板"),
        ],
    ),
    (
        "輸入列與留言列：直接打字",
        &[
            ("←→ Home End", "移動輸入游標"),
            ("Bksp Del", "刪除一個字"),
            ("Ctrl+U", "清除文字"),
        ],
    ),
];

/// Help sections stacked in one column, a blank row between them.
fn help_column(sections: &[HelpSection]) -> Vec<Vec<Span<'static>>> {
    let mut out = Vec::new();
    for (i, (title, rows)) in sections.iter().enumerate() {
        if i > 0 {
            out.push(Vec::new());
        }
        let kw = rows.iter().map(|(k, _)| width(k)).max().unwrap_or(0);
        out.push(vec![Span::styled(format!(" {title}"), BOLD.fg(ACCENT))]);
        for (k, d) in rows.iter() {
            out.push(vec![
                Span::styled(format!(" {}", fit(k, kw)), BOLD),
                Span::raw(format!("  {d}")),
            ]);
        }
    }
    out
}

fn draw_help(area: Rect, buf: &mut Buffer) {
    let left = help_column(HELP_LEFT);
    let right = help_column(HELP_RIGHT);
    let lw = left.iter().map(|l| spans_width(l)).max().unwrap_or(0) + 2;
    let rw = right.iter().map(|l| spans_width(l)).max().unwrap_or(0) + 1;
    let side_by_side = (lw + rw + 2) as u16 <= area.width.saturating_sub(2);
    let (w, h) = if side_by_side {
        (lw + rw + 2, left.len().max(right.len()) + 2)
    } else {
        (lw.max(rw) + 2, left.len() + right.len() + 3)
    };
    let rect = popup(area, w as u16, h as u16);
    Clear.render(rect, buf);
    let block = popup_block("說明", &[("任意鍵", "關閉")]);
    let inner = block.inner(rect);
    block.render(rect, buf);
    let iw = inner.width as usize;
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    if side_by_side {
        for i in 0..left.len().max(right.len()) {
            let mut row = fit_spans(left.get(i).cloned().unwrap_or_default(), lw, Style::new());
            row.extend(right.get(i).cloned().unwrap_or_default());
            rows.push(row);
        }
    } else {
        rows.extend(left);
        rows.push(Vec::new());
        rows.extend(right);
    }
    for (i, row) in rows.into_iter().take(inner.height as usize).enumerate() {
        put(
            buf,
            inner.x,
            inner.y + i as u16,
            inner.width,
            fit_spans(row, iw, Style::new()),
        );
    }
}

/// The buffer as plain text, one line per row, trailing spaces removed. Wide characters
/// occupy two cells but are printed once.
pub fn buffer_to_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        let mut skip = 0usize;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            line.push_str(sym);
            skip = width(sym).saturating_sub(1);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
