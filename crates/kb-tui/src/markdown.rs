//! Markdown for the card screen's body and notes (docs/board-v2.md section 6.2).
//!
//! A small line-based renderer for the syntax agents actually write: headings, bold,
//! italic, inline code, fenced code blocks, nested lists, tables, block quotes, links and
//! rules. Anything else is shown as written. Every width is a display width; a Chinese
//! character is 2 cells. Each source line is its own line on screen (no joining of soft
//! line breaks), so text keeps the line breaks its author typed.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text::{char_width, is_closing, take_last_unit, tokens, width};

const DIM: Style = Style::new().fg(Color::DarkGray);
const CODE: Style = Style::new().fg(Color::Cyan);

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq)]
struct Seg {
    text: String,
    style: Style,
}

impl Seg {
    fn new(text: impl Into<String>, style: Style) -> Seg {
        Seg {
            text: text.into(),
            style,
        }
    }
}

fn segs_width(segs: &[Seg]) -> usize {
    segs.iter().map(|s| width(&s.text)).sum()
}

fn to_line(segs: Vec<Seg>) -> Line<'static> {
    Line::from(
        segs.into_iter()
            .filter(|s| !s.text.is_empty())
            .map(|s| Span::styled(s.text, s.style))
            .collect::<Vec<_>>(),
    )
}

/// How tables are drawn. Every line of a table is dim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableStyle {
    /// Bold header, one `─` rule per column under it, no box (docs/board-v2.md 6.2).
    HeaderRule,
    /// Full-width rules above the header, under it and below the last row; no vertical
    /// lines.
    Book,
    /// A full box, one space of padding inside each cell, bold header.
    Grid,
}

/// THE SWITCH for the table style of the card and epic screens (the user chose `Grid`).
pub const TABLE_STYLE: TableStyle = TableStyle::Grid;

/// Renders `src` to lines at most `width` cells wide, tables in [`TABLE_STYLE`].
pub fn render(src: &str, width: usize) -> Vec<Line<'static>> {
    render_with(src, width, TABLE_STYLE)
}

/// Renders `src` to lines at most `width` cells wide. Code block lines are never
/// wrapped (they may be wider and are cut by the screen edge).
pub fn render_with(src: &str, width: usize, tables: TableStyle) -> Vec<Line<'static>> {
    let width = width.max(1);
    let lines: Vec<String> = src
        .split('\n')
        .map(|l| l.trim_end_matches('\r').replace('\t', "    "))
        .collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut blank_pending = false;
    // Indents of the open list items (docs: nested lists indent level by level).
    let mut list_stack: Vec<usize> = Vec::new();
    let mut i = 0;
    let push_blank = |out: &mut Vec<Line<'static>>, pending: &mut bool| {
        if *pending && !out.is_empty() {
            out.push(Line::default());
        }
        *pending = false;
    };
    while i < lines.len() {
        let line = &lines[i];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.is_empty() {
            blank_pending = true;
            i += 1;
            continue;
        }
        push_blank(&mut out, &mut blank_pending);

        // Fenced code block.
        if let Some(fence) = fence_of(trimmed) {
            list_stack.clear();
            i += 1;
            while i < lines.len() {
                let l = &lines[i];
                if l.trim_start().starts_with(&fence)
                    && l.trim().chars().all(|c| fence.starts_with(c))
                {
                    i += 1;
                    break;
                }
                let strip = l.len() - l.trim_start().len();
                let text = &l[strip.min(indent)..];
                out.push(to_line(vec![
                    Seg::new("  ", Style::new()),
                    Seg::new(text, DIM),
                ]));
                i += 1;
            }
            continue;
        }

        // Heading.
        if let Some((level, text)) = heading_of(trimmed) {
            list_stack.clear();
            let mut style = Style::new().add_modifier(Modifier::BOLD);
            if level == 1 {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let segs = inline(text, style);
            out.extend(wrap_segs(&segs, width).into_iter().map(to_line));
            i += 1;
            continue;
        }

        // Rule.
        if is_rule(trimmed) {
            list_stack.clear();
            out.push(to_line(vec![Seg::new("─".repeat(width), DIM)]));
            i += 1;
            continue;
        }

        // Block quote: the quoted text is rendered as Markdown, then dimmed.
        if trimmed.starts_with('>') {
            list_stack.clear();
            let mut inner = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let t = lines[i].trim_start()[1..].to_string();
                inner.push(t.strip_prefix(' ').map(str::to_string).unwrap_or(t));
                i += 1;
            }
            for l in render_with(&inner.join("\n"), width.saturating_sub(2).max(1), tables) {
                let mut spans = vec![Span::styled("│ ", DIM)];
                spans.extend(l.spans.into_iter().map(|s| {
                    let style = s.style.fg(Color::DarkGray);
                    Span::styled(s.content, style)
                }));
                out.push(Line::from(spans));
            }
            continue;
        }

        // Table: a row with `|` followed by a delimiter row.
        if trimmed.contains('|') && i + 1 < lines.len() && is_delimiter_row(&lines[i + 1]) {
            list_stack.clear();
            let header = split_row(trimmed);
            let aligns = split_row(lines[i + 1].trim())
                .iter()
                .map(|c| align_of(c))
                .collect::<Vec<_>>();
            let mut rows = Vec::new();
            i += 2;
            while i < lines.len() {
                let t = lines[i].trim();
                if t.is_empty() || !t.contains('|') {
                    break;
                }
                rows.push(split_row(t));
                i += 1;
            }
            match table(&header, &aligns, &rows, width, tables) {
                Some(t) => out.extend(t),
                None => {
                    // Too narrow for a table: the rows as plain text.
                    for r in std::iter::once(&header).chain(rows.iter()) {
                        let segs = inline(&r.join(" | "), Style::new());
                        out.extend(wrap_segs(&segs, width).into_iter().map(to_line));
                    }
                }
            }
            continue;
        }

        // List item (with continuation lines indented under its text).
        if let Some((marker, text)) = list_item_of(trimmed) {
            while let Some(&top) = list_stack.last() {
                if indent >= top + 2 {
                    break;
                }
                list_stack.pop();
            }
            let level = list_stack.len();
            list_stack.push(indent);
            let mut texts = vec![text.to_string()];
            i += 1;
            while i < lines.len() {
                let l = &lines[i];
                let t = l.trim_start();
                let ind = l.len() - t.len();
                if t.is_empty() || ind <= indent || starts_block(t) {
                    break;
                }
                texts.push(t.to_string());
                i += 1;
            }
            let prefix = format!("{}{marker} ", "  ".repeat(level));
            let pw = width_of(&prefix);
            let avail = width.saturating_sub(pw).max(1);
            let mut first = true;
            for t in texts {
                for l in wrap_segs(&inline(&t, Style::new()), avail) {
                    let head = if first {
                        Seg::new(prefix.clone(), Style::new().fg(Color::Yellow))
                    } else {
                        Seg::new(" ".repeat(pw), Style::new())
                    };
                    first = false;
                    let mut segs = vec![head];
                    segs.extend(l);
                    out.push(to_line(segs));
                }
            }
            continue;
        }

        // Plain paragraph line. A list continues across a paragraph only when the
        // paragraph is indented under it.
        if indent == 0 {
            list_stack.clear();
        }
        let segs = inline(trimmed, Style::new());
        out.extend(wrap_segs(&segs, width).into_iter().map(to_line));
        i += 1;
    }
    out
}

fn width_of(s: &str) -> usize {
    width(s)
}

/// The fence string (``` or ~~~, 3 or more) that opens a code block.
fn fence_of(t: &str) -> Option<String> {
    for ch in ['`', '~'] {
        let n = t.chars().take_while(|c| *c == ch).count();
        if n >= 3 {
            return Some(std::iter::repeat_n(ch, n).collect());
        }
    }
    None
}

fn heading_of(t: &str) -> Option<(usize, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let rest = &t[n..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    Some((n, text))
}

fn is_rule(t: &str) -> bool {
    let chars: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
    chars.len() >= 3 && ['-', '*', '_'].iter().any(|m| chars.iter().all(|c| c == m))
}

/// `-`, `*`, `+` or `1.` / `1)` followed by a space. Bullets are shown as `-`.
fn list_item_of(t: &str) -> Option<(String, &str)> {
    let mut chars = t.char_indices();
    let (_, first) = chars.next()?;
    if matches!(first, '-' | '*' | '+') {
        let rest = &t[1..];
        if rest.starts_with(' ') {
            return Some(("-".to_string(), rest.trim_start()));
        }
        return None;
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let after = &t[digits..];
        if let Some(rest) = after.strip_prefix('.').or_else(|| after.strip_prefix(')'))
            && (rest.is_empty() || rest.starts_with(' '))
        {
            return Some((t[..digits + 1].to_string(), rest.trim_start()));
        }
    }
    None
}

fn starts_block(t: &str) -> bool {
    fence_of(t).is_some()
        || heading_of(t).is_some()
        || is_rule(t)
        || t.starts_with('>')
        || list_item_of(t).is_some()
}

// ----- tables ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

fn is_delimiter_row(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') {
        return false;
    }
    let cells = split_row(t);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            !c.is_empty() && c.contains('-') && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

fn align_of(cell: &str) -> Align {
    let c = cell.trim();
    match (c.starts_with(':'), c.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    }
}

/// Cells of a table row: split on `|` (not `\|`, not inside backticks), outer pipes
/// optional.
fn split_row(t: &str) -> Vec<String> {
    let mut t = t.trim();
    if let Some(rest) = t.strip_prefix('|') {
        t = rest;
    }
    if t.ends_with('|') && !t.ends_with("\\|") {
        t = &t[..t.len() - 1];
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// A table at most `width` cells wide, columns aligned by display width, bold header,
/// every line dim. The style decides the lines (see [`TableStyle`]): `HeaderRule` and
/// `Book` put two spaces between columns and no vertical lines; `Grid` draws a box with
/// one space of padding inside each cell. When the table is too wide, the widest column
/// is narrowed (and wraps) until it fits. `None` when even one cell per column does not
/// fit.
fn table(
    header: &[String],
    aligns: &[Align],
    rows: &[Vec<String>],
    width: usize,
    style: TableStyle,
) -> Option<Vec<Line<'static>>> {
    let ncols = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let grid = style == TableStyle::Grid;
    // Cells besides the text: two spaces between columns, or `│ ` … ` │` for a grid.
    let chrome = if grid {
        ncols * 3 + 1
    } else {
        (ncols - 1) * GAP.len()
    };
    if width < chrome + ncols {
        return None;
    }
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let parse = |row: &[String], style: Style| -> Vec<Vec<Seg>> {
        (0..ncols)
            .map(|c| inline(row.get(c).map(String::as_str).unwrap_or(""), style))
            .collect()
    };
    let head = parse(header, bold);
    let body: Vec<Vec<Vec<Seg>>> = rows.iter().map(|r| parse(r, Style::new())).collect();
    let mut widths: Vec<usize> = (0..ncols)
        .map(|c| {
            std::iter::once(&head)
                .chain(body.iter())
                .map(|r| segs_width(&r[c]))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let budget = width - chrome;
    while widths.iter().sum::<usize>() > budget {
        let (k, _) = widths
            .iter()
            .enumerate()
            .max_by_key(|(i, w)| (**w, std::cmp::Reverse(*i)))
            .expect("at least one column");
        widths[k] -= 1;
    }
    let total = widths.iter().sum::<usize>() + chrome;

    let dim_line = |s: String| Line::from(Span::styled(s, DIM));
    // HeaderRule: one rule per column, as wide as the column.
    let column_rule = || {
        dim_line(
            widths
                .iter()
                .map(|w| "─".repeat(*w))
                .collect::<Vec<_>>()
                .join(GAP),
        )
    };
    // Book: one rule as wide as the table.
    let full_rule = || dim_line("─".repeat(total));
    // Grid: box lines.
    let grid_line = |l: &str, m: &str, r: &str| {
        dim_line(format!(
            "{l}{}{r}",
            widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join(m)
        ))
    };
    let row_lines = |row: &[Vec<Seg>]| -> Vec<Line<'static>> {
        let wrapped: Vec<Vec<Vec<Seg>>> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| wrap_segs(cell, *w))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        (0..height)
            .map(|y| {
                let mut segs = Vec::new();
                if grid {
                    segs.push(Seg::new("│ ", DIM));
                }
                for (c, cell) in wrapped.iter().enumerate() {
                    if c > 0 {
                        segs.push(if grid {
                            Seg::new(" │ ", DIM)
                        } else {
                            Seg::new(GAP, Style::new())
                        });
                    }
                    let line = cell.get(y).cloned().unwrap_or_default();
                    let room = widths[c].saturating_sub(segs_width(&line));
                    let (before, after) = match aligns.get(c).copied().unwrap_or(Align::Left) {
                        Align::Left => (0, room),
                        Align::Right => (room, 0),
                        Align::Center => (room / 2, room - room / 2),
                    };
                    segs.push(Seg::new(" ".repeat(before), Style::new()));
                    segs.extend(line);
                    // Without a right border, no padding after the last column.
                    if grid || c + 1 < wrapped.len() {
                        segs.push(Seg::new(" ".repeat(after), Style::new()));
                    }
                }
                if grid {
                    segs.push(Seg::new(" │", DIM));
                }
                to_line(segs)
            })
            .collect()
    };
    let mut out = Vec::new();
    match style {
        TableStyle::HeaderRule => {
            out.extend(row_lines(&head));
            out.push(column_rule());
            for r in &body {
                out.extend(row_lines(r));
            }
        }
        TableStyle::Book => {
            out.push(full_rule());
            out.extend(row_lines(&head));
            out.push(full_rule());
            for r in &body {
                out.extend(row_lines(r));
            }
            out.push(full_rule());
        }
        TableStyle::Grid => {
            out.push(grid_line("┌", "┬", "┐"));
            out.extend(row_lines(&head));
            out.push(grid_line("├", "┼", "┤"));
            for r in &body {
                out.extend(row_lines(r));
            }
            out.push(grid_line("└", "┴", "┘"));
        }
    }
    Some(out)
}

/// Space between table columns.
const GAP: &str = "  ";

// ----- inline ---------------------------------------------------------------------------

/// Inline Markdown: `code`, **bold**, *italic*, ***both***, [text](url), <url>, and
/// backslash escapes. Unmatched markers stay as written.
fn inline(s: &str, base: Style) -> Vec<Seg> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Seg> = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<Seg>| {
        if !buf.is_empty() {
            out.push(Seg::new(std::mem::take(buf), base));
        }
    };
    let collect = |from: usize, to: usize| -> String { chars[from..to].iter().collect() };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Escapes.
        if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        // Inline code.
        if c == '`' {
            let n = run(&chars, i, '`');
            if let Some(close) = find_run(&chars, i + n, '`', n) {
                flush(&mut buf, &mut out);
                let code = collect(i + n, close);
                let code = if code.starts_with(' ') && code.ends_with(' ') && code.len() > 2 {
                    code[1..code.len() - 1].to_string()
                } else {
                    code
                };
                out.push(Seg::new(code, base.patch(CODE)));
                i = close + n;
                continue;
            }
            buf.extend(&chars[i..i + n]);
            i += n;
            continue;
        }
        // Links.
        if c == '['
            && let Some((text_end, url_end)) = link_at(&chars, i)
        {
            flush(&mut buf, &mut out);
            let text = collect(i + 1, text_end);
            let url = collect(text_end + 2, url_end);
            let url = url.split_whitespace().next().unwrap_or("").to_string();
            out.extend(inline(&text, base));
            if !url.is_empty() && url != text {
                out.push(Seg::new(format!(" ({url})"), base.patch(DIM)));
            }
            i = url_end + 1;
            continue;
        }
        if c == '<'
            && let Some(end) = chars[i + 1..].iter().position(|ch| *ch == '>')
        {
            let inner = collect(i + 1, i + 1 + end);
            if (inner.starts_with("http://") || inner.starts_with("https://"))
                && !inner.contains(' ')
            {
                flush(&mut buf, &mut out);
                out.push(Seg::new(inner, base.patch(DIM)));
                i += end + 2;
                continue;
            }
        }
        // Emphasis.
        if c == '*' || c == '_' {
            let n = run(&chars, i, c);
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            let next = chars.get(i + n).copied();
            let can_open = next.is_some_and(|ch| !ch.is_whitespace())
                && (c == '*' || !prev.is_some_and(|p| p.is_alphanumeric()));
            if can_open
                && n <= 3
                && let Some(close) = find_closer(&chars, i + n, c, n)
            {
                flush(&mut buf, &mut out);
                let mut style = base;
                if n >= 2 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if n != 2 {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                let inner = collect(i + n, close);
                out.extend(inline(&inner, style));
                i = close + n;
                continue;
            }
            buf.extend(&chars[i..i + n]);
            i += n;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

/// Length of the run of `ch` starting at `i`.
fn run(chars: &[char], i: usize, ch: char) -> usize {
    chars[i..].iter().take_while(|c| **c == ch).count()
}

/// Start of the next run of exactly `n` `ch` at or after `from`.
fn find_run(chars: &[char], from: usize, ch: char, n: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == ch {
            let m = run(chars, j, ch);
            if m == n {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

/// The closing emphasis run for an opener of `n` markers: a run of at least `n` `ch` that
/// follows a non-space character (for `_`, not followed by a letter or digit).
fn find_closer(chars: &[char], from: usize, ch: char, n: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == '\\' {
            j += 2;
            continue;
        }
        if chars[j] == '`' {
            let m = run(chars, j, '`');
            match find_run(chars, j + m, '`', m) {
                Some(end) => j = end + m,
                None => j += m,
            }
            continue;
        }
        if chars[j] == ch {
            let m = run(chars, j, ch);
            let prev_ok = j > from && !chars[j - 1].is_whitespace();
            let after = chars.get(j + m).copied();
            let after_ok = ch == '*' || !after.is_some_and(|a| a.is_alphanumeric());
            if m >= n && prev_ok && after_ok {
                // A closer of more markers closes at its end (`***` closing `**`
                // leaves the first `*` as text inside).
                return Some(j + m - n);
            }
            j += m;
            continue;
        }
        j += 1;
    }
    None
}

/// `[text](url)` starting at `i`: returns (index of `]`, index of `)`).
fn link_at(chars: &[char], i: usize) -> Option<(usize, usize)> {
    let mut depth = 0;
    let mut j = i;
    let text_end = loop {
        let c = *chars.get(j)?;
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break j;
                }
            }
            _ => {}
        }
        j += 1;
    };
    if chars.get(text_end + 1) != Some(&'(') {
        return None;
    }
    let close = chars[text_end + 2..].iter().position(|c| *c == ')')?;
    Some((text_end, text_end + 2 + close))
}

// ----- wrapping -------------------------------------------------------------------------

/// Takes the last unit (wide character or ASCII word) off a styled line, to move it to
/// the next line together with a closing mark.
fn carry_unit(line: &mut Vec<Seg>, max: usize) -> Option<Seg> {
    let n = line.len();
    let last = line.last_mut()?;
    let mut text = last.text.clone();
    if let Some(unit) = take_last_unit(&mut text, max) {
        last.text = text;
        return Some(Seg::new(unit, last.style));
    }
    // The whole last segment may be the unit, with other segments before it.
    let mut probe = format!("\u{3000}{}", last.text);
    if n > 1 && take_last_unit(&mut probe, max).as_deref() == Some(last.text.as_str()) {
        return line.pop();
    }
    None
}

/// Wraps styled text to lines of at most `w` cells, with the rules of
/// [`crate::text::wrap`]: words of ASCII letters stay whole unless wider than a line,
/// CJK text breaks between any two characters, a space at a break is dropped.
fn wrap_segs(segs: &[Seg], w: usize) -> Vec<Vec<Seg>> {
    let w = w.max(1);
    let mut lines: Vec<Vec<Seg>> = Vec::new();
    let mut line: Vec<Seg> = Vec::new();
    let mut line_w = 0;
    let push = |line: &mut Vec<Seg>, text: &str, style: Style| match line.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => line.push(Seg::new(text, style)),
    };
    let finish = |line: &mut Vec<Seg>, lines: &mut Vec<Vec<Seg>>| {
        // Drop trailing spaces.
        while let Some(last) = line.last_mut() {
            let t = last.text.trim_end_matches(' ').to_string();
            if t.is_empty() {
                line.pop();
            } else {
                last.text = t;
                break;
            }
        }
        lines.push(std::mem::take(line));
    };
    for seg in segs {
        for token in tokens(&seg.text) {
            let tw = width(token);
            if line_w + tw <= w {
                push(&mut line, token, seg.style);
                line_w += tw;
                continue;
            }
            if token == " " {
                finish(&mut line, &mut lines);
                line_w = 0;
                continue;
            }
            if line_w > 0 {
                // A closing mark does not start a line (see `text::wrap`).
                let carry = if is_closing(token) {
                    carry_unit(&mut line, w.saturating_sub(tw))
                } else {
                    None
                };
                finish(&mut line, &mut lines);
                line_w = 0;
                if let Some(c) = carry {
                    line_w = width(&c.text);
                    line.push(c);
                }
            }
            if line_w + tw <= w {
                push(&mut line, token, seg.style);
                line_w += tw;
            } else {
                for c in token.chars() {
                    let cw = char_width(c);
                    if line_w + cw > w && line_w > 0 {
                        finish(&mut line, &mut lines);
                        line_w = 0;
                    }
                    push(&mut line, c.encode_utf8(&mut [0; 4]), seg.style);
                    line_w += cw;
                }
            }
        }
    }
    finish(&mut line, &mut lines);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn inline_styles() {
        let segs = inline(
            "用 `kb ask` 提問，**不要**停下，*斜體*與 snake_case_name",
            Style::new(),
        );
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "用 ",
                "kb ask",
                " 提問，",
                "不要",
                "停下，",
                "斜體",
                "與 snake_case_name"
            ]
        );
        assert_eq!(segs[1].style.fg, Some(Color::Cyan));
        assert!(segs[3].style.add_modifier.contains(Modifier::BOLD));
        assert!(segs[5].style.add_modifier.contains(Modifier::ITALIC));
        let segs = inline("見 [規格](docs/board-v2.md) 與 2 * 3 * 4", Style::new());
        let text: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "見 規格 (docs/board-v2.md) 與 2 * 3 * 4");
        let segs = inline("***兩者***", Style::new());
        assert_eq!(segs.len(), 1);
        assert!(
            segs[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
        );
    }

    /// Display column where the text after the first run of two spaces starts, i.e. the
    /// start of the second table column.
    fn second_column(line: &str) -> Option<usize> {
        let at = line.find("  ")?;
        let rest = &line[at..];
        let start = at + (rest.len() - rest.trim_start().len());
        Some(width(&line[..start]))
    }

    #[test]
    fn borderless_table_with_chinese_cells_stays_aligned() {
        let src = "| 部分 | 內容 | 負責 |\n|------|:----:|-----:|\n| 資料 | epic、來源、回顧 | 後端 |\n| 讀取 API | 統計 | TUI |";
        let lines = render_with(src, 60, TableStyle::HeaderRule);
        let out = plain(&lines);
        let sp = |n: usize| " ".repeat(n);
        assert_eq!(
            out,
            vec![
                format!("部分{}內容{}負責", sp(12), sp(8)),
                format!("{}  {}  {}", "─".repeat(8), "─".repeat(16), "─".repeat(4)),
                format!("資料{}epic、來源、回顧  後端", sp(6)),
                format!("讀取 API{}統計{}TUI", sp(8), sp(9)),
            ]
        );
        // No frame and no vertical lines.
        assert!(out.iter().all(|l| !l.contains(['│', '┌', '┼', '└'])));
        // Bold header, dim rule.
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::DarkGray));
        // Every row's columns start at the same display columns as the rule's.
        assert_eq!(second_column(&out[1]), Some(10));
        assert_eq!(second_column(&out[2]), Some(10));
    }

    #[test]
    fn overlong_table_wraps_its_widest_column() {
        let long = "這一欄的內容非常長，必須自動換行才能放進區塊的寬度裡面，而且不能弄亂對齊";
        let src = format!("| 項目 | 說明 |\n|---|---|\n| 甲 | {long} |\n| 乙 | 短 |");
        let out = plain(&render_with(&src, 30, TableStyle::HeaderRule));
        assert!(out.len() > 5, "{out:#?}");
        for l in &out {
            assert!(width(l) <= 30, "{l}");
        }
        // The narrow column keeps its natural width (項目 = 4 cells): the rule is
        // `────` + two spaces + the wrapped column, 24 cells wide.
        assert_eq!(out[1], format!("────  {}", "─".repeat(24)));
        // Continuation rows of the wrapped cell start in the second column too.
        for l in &out[2..out.len() - 1] {
            assert_eq!(second_column(l), Some(6), "{l}");
        }
        let text: String = out.iter().map(|l| l.replace([' ', '─'], "")).collect();
        assert!(text.contains(&long.replace(' ', "")), "{out:#?}");
    }

    #[test]
    fn grid_and_book_tables_align_chinese_cells_and_wrap() {
        let src = "| 部分 | 內容 |\n|---|---|\n| 資料 | epic、來源、回顧 |\n| 讀取 API | 統計 |";
        // Grid (the default): a box of dim lines, one space of padding in each cell.
        let lines = render(src, 60);
        assert_eq!(
            plain(&lines),
            vec![
                "┌──────────┬──────────────────┐",
                "│ 部分     │ 內容             │",
                "├──────────┼──────────────────┤",
                "│ 資料     │ epic、來源、回顧 │",
                "│ 讀取 API │ 統計             │",
                "└──────────┴──────────────────┘",
            ]
        );
        assert!(
            lines
                .iter()
                .all(|l| l.spans[0].style.fg == Some(Color::DarkGray))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content == "部分" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        // Book: full-width dim rules above, under the header and at the end.
        let out = plain(&render_with(src, 60, TableStyle::Book));
        let rule = "─".repeat(26);
        assert_eq!(
            out,
            vec![
                rule.clone(),
                "部分      內容".to_string(),
                rule.clone(),
                "資料      epic、來源、回顧".to_string(),
                "讀取 API  統計".to_string(),
                rule,
            ]
        );
        // An overlong grid wraps its widest column and keeps every border aligned.
        let long = "這一欄的內容非常長，必須自動換行才能放進區塊的寬度裡面，而且框線要對齊";
        let src = format!("| 項目 | 說明 |\n|---|---|\n| 甲 | {long} |");
        let out = plain(&render(&src, 30));
        assert!(out.len() > 6, "{out:#?}");
        let cols = |l: &str| -> Vec<usize> {
            let mut col = 0;
            let mut v = Vec::new();
            for c in l.chars() {
                if "│┌┬┐├┼┤└┴┘".contains(c) {
                    v.push(col);
                }
                col += width(c.encode_utf8(&mut [0; 4]));
            }
            v
        };
        let first = cols(&out[0]);
        for l in &out {
            assert!(width(l) <= 30, "{l}");
            assert_eq!(cols(l), first, "{l}");
        }
    }

    #[test]
    fn nested_lists_indent_level_by_level() {
        let src =
            "- 第一層\n  - 第二層\n    - 第三層\n- 回到第一層\n1. 一\n2. 二很長很長很長很長很長";
        let out = plain(&render(src, 16));
        assert_eq!(
            out,
            vec![
                "- 第一層",
                "  - 第二層",
                "    - 第三層",
                "- 回到第一層",
                "1. 一",
                "2. 二很長很長很",
                "   長很長很長",
            ]
        );
    }

    #[test]
    fn code_blocks_keep_their_lines_and_are_not_wrapped() {
        let long = "let x = some_function(argument_one, argument_two, argument_three);";
        let src = format!("前\n```rust\n{long}\n    縮排\n```\n後");
        let lines = render(&src, 20);
        let out = plain(&lines);
        assert_eq!(
            out,
            vec![
                "前".to_string(),
                format!("  {long}"),
                "      縮排".into(),
                "後".into()
            ]
        );
        assert_eq!(lines[1].spans[1].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn paragraphs_wrap_by_display_width_and_keep_line_breaks() {
        let out = plain(&render(
            "排程器每 60 秒執行一次，但是 token 的有效期只有 45 秒。\n第二行",
            20,
        ));
        assert!(out.iter().all(|l| width(l) <= 20), "{out:?}");
        assert_eq!(out.last().unwrap(), "第二行");
        assert!(out.len() >= 3);
    }

    #[test]
    fn headings_rules_quotes_and_unsupported_syntax() {
        let out = plain(&render(
            "# 標題\n## 小標\n---\n> 引言 **粗**\n<div>原文</div>",
            20,
        ));
        assert_eq!(
            out,
            vec![
                "標題",
                "小標",
                &"─".repeat(20),
                "│ 引言 粗",
                "<div>原文</div>"
            ]
        );
    }
}
