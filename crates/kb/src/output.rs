//! Text and JSON rendering of command results. Labels are English; lane names, titles
//! and notes are printed as-is (UTF-8).

use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use kb_core::db::{DbStatus, MigrateReport};
use kb_core::model::{
    Activity, Board, Cause, CauseKind, DecisionKind, Lane, Origin, QuestionStatus, format_priority,
};
use kb_core::read::{
    ActiveFinding, BoardSnapshot, BoardSummary, CardDetail, CardSummary, CheckRecord, CommentView,
    Consequence, EpicSummary, LinkDirection, QuestionView, WorkspaceSummary,
};
use kb_core::rules::RuleResult;
use kb_core::write::Inbox;
use serde::Serialize;

pub fn json<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// The first line of `kb board terminal` without `--id` (docs/board-v2.md section 26.4):
/// the board's terminal, with its space and tab when they are set.
pub fn terminal_line(
    board_id: i64,
    terminal: Option<&str>,
    space: Option<&str>,
    tab: Option<&str>,
) -> String {
    let Some(terminal) = terminal else {
        return format!("terminal of board {board_id}: none");
    };
    let parts: Vec<String> = [("space", space), ("tab", tab)]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| format!("{k} {v}")))
        .collect();
    if parts.is_empty() {
        format!("terminal of board {board_id}: {terminal}")
    } else {
        format!(
            "terminal of board {board_id}: {terminal} ({})",
            parts.join(", ")
        )
    }
}

/// The UI contract version (docs/ui-contract.md section 2). Removing or renaming a command
/// or field listed there adds 1; adding does not.
pub const UI_CONTRACT: u32 = 1;

/// `kb version`: one line, or `{"version", "ui_contract"}` (docs/cli.md section 4.21).
pub fn version(json_out: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    if json_out {
        json(&serde_json::json!({ "version": version, "ui_contract": UI_CONTRACT }))
    } else {
        format!("kb {version} ui_contract {UI_CONTRACT}\n")
    }
}

/// `kb board show --json --findings`: the snapshot plus `findings`, the model judge's active
/// findings with the TUI's tag label and explanation (docs/ui-contract.md section 5).
pub fn board_with_findings(snap: &BoardSnapshot, findings: &[ActiveFinding]) -> serde_json::Value {
    let mut value = serde_json::to_value(snap).unwrap_or_default();
    let list: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            let seq = f
                .card_id
                .and_then(|id| {
                    snap.lanes
                        .iter()
                        .flat_map(|l| &l.cards)
                        .find(|c| c.card.id == id)
                })
                .map(|c| c.card.seq);
            serde_json::json!({
                "id": f.id,
                "card_id": f.card_id,
                "card_seq": seq,
                "question": f.tag.question(),
                "p": f.p,
                "label": kb_tui::layout::judge_label(f.tag),
                "text": kb_tui::layout::judge_explanation(f.tag, f.p),
            })
        })
        .collect();
    if let Some(map) = value.as_object_mut() {
        map.insert("findings".into(), list.into());
    }
    value
}

/// Compact JSON with every non-ASCII character written as `\uXXXX` (UTF-16, so characters
/// outside the BMP become surrogate pairs). Used for all `kb hook` output (docs/hooks.md
/// section 2.2). Non-ASCII characters only occur inside JSON strings, where the escape is
/// valid.
pub fn json_ascii(value: &serde_json::Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

fn time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

/// Continuation lines of multi-line text, indented under the first line.
fn indent_rest(text: &str, indent: &str) -> String {
    text.lines()
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}

pub fn migrate(r: &MigrateReport) -> String {
    let mut s = String::new();
    if r.created_database {
        let _ = writeln!(s, "database {} created", r.database);
    }
    if r.applied.is_empty() {
        let _ = write!(
            s,
            "database {} is up to date (schema version {})",
            r.database, r.version
        );
    } else {
        let applied: Vec<String> = r.applied.iter().map(|v| v.to_string()).collect();
        let _ = write!(
            s,
            "database {}: applied migrations {} (schema version {})",
            r.database,
            applied.join(", "),
            r.version
        );
    }
    s
}

pub fn db_status(st: &DbStatus) -> String {
    let version = match st.schema_version {
        Some(v) if v == st.latest_version => format!("{v} (latest)"),
        Some(v) => format!(
            "{v} (latest is {}: run \"kb db migrate\")",
            st.latest_version
        ),
        None => "none (run \"kb db migrate\")".to_string(),
    };
    format!(
        "connection: ok ({}@{}:{}/{})\nschema version: {version}\nworkspaces: {}\nactive boards: {}",
        st.user, st.host, st.port, st.database, st.workspaces, st.active_boards
    )
}

pub fn workspaces(list: &[WorkspaceSummary]) -> String {
    if list.is_empty() {
        return "no workspaces".to_string();
    }
    list.iter()
        .map(|w| {
            format!(
                "{}  [{}]  active {}  archived {}  {}",
                w.name, w.source, w.active_boards, w.archived_boards, w.key
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn boards(list: &[BoardSummary]) -> String {
    if list.is_empty() {
        return "no boards".to_string();
    }
    list.iter()
        .map(|b| {
            format!(
                "{}  [{}]  todo {}  doing {}  done {}  dropped {}  updated {}  {} / {}",
                b.session_key,
                b.status,
                b.cards.todo,
                b.cards.doing,
                b.cards.done,
                b.cards.dropped,
                time(b.updated_at),
                b.workspace_name,
                b.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `kb board start` without a board and with predecessor candidates (docs/board-v2.md
/// section 12.4).
pub fn start_candidates(session_key: &str, candidates: &[Board]) -> String {
    let mut s = format!(
        "no board for session {session_key}. Recent boards that may belong to this conversation:\n"
    );
    for b in candidates {
        let title = if b.title.is_empty() {
            "(untitled)"
        } else {
            b.title.as_str()
        };
        let place = match (&b.terminal_id, &b.cwd) {
            (Some(t), _) => format!(", terminal {t}"),
            (None, Some(cwd)) => format!(", folder {cwd}"),
            (None, None) => String::new(),
        };
        let _ = writeln!(
            s,
            "  board {} [{}] {title}  (changed {}{place})",
            b.id,
            b.status,
            time(b.updated_at)
        );
        let _ = writeln!(s, "    kb board adopt {}", b.id);
    }
    s.push_str("To start a new board instead: kb board start --new");
    s
}

/// The `kb board show` format of docs/cli.md section 4.4. Without `all`, lanes of kind
/// done and dropped print only their header and card count (docs/board-v2.md section
/// 15.1), e.g. `== 完成 (done) == 28 cards`.
pub fn board(snap: &BoardSnapshot, all: bool) -> String {
    let b = &snap.board;
    let mut s = String::new();
    let _ = writeln!(s, "board: {} [{}] rev {}", b.title, b.status, b.rev);
    let _ = writeln!(s, "workspace: {}", snap.workspace.name);
    if !b.goal.is_empty() {
        let _ = writeln!(s, "goal: {}", indent_rest(&b.goal, "      "));
    }
    for e in &snap.epics {
        let c = &e.counts;
        let _ = writeln!(
            s,
            "epic: {}  {}/{}  (+{} derived, {} discussing)",
            e.epic.name, c.done, c.total, c.derived, c.discussing
        );
    }
    if let Some(outcome) = b.outcome {
        let _ = writeln!(s, "outcome: {outcome}");
    }
    if let Some(summary) = &b.summary {
        let _ = writeln!(s, "summary: {}", indent_rest(summary, "         "));
    }
    for lane in &snap.lanes {
        let l = &lane.lane;
        let wip = l
            .wip_limit
            .map(|w| format!(", wip {}/{w}", lane.cards.len()))
            .unwrap_or_default();
        let _ = write!(s, "\n== {} ({}{wip}) ==", l.name, l.kind);
        if !all && l.kind.is_closed() {
            let n = lane.cards.len();
            let _ = write!(s, " {n} {}", if n == 1 { "card" } else { "cards" });
            continue;
        }
        for c in &lane.cards {
            let card = &c.card;
            // No priority here: it does not order the lane any more (docs/board-v2.md 10.2).
            let _ = write!(s, "\n  [{}] {}", card.seq, card.title);
            if let Some(epic) = &c.epic_name {
                let _ = write!(s, " {{{epic}}}");
            }
            if card.origin != Origin::Agreed {
                let _ = write!(s, " ({})", card.origin);
            }
            if let Some(reason) = &card.blocked_reason {
                let _ = write!(s, "  (blocked: {reason})");
            }
            if c.open_question_count > 0 {
                let _ = write!(s, "  (questions: {} open)", c.open_question_count);
            }
        }
    }
    s
}

/// The `kb card show` format: fields, notes, links (docs/cli.md section 4.17).
pub fn card(d: &CardDetail) -> String {
    let c = &d.card;
    let mut s = String::new();
    let _ = writeln!(s, "card {}: {}", c.seq, c.title);
    let _ = writeln!(s, "lane: {} ({})", d.lane.name, d.lane.kind);
    if let Some(epic) = &d.epic_name {
        let _ = writeln!(s, "epic: {epic}");
    }
    let _ = writeln!(s, "origin: {}", c.origin);
    let _ = writeln!(s, "priority: {}", format_priority(c.priority));
    let _ = writeln!(s, "owner: {}", c.owner);
    if let Some(reason) = &c.blocked_reason {
        let _ = writeln!(s, "blocked: {reason}");
    }
    if let Some(result) = &c.result {
        let _ = writeln!(s, "result: {}", indent_rest(result, "        "));
        for cause in &d.result_because {
            let _ = writeln!(s, "        {}", because_line(cause));
        }
    }
    let _ = writeln!(s, "created: {}", time(c.created_at));
    let _ = writeln!(s, "updated: {}", time(c.updated_at));
    if let Some(closed) = c.closed_at {
        let _ = writeln!(s, "closed: {}", time(closed));
    }
    if c.body.is_empty() {
        let _ = writeln!(s, "\nbody: (empty)");
    } else {
        let _ = writeln!(s, "\nbody:\n  {}", indent_rest(&c.body, "  "));
    }
    if !d.questions.is_empty() {
        let _ = write!(s, "\nquestions ({}):", d.questions.len());
        for q in &d.questions {
            let _ = write!(s, "\n{}", indent_lines(&question_block(q), "  "));
        }
        s.push('\n');
    }
    let _ = write!(s, "\nnotes ({}):", d.notes.len());
    for n in &d.notes {
        let _ = write!(
            s,
            "\n  {}  {:<8}  {}",
            time(n.created_at),
            n.kind.as_str(),
            indent_rest(&n.body, &" ".repeat(30))
        );
        for cause in &n.because {
            let _ = write!(s, "\n{}{}", " ".repeat(30), because_line(cause));
        }
    }
    let _ = write!(s, "\n\nlinks ({}):", d.links.len());
    for l in &d.links {
        let direction = match l.direction {
            LinkDirection::Outgoing => "out",
            LinkDirection::Incoming => "in ",
        };
        let target = match (&l.card, &l.target) {
            (Some(card), _) => format!("#{} {}", card.seq, card.title),
            (None, Some(t)) => t.clone(),
            (None, None) => "?".to_string(),
        };
        let _ = write!(
            s,
            "\n  [{}] {direction} {:<9} {target}",
            l.id,
            l.kind.as_str()
        );
        if let Some(label) = &l.label {
            let _ = write!(s, "  ({label})");
        }
    }
    if !d.led_to.is_empty() {
        let _ = write!(s, "\n\nled to ({}):", d.led_to.len());
        for c in &d.led_to {
            let _ = write!(s, "\n  {}", led_to_line(c));
        }
    }
    s
}

/// One cause under a note or a result (docs/board-v2.md section 18.4), e.g.
/// `because: #30 "統一提問畫面"` or `because: question 5 (card #24)`.
pub fn because_line(c: &Cause) -> String {
    let seq = c.card_seq.map(|n| n.to_string()).unwrap_or_default();
    let id = c.id.unwrap_or(0);
    let what = match c.kind {
        CauseKind::Card => {
            let title = c.card_title.as_deref().unwrap_or("");
            format!("#{seq} \"{}\"", title.lines().next().unwrap_or(""))
        }
        CauseKind::Question => format!("question {id} (card #{seq})"),
        CauseKind::Comment => format!("comment {id} (card #{seq})"),
        CauseKind::User => "the user's instruction".to_string(),
    };
    format!("because: {what}")
}

/// One entry of `led to (N):`: `#31 dropped — <first line>`, with `  (via question 5)`
/// when the decision cites a question or comment of this card.
pub fn led_to_line(c: &Consequence) -> String {
    let decision = match c.decision {
        DecisionKind::Drop => "dropped",
        DecisionKind::Done => "done",
        DecisionKind::Note => c.note_kind.map_or("note", |k| k.as_str()),
    };
    let mut s = format!("#{} {decision}", c.card_seq);
    let first = c.text.lines().map(str::trim).find(|l| !l.is_empty());
    if let Some(first) = first {
        let _ = write!(s, " — {first}");
    }
    match (c.via, c.via_id) {
        (CauseKind::Question, Some(id)) => {
            let _ = write!(s, "  (via question {id})");
        }
        (CauseKind::Comment, Some(id)) => {
            let _ = write!(s, "  (via comment {id})");
        }
        _ => {}
    }
    s
}

/// `kb epic list`: name, done/total, derived, discussing.
pub fn epics(list: &[EpicSummary]) -> String {
    if list.is_empty() {
        return "no epics".to_string();
    }
    list.iter()
        .map(|e| {
            let c = &e.counts;
            format!(
                "{}  {}/{}  +{} derived  {} discussing",
                e.epic.name, c.done, c.total, c.derived, c.discussing
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `next:` line when the todo lanes have no card at all (docs/board-v2.md section 10.2).
pub const NEXT_NO_CARD: &str = "next: (no card in the todo lanes)";
/// The `next:` line when every todo card is blocked or under discussion.
pub const NEXT_NONE_READY: &str =
    "next: (no card ready in the todo lanes: all are blocked or under discussion)";

/// The last line of every write command (docs/board-v2.md section 10.2):
/// `next: #<n> <title> (<lane>)` for the head of the queue, else [`NEXT_NONE_READY`] when
/// the todo lanes have cards, else [`NEXT_NO_CARD`].
pub fn next_line(next: Option<(&Lane, &CardSummary)>, todo_cards: usize) -> String {
    match next {
        None if todo_cards > 0 => NEXT_NONE_READY.to_string(),
        None => NEXT_NO_CARD.to_string(),
        Some((lane, c)) => {
            let title = c.card.title.lines().next().unwrap_or("");
            format!("next: #{} {title} ({})", c.card.seq, lane.name)
        }
    }
}

pub fn activity(items: &[Activity]) -> String {
    if items.is_empty() {
        return "no activity".to_string();
    }
    items
        .iter()
        .map(|a| {
            let detail = activity_detail(&a.verb, &a.detail)
                .unwrap_or_else(|| a.detail.to_string());
            format!("{}  {:<13} {detail}", time(a.at), a.verb)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A readable detail for the model judge's rows (docs/hooks.md section 7.2); `None` keeps
/// the raw JSON.
fn activity_detail(verb: &str, d: &serde_json::Value) -> Option<String> {
    let num = |k: &str| d.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
    match verb {
        "judge.finding" => {
            let woke = if d["woke"].as_bool() == Some(true) {
                ", woke the agent"
            } else {
                ""
            };
            Some(format!(
                "{} p={:.2} ({}{woke}): {}",
                d["question"].as_str()?,
                d["p"].as_f64()?,
                d["band"].as_str().unwrap_or("record"),
                d["text"].as_str().unwrap_or("")
            ))
        }
        "judge.run" => {
            let plural = |n: i64, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
            let mut s = format!(
                "{}, {}, {} ms, {}",
                plural(num("questions"), "question"),
                plural(num("findings"), "finding"),
                num("ms"),
                d["model"].as_str().unwrap_or("")
            );
            if num("left_out") > 0 {
                let _ = write!(s, ", {} left out", num("left_out"));
            }
            if let Some(e) = d["error"].as_str() {
                let _ = write!(s, ", error: {e}");
            }
            Some(s)
        }
        _ => None,
    }
}

fn indent_lines(text: &str, indent: &str) -> String {
    text.lines()
        .map(|l| format!("{indent}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What `kb inbox` prints as the answer of a delivered question.
const CHAT_ANSWER: &str = "(the user will reply in this session)";

/// The `answer:` value; `None` when the human only typed text (docs/board-v2.md 7.1).
fn answer_text(q: &QuestionView) -> Option<String> {
    match q.question.status {
        QuestionStatus::Chat => Some(CHAT_ANSWER.to_string()),
        QuestionStatus::Answered => {
            let labels = q.selected_labels();
            (!labels.is_empty() || typed_text(q).is_none()).then(|| labels.join(", "))
        }
        QuestionStatus::Open => Some("(waiting)".to_string()),
        QuestionStatus::Cancelled => Some("(cancelled)".to_string()),
    }
}

/// The text the human typed with an answer (never with 「在 session 裡說明」).
fn typed_text(q: &QuestionView) -> Option<&str> {
    if q.question.status != QuestionStatus::Answered {
        return None;
    }
    q.question
        .answer_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// A question with its options: `[x]` marks selected options.
fn question_block(q: &QuestionView) -> String {
    let mut s = format!(
        "question {} [{}]{} {}",
        q.question.id,
        q.question.status,
        if q.question.multi { " (multi)" } else { "" },
        indent_rest(&q.question.text, "  ")
    );
    for o in &q.options {
        let mark = if o.selected { "[x]" } else { "[ ]" };
        let _ = write!(s, "\n  {mark} {}. {}", o.position, o.label);
        if let Some(d) = &o.description {
            let _ = write!(s, " - {d}");
        }
    }
    if q.question.status == QuestionStatus::Chat {
        let _ = write!(s, "\n  answer: {CHAT_ANSWER}");
    }
    if let Some(t) = typed_text(q) {
        let _ = write!(s, "\n  text: {}", indent_rest(t, "        "));
    }
    s
}

/// How the agent answers a comment (docs/board-v2.md section 13.9).
pub const REPLY_HINT: &str = "kb card note <n> --kind reply \"<text>\"";

/// `comment <id> on card <n> "<title>": <text>` (docs/board-v2.md section 13.8).
pub fn comment_line(c: &CommentView) -> String {
    let title = c.card_title.lines().next().unwrap_or("");
    format!(
        "comment {} on card {} \"{title}\": {}",
        c.note.id,
        c.card_seq,
        indent_rest(&c.note.body, "  ")
    )
}

/// The reminder about comments without a reply, e.g.
/// `1 comment without a reply: card 12. Reply with: kb card note <n> --kind reply "<text>"`;
/// `None` when every comment has a reply (docs/board-v2.md section 13.9).
pub fn unreplied_line(unreplied: &[CommentView]) -> Option<String> {
    if unreplied.is_empty() {
        return None;
    }
    let n = unreplied.len();
    let noun = if n == 1 { "comment" } else { "comments" };
    Some(format!(
        "{n} {noun} without a reply: {}. Reply with: {REPLY_HINT}",
        unreplied_cards(unreplied)
    ))
}

/// `card 12, card 14`: each card once, by number.
fn unreplied_cards(unreplied: &[CommentView]) -> String {
    let cards: std::collections::BTreeSet<i32> = unreplied.iter().map(|c| c.card_seq).collect();
    cards
        .iter()
        .map(|seq| format!("card {seq}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `kb inbox` format of docs/questions.md section 5.2 and docs/board-v2.md section
/// 13.8 (also used by the hooks): answers, comments, the open question count, and the
/// reminder about comments without a reply.
pub fn inbox_text(
    answers: &[QuestionView],
    comments: &[CommentView],
    open_questions: i64,
    unreplied: &[CommentView],
) -> String {
    let head = (answers.is_empty() && comments.is_empty()).then_some("inbox is empty");
    inbox_body(head, answers, comments, open_questions, unreplied)
}

fn inbox_body(
    head: Option<&str>,
    answers: &[QuestionView],
    comments: &[CommentView],
    open_questions: i64,
    unreplied: &[CommentView],
) -> String {
    let mut s = String::new();
    if let Some(head) = head {
        let _ = writeln!(s, "{head}");
    }
    push_items(&mut s, answers, comments);
    let _ = write!(s, "open questions: {open_questions}");
    if let Some(line) = unreplied_line(unreplied) {
        let _ = write!(s, "\n{line}");
    }
    s
}

/// First line of `kb inbox` when nothing is new but items were delivered in this turn
/// (docs/board-v2.md section 19.2).
pub const ALREADY_DELIVERED: &str = "inbox is empty; already delivered in this turn:";

/// Answers, then comments, one block each, in the `kb inbox` format.
fn push_items(s: &mut String, answers: &[QuestionView], comments: &[CommentView]) {
    for q in answers {
        let _ = writeln!(
            s,
            "question {} on card {}: {}",
            q.question.id,
            q.card_seq,
            indent_rest(&q.question.text, "  ")
        );
        if let Some(a) = answer_text(q) {
            let _ = writeln!(s, "  answer: {a}");
        }
        if let Some(t) = typed_text(q) {
            let _ = writeln!(s, "  text: {}", indent_rest(t, "        "));
        }
    }
    for c in comments {
        let _ = writeln!(s, "{}", comment_line(c));
    }
}

/// `kb inbox`: the new items; when there are none, what was already delivered in this
/// turn under [`ALREADY_DELIVERED`] (docs/board-v2.md section 19.2).
pub fn inbox(i: &Inbox) -> String {
    let earlier = &i.delivered_this_turn;
    if !i.answers.is_empty() || !i.comments.is_empty() || earlier.is_empty() {
        return inbox_text(
            &i.answers,
            &i.comments,
            i.open_questions,
            &i.unreplied_comments,
        );
    }
    inbox_body(
        Some(ALREADY_DELIVERED),
        &earlier.answers,
        &earlier.comments,
        i.open_questions,
        &i.unreplied_comments,
    )
}

/// `kb question list`.
pub fn questions(list: &[QuestionView]) -> String {
    if list.is_empty() {
        return "no questions".to_string();
    }
    list.iter()
        .map(|q| {
            let head: String = q
                .question
                .text
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            format!(
                "[{}] card {:<3} {:<9} {head}",
                q.question.id,
                q.card_seq,
                q.question.status.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `kb question show`.
pub fn question(q: &QuestionView) -> String {
    let mut s = format!("question {} on card {}\n", q.question.id, q.card_seq);
    let _ = writeln!(s, "status: {}", q.question.status);
    let _ = writeln!(
        s,
        "choice: {}",
        if q.question.multi {
            "multiple"
        } else {
            "single"
        }
    );
    let _ = writeln!(s, "text: {}", indent_rest(&q.question.text, "      "));
    let _ = writeln!(s, "options:");
    for o in &q.options {
        let mark = if o.selected { "[x]" } else { "[ ]" };
        let _ = write!(s, "  {mark} {}. {}", o.position, o.label);
        if let Some(d) = &o.description {
            let _ = write!(s, " - {d}");
        }
        s.push('\n');
    }
    let chat = if q.question.status == QuestionStatus::Chat {
        "[x]"
    } else {
        "[ ]"
    };
    let _ = writeln!(s, "  {chat} {CHAT_ANSWER}");
    if let Some(t) = typed_text(q) {
        let _ = writeln!(s, "  text: {}", indent_rest(t, "        "));
    }
    let _ = writeln!(
        s,
        "asked: {} by {}",
        time(q.question.asked_at),
        q.question.asked_by
    );
    let fmt = |t: Option<DateTime<Utc>>| t.map(time).unwrap_or_else(|| "-".to_string());
    let _ = writeln!(s, "answered: {}", fmt(q.question.answered_at));
    let _ = write!(s, "delivered: {}", fmt(q.question.delivered_at));
    s
}

/// `kb board check --history` (docs/board-v2.md section 16.2): one line per recorded rule
/// failure, newest first: local time, turn marker, rule, outcome, text.
pub fn check_history(records: &[CheckRecord]) -> String {
    if records.is_empty() {
        return "no rule failures recorded".to_string();
    }
    records
        .iter()
        .map(|r| {
            let at = r.at.with_timezone(&Local).format("%m-%d %H:%M");
            let turn: String = match r.turn_key.as_deref() {
                Some(k) if !k.is_empty() => k.chars().take(8).collect(),
                _ => "-".to_string(),
            };
            let source = if r.text.trim().is_empty() {
                &r.message
            } else {
                &r.text
            };
            let text = source
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "{at}  turn {turn:<8}  {:<2}  {:<12}  {text}",
                r.rule,
                r.outcome.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `kb board check`: one line per rule, then each violation and its fix, then the
/// informational line about comments without a reply (docs/board-v2.md section 13.9).
pub fn check(results: &[RuleResult], unreplied: &[CommentView]) -> String {
    let mut s = String::new();
    for r in results {
        let status = if !r.checked {
            "skip"
        } else if r.passed {
            "pass"
        } else {
            "FAIL"
        };
        let _ = write!(
            s,
            "{}  {:<5}  {status}  {}",
            r.rule,
            r.level.as_str(),
            r.description
        );
        if let Some(why) = r.skipped {
            let _ = write!(s, "  ({why})");
        }
        s.push('\n');
        for v in &r.violations {
            let _ = writeln!(s, "      {}", v.text);
            if !v.fix.is_empty() {
                let _ = writeln!(s, "          Fix: {}", v.fix);
            }
        }
    }
    if unreplied.is_empty() {
        s.push_str("comments: none without a reply");
    } else {
        let _ = write!(
            s,
            "comments: {} without a reply ({})",
            unreplied.len(),
            unreplied_cards(unreplied)
        );
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs/board-v2.md section 26.4.
    #[test]
    fn terminal_line_lists_only_the_values_that_are_set() {
        assert_eq!(
            terminal_line(42, None, None, None),
            "terminal of board 42: none"
        );
        assert_eq!(
            terminal_line(42, Some("t2b"), None, None),
            "terminal of board 42: t2b"
        );
        assert_eq!(
            terminal_line(42, Some("t2b"), Some("spc1"), Some("tb1")),
            "terminal of board 42: t2b (space spc1, tab tb1)"
        );
        assert_eq!(
            terminal_line(42, Some("t2b"), None, Some("tb1")),
            "terminal of board 42: t2b (tab tb1)"
        );
    }
}
