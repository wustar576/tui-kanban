//! Display-width text helpers and the Chinese labels of docs/data-model.md.
//!
//! Every width here is a terminal display width (`unicode-width`): a Chinese character
//! is 2 cells. Nothing slices strings by byte or char count.

use chrono::{DateTime, Local, NaiveDate, Utc};
use kb_core::model::{Activity, Cause, CauseKind, LaneKind, LinkKind, NoteKind, Origin, Outcome};
use kb_core::read::LinkDirection;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The ellipsis used for truncation (1 cell wide).
pub const ELLIPSIS: &str = "…";

/// Display width of `s`.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of one character (0 for control characters).
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// One-line form of `s`: line breaks and tabs become spaces, other control characters
/// are dropped.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\r' => {}
            '\n' | '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The longest prefix of `s` that is at most `max` cells wide (no ellipsis).
pub fn take_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// `s` cut to at most `max` cells. When it does not fit, the end is replaced by `…`.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str(ELLIPSIS);
    out
}

/// `s` padded with spaces on the right to exactly `w` cells (truncated with `…` first
/// when it is wider).
pub fn fit(s: &str, w: usize) -> String {
    let mut out = truncate(s, w);
    let used = width(&out);
    out.extend(std::iter::repeat_n(' ', w.saturating_sub(used)));
    out
}

/// `s` padded with spaces on the right to at least `w` cells (never truncated).
pub fn pad(s: &str, w: usize) -> String {
    let mut out = s.to_string();
    out.extend(std::iter::repeat_n(' ', w.saturating_sub(width(s))));
    out
}

/// Wraps text to lines of at most `w` cells. Explicit line breaks are kept. Words of
/// ASCII letters are not split unless a single word is wider than a line; CJK text
/// breaks between any two characters.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let raw = one_line(raw);
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut line_w = 0;
        for token in tokens(&raw) {
            let tw = width(token);
            if line_w + tw <= w {
                line.push_str(token);
                line_w += tw;
                continue;
            }
            if token == " " {
                // A space at the break point is dropped.
                lines.push(std::mem::take(&mut line));
                line_w = 0;
                continue;
            }
            if !line.is_empty() {
                // A closing mark does not start a line: the character before it moves
                // down with it.
                let carry = if is_closing(token) {
                    take_last_unit(&mut line, w.saturating_sub(tw))
                } else {
                    None
                };
                lines.push(std::mem::take(&mut line).trim_end().to_string());
                line_w = 0;
                if let Some(c) = carry {
                    line_w = width(&c);
                    line = c;
                }
            }
            if line_w + tw <= w {
                line.push_str(token);
                line_w += tw;
            } else {
                // A word wider than a line: split it by characters.
                for c in token.chars() {
                    let cw = char_width(c);
                    if line_w + cw > w && !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                        line_w = 0;
                    }
                    line.push(c);
                    line_w += cw;
                }
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

/// CJK closing punctuation, which should not start a line.
pub fn is_closing(token: &str) -> bool {
    let mut chars = token.chars();
    matches!(
        (chars.next(), chars.next()),
        (
            Some(
                '，' | '。'
                    | '、'
                    | '；'
                    | '：'
                    | '！'
                    | '？'
                    | '）'
                    | '」'
                    | '』'
                    | '】'
                    | '》'
                    | '〉'
                    | '〕'
            ),
            None
        )
    )
}

/// Removes and returns the last unit of `line` (a wide character, or a word of ASCII
/// letters) so that it can move to the next line with a closing mark. Leaves `line`
/// alone when that would empty it or the unit is wider than `max`.
pub fn take_last_unit(line: &mut String, max: usize) -> Option<String> {
    let last = line.chars().last()?;
    let start = if last.is_ascii_graphic() {
        line.trim_end_matches(|c: char| c.is_ascii_graphic()).len()
    } else if char_width(last) == 2 && !is_closing(last.encode_utf8(&mut [0; 4])) {
        line.len() - last.len_utf8()
    } else {
        return None;
    };
    if start == 0 || width(&line[start..]) > max {
        return None;
    }
    Some(line.split_off(start))
}

/// [`wrap`] limited to `max` lines; when text is cut, the last line ends with `…`.
pub fn wrap_max(text: &str, w: usize, max: usize) -> Vec<String> {
    let mut lines = wrap(text, w);
    if lines.len() > max && max > 0 {
        lines.truncate(max);
        let last = lines.pop().unwrap_or_default();
        lines.push(format!(
            "{}{ELLIPSIS}",
            take_width(&last, w.saturating_sub(1))
        ));
    }
    lines
}

/// Splits a line into wrap units: runs of ASCII word characters, single spaces, and
/// single other characters (each CJK character is its own unit).
pub fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        let wordy = c.is_ascii_graphic();
        match (start, wordy) {
            (None, true) => start = Some(i),
            (Some(_), true) => {}
            (Some(st), false) => {
                out.push(&s[st..i]);
                start = None;
                out.push(&s[i..i + c.len_utf8()]);
            }
            (None, false) => out.push(&s[i..i + c.len_utf8()]),
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// Local time: `時:分` for today, `月/日 時:分` for other days (docs/tui.md section 9).
pub fn fmt_time(t: DateTime<Utc>, today: NaiveDate) -> String {
    let local = t.with_timezone(&Local);
    if local.date_naive() == today {
        local.format("%H:%M").to_string()
    } else {
        local.format("%m/%d %H:%M").to_string()
    }
}

pub fn priority_label(p: i16) -> String {
    format!("P{p}")
}

pub fn lane_kind_label(kind: LaneKind) -> &'static str {
    match kind {
        LaneKind::Todo => "待處理",
        LaneKind::Doing => "處理中",
        LaneKind::Done => "完成",
        LaneKind::Dropped => "捨棄",
    }
}

pub fn note_kind_label(kind: NoteKind) -> &'static str {
    match kind {
        NoteKind::Progress => "進度",
        NoteKind::Decision => "決定",
        NoteKind::Blocker => "阻擋",
        NoteKind::Result => "結果",
        NoteKind::Reply => "回覆",
        NoteKind::Comment => "留言",
    }
}

/// `card.origin` on screen (docs/board-v2.md section 3.3).
pub fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::Discussing => "討論",
        Origin::Agreed => "約定",
        Origin::Derived => "衍生",
    }
}

pub fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "完成",
        Outcome::Incomplete => "未完成",
        Outcome::Abandoned => "放棄",
    }
}

/// Direction text of a link as seen from one card (docs/data-model.md section 2.6).
pub fn link_label(kind: LinkKind, direction: LinkDirection) -> &'static str {
    use LinkDirection::*;
    match (kind, direction) {
        (LinkKind::Spawned, Outgoing) => "延伸出",
        (LinkKind::Spawned, Incoming) => "延伸自",
        (LinkKind::Blocks, Outgoing) => "阻擋",
        (LinkKind::Blocks, Incoming) => "被阻擋",
        (LinkKind::Relates, _) => "相關",
        (LinkKind::Continues, Outgoing) => "接續",
        (LinkKind::Continues, Incoming) => "被接續",
        (LinkKind::Ref, _) => "參照",
    }
}

/// What 「使用者的指示」 stands for: a `user` cause (docs/board-v2.md section 18.6).
pub const USER_CAUSE: &str = "使用者的指示";

/// A cause on screen: `#30`, `提問 5`, `留言 75` or `使用者的指示`.
pub fn cause_label(kind: CauseKind, id: Option<i64>, seq: Option<i32>) -> String {
    match kind {
        CauseKind::Card => format!("#{}", seq.unwrap_or(0)),
        CauseKind::Question => format!("提問 {}", id.unwrap_or(0)),
        CauseKind::Comment => format!("留言 {}", id.unwrap_or(0)),
        CauseKind::User => USER_CAUSE.to_string(),
    }
}

/// Several causes joined with 「、」, e.g. `#30、提問 5`.
pub fn causes_text(causes: &[Cause]) -> String {
    causes
        .iter()
        .map(|c| cause_label(c.kind, c.id, c.card_seq))
        .collect::<Vec<_>>()
        .join("、")
}

/// The `because` array of an activity detail as text; `None` without causes.
fn activity_because(d: &serde_json::Value) -> Option<String> {
    let items = d.get("because")?.as_array()?;
    let labels: Vec<String> = items
        .iter()
        .filter_map(|v| {
            let kind = v.get("kind")?.as_str()?.parse::<CauseKind>().ok()?;
            let id = v.get("id").and_then(|x| x.as_i64());
            let seq = v.get("seq").and_then(|x| x.as_i64()).map(|n| n as i32);
            Some(cause_label(kind, id, seq))
        })
        .collect();
    (!labels.is_empty()).then(|| labels.join("、"))
}

/// One activity line's description (docs/data-model.md section 2.7), without the time.
pub fn activity_text(a: &Activity) -> String {
    let d = &a.detail;
    let s = |k: &str| -> String {
        match d.get(k) {
            Some(serde_json::Value::String(v)) => one_line(v),
            Some(serde_json::Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        }
    };
    let seq = || format!("#{}", s("seq"));
    let num = |k: &str| d.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let paren = |head: &str, inner: &str| {
        if inner.is_empty() {
            head.to_string()
        } else {
            format!("{head}（{inner}）")
        }
    };
    let text = match a.verb.as_str() {
        "board.create" => format!("建立看板「{}」", s("title")),
        // A changed goal (docs/board-v2.md section 20.2).
        "board.update" => match d.get("goal") {
            Some(_) if s("goal").trim().is_empty() => "看板目標清除".to_string(),
            Some(_) => format!("看板目標更新：{}", s("goal")),
            None => "更新看板".to_string(),
        },
        "board.archive" => {
            let outcome = s("outcome")
                .parse::<Outcome>()
                .map(outcome_label)
                .unwrap_or("");
            if outcome.is_empty() {
                "封存看板".to_string()
            } else {
                format!("封存看板（{outcome}）")
            }
        }
        "board.reopen" => "重新開啟看板".to_string(),
        // docs/board-v2.md sections 12.5 and 13.16.
        "board.adopt" => {
            if d.get("auto").and_then(|v| v.as_bool()) == Some(true) {
                "看板由新的 session 自動接手".to_string()
            } else {
                "看板由新的 session 接手".to_string()
            }
        }
        // docs/board-v2.md section 26.3.
        "board.terminal" => match d.get("workspace") {
            Some(_) => format!(
                "記錄終端機 {}，workspace「{}」",
                s("terminal"),
                s("workspace")
            ),
            None => format!("記錄終端機 {}", s("terminal")),
        },
        "lane.add" => format!("新增泳道「{}」", s("lane")),
        "lane.rename" => format!("泳道「{}」改名為「{}」", s("from"), s("to")),
        "lane.move" => format!("移動泳道「{}」", s("lane")),
        "lane.remove" => format!("移除泳道「{}」", s("lane")),
        "card.add" => format!("{} 新增到「{}」", seq(), s("lane")),
        "card.move" => format!("{} 移到「{}」", seq(), s("to")),
        "card.edit" => {
            let fields: Vec<&str> = d
                .get("fields")
                .and_then(|f| f.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|f| match f {
                            "title" => "標題",
                            "body" => "內容",
                            "priority" => "優先度",
                            "owner" => "負責的 agent",
                            other => other,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if fields.is_empty() {
                format!("{} 修改", seq())
            } else {
                format!("{} 修改{}", seq(), fields.join("、"))
            }
        }
        "card.note" => {
            let kind = s("kind")
                .parse::<NoteKind>()
                .map(note_kind_label)
                .unwrap_or("紀錄");
            // With causes the kind replaces 紀錄（種類） (docs/board-v2.md section 18.6).
            match activity_because(d) {
                Some(because) => format!("{} {kind}（依據 {because}）{}", seq(), s("excerpt")),
                None => format!("{} 紀錄（{kind}）{}", seq(), s("excerpt")),
            }
        }
        "card.block" => format!("{} 阻擋：{}", seq(), s("reason")),
        "card.unblock" => format!("{} 解除阻擋", seq()),
        "card.done" => match activity_because(d) {
            Some(because) => format!("{} 完成（依據 {because}）", seq()),
            None => format!("{} 完成", seq()),
        },
        "card.drop" => match activity_because(d) {
            Some(because) => format!("{} 捨棄（依據 {because}）", seq()),
            None => format!("{} 捨棄", seq()),
        },
        "link.add" => {
            let to = format!("#{}", s("to_seq"));
            match s("kind").parse::<LinkKind>() {
                Ok(LinkKind::Spawned) => format!("{to} 由 {} 延伸", seq()),
                Ok(LinkKind::Blocks) => format!("{} 阻擋 {to}", seq()),
                Ok(LinkKind::Relates) => format!("{} 與 {to} 相關", seq()),
                Ok(LinkKind::Continues) => format!("{} 接續 {to}", seq()),
                Ok(LinkKind::Ref) => format!("{} 參照 {}", seq(), s("target")),
                Err(_) => format!("{} 新增連結", seq()),
            }
        }
        "link.remove" => format!("{} 移除連結", seq()),
        // docs/hooks.md section 7.2.
        "session.start" => paren("session 開始", &s("source")),
        "session.end" => paren("session 結束", &s("reason")),
        // The turn `kb board start` starts (docs/board-v2.md section 12.4).
        "user.prompt" if d.get("source").and_then(|v| v.as_str()) == Some("board.start") => {
            "看板開始，這一輪起檢查守則".to_string()
        }
        "user.prompt" => format!("使用者：{}", s("excerpt")),
        // Recorded after the turn's one block (docs/board-v2.md section 16.1).
        "check.warn" if d.get("after_block").and_then(|v| v.as_bool()) == Some(true) => {
            format!(
                "守則 {} 未通過（本輪已阻擋過一次，只記錄）：{}",
                s("rule"),
                s("message")
            )
        }
        "check.warn" => format!("守則 {} 警告：{}", s("rule"), s("message")),
        "check.block" => format!("守則 {} 阻擋：{}", s("rule"), s("message")),
        // The model judge (docs/tui.md section 12.1). `message` carries the card number.
        "judge.finding" => {
            let p = d
                .get("p")
                .and_then(|v| v.as_f64())
                .map(|p| format!("{p:.2}"))
                .unwrap_or_else(|| "?".to_string());
            let message = match s("message") {
                m if m.trim().is_empty() => s("question"),
                m => m,
            };
            if d.get("woke").and_then(|v| v.as_bool()) == Some(true) {
                format!("評測：{message}（{p}，已喚醒 agent）")
            } else {
                format!("評測：{message}（{p}）")
            }
        }
        // The feed leaves out runs without an error (read::recent_feed).
        "judge.run" => match d.get("error") {
            Some(_) => format!("評測失敗：{}", s("error")),
            None => format!("評測：{} 題、{} 個發現", num("questions"), num("findings")),
        },
        "turn.summary" => format!(
            "這一輪：{} 張卡片變動、{} 筆紀錄、{} 次工具",
            num("cards_changed"),
            num("notes"),
            num("tools")
        ),
        // docs/questions.md section 4.4.
        "question.ask" => format!("{} 提問：{}", seq(), s("excerpt")),
        "question.answer" => {
            if d.get("chat").and_then(|v| v.as_bool()) == Some(true) {
                format!("{} 回答：在 session 裡說明", seq())
            } else {
                let labels: Vec<String> = d
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).map(one_line).collect())
                    .unwrap_or_default();
                // With typed text (docs/board-v2.md section 7.1): `選項｜文字`, or the text.
                let text = s("text");
                let answer = match (labels.is_empty(), text.trim().is_empty()) {
                    (_, true) => labels.join("、"),
                    (true, false) => text.trim().to_string(),
                    (false, false) => format!("{}｜{}", labels.join("、"), text.trim()),
                };
                format!("{} 回答：{answer}", seq())
            }
        }
        "question.deliver" => format!("{} 回答已送達 agent", seq()),
        "question.cancel" => format!("{} 取消提問", seq()),
        // docs/board-v2.md sections 13.7 and 13.16.
        "comment.add" => format!("{} 留言：{}", seq(), s("excerpt")),
        "comment.deliver" => format!("{} 留言已送達 agent", seq()),
        // docs/board-v2.md section 3.5.
        "epic.add" => format!("新增 epic「{}」", s("epic")),
        "epic.rename" => format!("epic「{}」改名為「{}」", s("from"), s("to")),
        "epic.move" => format!("移動 epic「{}」", s("epic")),
        "epic.remove" => format!("移除 epic「{}」", s("epic")),
        // Old rows of the removed summary feature still render (docs/board-v2.md 10.1).
        "epic.summary" => format!("epic「{}」寫入回顧", s("epic")),
        // docs/board-v2.md section 10.2: 「待辦」的順序：#6、#8、#10
        "card.order" => {
            let seqs: Vec<String> = d
                .get("seqs")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| format!("#{v}")).collect())
                .unwrap_or_default();
            format!("「{}」的順序：{}", s("lane"), seqs.join("、"))
        }
        "card.epic" => match (s("from"), s("to")) {
            (_, to) if !to.is_empty() => format!("{} 歸入 epic「{to}」", seq()),
            (from, _) if !from.is_empty() => format!("{} 移出 epic「{from}」", seq()),
            _ => format!("{} 沒有 epic", seq()),
        },
        "card.origin" => {
            let label = |k: &str| {
                s(k).parse::<Origin>()
                    .map(|o| origin_label(o).to_string())
                    .unwrap_or_else(|_| s(k))
            };
            format!("{} {} → {}", seq(), label("from"), label("to"))
        }
        other => other.to_string(),
    };
    one_line(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_uses_display_width() {
        assert_eq!(truncate("補上逾時測試", 12), "補上逾時測試");
        assert_eq!(truncate("補上逾時測試", 11), "補上逾時測…");
        assert_eq!(truncate("補上逾時測試", 10), "補上逾時…");
        assert_eq!(width(&truncate("補上逾時測試", 10)), 9);
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(fit("修正 token", 8), "修正 to…");
        assert_eq!(width(&fit("修正", 7)), 7);
    }

    #[test]
    fn wrap_breaks_cjk_and_keeps_words() {
        let lines = wrap("排程器每 60 秒執行一次", 10);
        assert!(lines.iter().all(|l| width(l) <= 10), "{lines:?}");
        assert_eq!(lines.concat().replace(' ', ""), "排程器每60秒執行一次");
        assert_eq!(wrap("hello world", 7), vec!["hello", "world"]);
        assert_eq!(wrap("a\n\nb", 5), vec!["a", "", "b"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn card_order_sentence() {
        let a = Activity {
            id: 1,
            board_id: 1,
            card_id: None,
            at: Utc::now(),
            actor: "agent".into(),
            verb: "card.order".into(),
            detail: serde_json::json!({"lane": "待辦", "seqs": [6, 8, 10]}),
            turn_key: None,
        };
        assert_eq!(activity_text(&a), "「待辦」的順序：#6、#8、#10");
    }

    /// docs/board-v2.md section 20.2: a changed goal shows in the feed.
    #[test]
    fn board_update_shows_a_changed_goal() {
        let act = |detail: serde_json::Value| Activity {
            id: 1,
            board_id: 1,
            card_id: None,
            at: Utc::now(),
            actor: "agent".into(),
            verb: "board.update".into(),
            detail,
            turn_key: None,
        };
        use serde_json::json;
        assert_eq!(
            activity_text(&act(json!({"title": "看板", "goal": "找到逾時的\n原因"}))),
            "看板目標更新：找到逾時的 原因"
        );
        assert_eq!(
            activity_text(&act(json!({"title": "看板", "goal": ""}))),
            "看板目標清除"
        );
        assert_eq!(activity_text(&act(json!({"title": "看板"}))), "更新看板");
    }

    #[test]
    fn closing_marks_do_not_start_a_line() {
        assert_eq!(
            wrap("#8 要不要讓「未歸類」也能被選取？", 20),
            vec!["#8 要不要讓「未歸", "類」也能被選取？"]
        );
        // An ASCII word moves down with the mark.
        assert_eq!(
            wrap("看板模型加入 epic、來源", 17),
            vec!["看板模型加入", "epic、來源"]
        );
        // Nothing to carry: the mark starts the line rather than emptying one.
        assert_eq!(wrap("一」", 2), vec!["一", "」"]);
    }

    #[test]
    fn activity_sentences() {
        let act = |verb: &str, detail: serde_json::Value| Activity {
            id: 1,
            board_id: 1,
            card_id: None,
            at: Utc::now(),
            actor: "agent".into(),
            verb: verb.into(),
            detail,
            turn_key: None,
        };
        use serde_json::json;
        assert_eq!(
            activity_text(&act(
                "card.note",
                json!({"seq": 5, "kind": "decision", "excerpt": "改用滑動視窗"})
            )),
            "#5 紀錄（決定）改用滑動視窗"
        );
        assert_eq!(
            activity_text(&act(
                "card.move",
                json!({"seq": 5, "from": "待辦", "to": "實作"})
            )),
            "#5 移到「實作」"
        );
        assert_eq!(
            activity_text(&act(
                "link.add",
                json!({"seq": 5, "kind": "spawned", "to_seq": 8})
            )),
            "#8 由 #5 延伸"
        );
        assert_eq!(
            activity_text(&act("board.archive", json!({"outcome": "completed"}))),
            "封存看板（完成）"
        );
        assert_eq!(
            activity_text(&act(
                "card.edit",
                json!({"seq": 2, "fields": ["title", "priority"]})
            )),
            "#2 修改標題、優先度"
        );
        assert_eq!(
            activity_text(&act(
                "board.terminal",
                json!({"terminal": "t2b", "space": "spc1"})
            )),
            "記錄終端機 t2b"
        );
        assert_eq!(
            activity_text(&act(
                "board.terminal",
                json!({"terminal": "t2b", "space": "spc1", "workspace": "Acme_DevOps"})
            )),
            "記錄終端機 t2b，workspace「Acme_DevOps」"
        );
    }
}
