//! What the judge asks and how it reads the answers (docs/behaviour-checks.md sections 4
//! and 5): the turn description, the questions, the findings and the wake text. Pure
//! functions; the database side is in the parent module.

use std::collections::{BTreeMap, BTreeSet};

use super::config::{JudgeConfig, QuestionConfig};
use super::provider::{ModelAnswer, ModelAnswers, ModelQuestion, QuestionShape};

// ---------------------------------------------------------------------------------------
// Wording (docs/behaviour-checks.md section 4, verbatim)

pub const CONTEXT: &str = "The input describes one turn of a coding session in which an AI agent keeps a kanban board that a human watches (the kb command-line tool). It lists the user message, the tools the agent used, the board commands it ran, and the agent's final message of the turn.";
pub const SUFFIX_TURN: &str = "Is the statement true for this turn?";
pub const SUFFIX_MESSAGE: &str = "Is the statement true for this message?";

pub const ADVICE: &str = "In the agent's final message, the agent recommends or decides something about card #N (\"<title>\") (drop it, keep it, change its scope, reorder it, split it, or states a decision about its work), as opposed to merely mentioning it or reporting its status.";
pub const WAITING: &str = "The agent ends this message by asking the user a question or for a decision that blocks further work (pointing to such a question posted on the board counts). A courtesy offer when nothing is waiting on the answer does not count.";
pub const WAITING_WHICH: &str = "Which card is left unable to proceed until the user makes a choice or supplies information that the agent asks for at the end of its final message, either in the message itself or by pointing to a question it posted on the board? An offer to do more that the agent does not depend on is not such a request.";
pub const WAITING_NONE: &str = "none — no card waits on the user";
pub const WAITING_NONE_DESCRIPTION: &str = "The final message does not end by asking the user for a choice or information that a card depends on; it ends with a report, an answer, or an offer to do more.";
pub const OFFCARD: &str = "Where does the project work of this turn belong? Project work means changing the project's files or running the project's code or tests. Pick the card whose topic the work serves; if the work serves several cards, pick the main one.";
pub const OFFCARD_NONE: &str = "no project work this turn";
pub const OFFCARD_NONE_DESCRIPTION: &str =
    "The agent did not change the project's files or run the project's code or tests in this turn.";
pub const OFFCARD_NOCARD: &str = "project work that no listed card covers";
pub const OFFCARD_NOCARD_DESCRIPTION: &str = "The agent changed the project's files or ran the project's code or tests for something that none of the listed cards is about.";
pub const UNCONFIRMED: &str = "During this turn the agent changed the project's files, or ran the project's code, for the topic of card #N (\"<title>\") before the user had approved that topic. Approval given in this turn's user message, or in an answer or comment delivered to the agent this turn, counts as approval.";

fn statement(text: &str, suffix: &str) -> String {
    format!("{CONTEXT}\n\nStatement: {text}\n\n{suffix}")
}

fn question(text: &str) -> String {
    format!("{CONTEXT}\n\nQuestion: {text}")
}

/// A statement about one card: `#N` and `<title>` filled in.
fn about_card(template: &str, card: &CardFacts) -> String {
    template
        .replacen("#N", &format!("#{}", card.seq), 1)
        .replacen("<title>", &card.title, 1)
}

/// The option key of a card: `#<seq> <title>`, at most 128 characters.
pub fn card_key(seq: i32, title: &str) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = format!("#{seq} {title}");
    key.trim().chars().take(128).collect::<String>().trim().to_string()
}

// ---------------------------------------------------------------------------------------
// The board as the judge sees it

/// One card of the board at the end of the turn.
#[derive(Debug, Clone, PartialEq)]
pub struct CardFacts {
    pub id: i64,
    pub seq: i32,
    pub title: String,
    /// In a `done` or `dropped` lane.
    pub finished: bool,
    /// Origin `discussing` (and not finished), or it was `discussing` when the turn started
    /// (its origin changed in this turn).
    pub discussing: bool,
    /// A blocked reason, or an unfinished card blocking it.
    pub blocked: bool,
    /// Questions with status `open`.
    pub open_questions: i64,
    /// The agent wrote it in this turn (a note, a move, done, drop, ...).
    pub written: bool,
    /// Id of the card's latest activity row; 0 without one.
    pub last_activity: i64,
}

/// Which question of the plan an answer belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Waiting,
    WaitingWhich,
    OffcardWork,
    Unconfirmed(i32),
    Advice(i32),
}

/// The questions of one turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub questions: Vec<ModelQuestion>,
    pub targets: BTreeMap<String, Target>,
    /// Option key -> card number, for the choice questions.
    pub card_keys: BTreeMap<String, i32>,
    /// Questions over `max_questions`, not asked.
    pub left_out: usize,
}

/// Card numbers written as `#<seq>` in a text.
pub fn mentioned_cards(text: &str) -> BTreeSet<i32> {
    let mut out = BTreeSet::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != '#' {
            continue;
        }
        let digits: String = chars[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = digits.parse::<i32>() {
            out.insert(n);
        }
    }
    out
}

/// Builds the questions (docs/behaviour-checks.md section 4; docs/hooks.md section 3.7):
/// `waiting`, `waiting_which` (only with cards), `offcard_work`, one `unconfirmed_work` per
/// card under discussion, one `advice` per card without a write in this turn that is
/// unfinished or mentioned as `#<seq>` in the final message. Over `max_questions`, the
/// per-turn questions and `unconfirmed_work` come first, then `advice` for mentioned
/// cards, then the other cards by most recent activity.
pub fn plan(cards: &[CardFacts], final_message: &str, config: &JudgeConfig) -> Plan {
    let mut cards: Vec<&CardFacts> = cards.iter().collect();
    cards.sort_by_key(|c| c.seq);
    let mut plan = Plan::default();
    for c in &cards {
        plan.card_keys.insert(card_key(c.seq, &c.title), c.seq);
    }
    let card_options = || -> Vec<(String, String)> {
        cards
            .iter()
            .map(|c| (card_key(c.seq, &c.title), String::new()))
            .collect()
    };
    let mut ordered: Vec<(String, Target, QuestionShape)> = Vec::new();
    if config.waiting.enabled {
        ordered.push((
            "waiting".into(),
            Target::Waiting,
            QuestionShape::Statement {
                instructions: statement(WAITING, SUFFIX_TURN),
            },
        ));
        if !cards.is_empty() {
            let mut options = card_options();
            options.push((WAITING_NONE.into(), WAITING_NONE_DESCRIPTION.into()));
            ordered.push((
                "waiting_which".into(),
                Target::WaitingWhich,
                QuestionShape::Choice {
                    instructions: question(WAITING_WHICH),
                    options,
                },
            ));
        }
    }
    if config.offcard_work.enabled {
        let mut options = card_options();
        options.push((OFFCARD_NONE.into(), OFFCARD_NONE_DESCRIPTION.into()));
        options.push((OFFCARD_NOCARD.into(), OFFCARD_NOCARD_DESCRIPTION.into()));
        ordered.push((
            "offcard_work".into(),
            Target::OffcardWork,
            QuestionShape::Choice {
                instructions: question(OFFCARD),
                options,
            },
        ));
    }
    if config.unconfirmed_work.enabled {
        for c in cards.iter().filter(|c| c.discussing) {
            ordered.push((
                format!("unconfirmed_work_{}", c.seq),
                Target::Unconfirmed(c.seq),
                QuestionShape::Statement {
                    instructions: statement(&about_card(UNCONFIRMED, c), SUFFIX_TURN),
                },
            ));
        }
    }
    if config.advice.enabled {
        let mentioned = mentioned_cards(final_message);
        let mut eligible: Vec<&CardFacts> = cards
            .iter()
            .copied()
            .filter(|c| !c.written && (!c.finished || mentioned.contains(&c.seq)))
            .collect();
        // Mentioned cards first (by number), then by most recent activity.
        eligible.sort_by(|a, b| {
            let (ma, mb) = (mentioned.contains(&a.seq), mentioned.contains(&b.seq));
            mb.cmp(&ma).then_with(|| {
                if ma {
                    a.seq.cmp(&b.seq)
                } else {
                    (b.last_activity, b.seq).cmp(&(a.last_activity, a.seq))
                }
            })
        });
        for c in eligible {
            ordered.push((
                format!("advice_{}", c.seq),
                Target::Advice(c.seq),
                QuestionShape::Statement {
                    instructions: statement(&about_card(ADVICE, c), SUFFIX_MESSAGE),
                },
            ));
        }
    }
    plan.left_out = ordered.len().saturating_sub(config.max_questions);
    for (id, target, kind) in ordered.into_iter().take(config.max_questions) {
        plan.targets.insert(id.clone(), target);
        plan.questions.push(ModelQuestion { id, kind });
    }
    plan
}

// ---------------------------------------------------------------------------------------
// Findings (docs/behaviour-checks.md section 5)

/// The question a finding comes from (`detail.question`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Advice,
    Waiting,
    OffcardWork,
    UnconfirmedWork,
}

impl Topic {
    pub fn as_str(self) -> &'static str {
        match self {
            Topic::Advice => "advice",
            Topic::Waiting => "waiting",
            Topic::OffcardWork => "offcard_work",
            Topic::UnconfirmedWork => "unconfirmed_work",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// Recorded for the human only.
    Record,
    /// Also wakes the agent (once per turn).
    Wake,
}

impl Band {
    pub fn as_str(self) -> &'static str {
        match self {
            Band::Record => "record",
            Band::Wake => "wake",
        }
    }
}

/// One finding: a `judge.finding` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub topic: Topic,
    /// The card's id and number; `None` for a board-level finding.
    pub card: Option<(i64, i32)>,
    pub p: f64,
    pub band: Band,
    /// One English line for the agent (without the `[kb] ` prefix).
    pub text: String,
    /// One Traditional Chinese line for the human.
    pub message: String,
}

fn band(p: f64, q: &QuestionConfig) -> Band {
    if p >= q.wake { Band::Wake } else { Band::Record }
}

fn first_line(title: &str) -> &str {
    title.lines().next().unwrap_or("").trim()
}

pub fn advice_text(seq: i32, title: &str) -> String {
    format!(
        "card #{seq} \"{}\": your last message recommends or decides something about it, but \
         nothing was recorded on the card. If so, record it: kb card note {seq} --kind decision \
         \"<text>\"",
        first_line(title)
    )
}

pub const WAITING_BOARD_TEXT: &str = "your last message ends with a question for the user, but the board shows no open question and no blocked card. Put it on the board: kb ask --card <n> \"<question>\" --option ... ; kb card block <n> --reason \"waiting for answer to question <id>\"";

pub fn waiting_card_text(seq: i32, title: &str) -> String {
    format!(
        "card #{seq} \"{}\": your last message ends with a question for the user that this card \
         waits on, but the card has no open question and is not blocked. Put it on the board: \
         kb ask --card {seq} \"<question>\" --option ... ; kb card block {seq} --reason \
         \"waiting for answer to question <id>\"",
        first_line(title)
    )
}

pub const OFFCARD_TEXT: &str = "the project work of this turn belongs to no card on the board. Add a card for it and record its result: kb card add --lane <lane> --title \"<title>\" ; kb card done <n> --result \"<result>\"";

pub fn unconfirmed_text(seq: i32, title: &str) -> String {
    format!(
        "card #{seq} \"{}\": this turn changed or ran the project's code for its topic before \
         the user approved it. Ask the user first; if they did approve it, record that: kb card \
         confirm {seq}",
        first_line(title)
    )
}

/// Turns the answers into findings (docs/hooks.md section 3.7): `waiting` first, then
/// `offcard_work`, `unconfirmed_work` and `advice` by card number.
pub fn findings(
    plan: &Plan,
    answers: &ModelAnswers,
    cards: &[CardFacts],
    config: &JudgeConfig,
) -> Vec<Finding> {
    let card = |seq: i32| cards.iter().find(|c| c.seq == seq);
    let probability = |id: &str| match answers.answers.get(id) {
        Some(ModelAnswer::Probability(p)) => Some(*p),
        _ => None,
    };
    let mut out = Vec::new();

    // waiting + waiting_which
    if plan.targets.contains_key("waiting")
        && let Some(p) = probability("waiting")
        && p >= config.waiting.record
    {
        let chosen = match answers.answers.get("waiting_which") {
            Some(ModelAnswer::Choice { first, .. }) => {
                plan.card_keys.get(first).and_then(|seq| card(*seq))
            }
            _ => None,
        };
        let b = band(p, &config.waiting);
        match chosen {
            Some(c) => {
                if c.open_questions == 0 && !c.blocked {
                    out.push(Finding {
                        topic: Topic::Waiting,
                        card: Some((c.id, c.seq)),
                        p,
                        band: b,
                        text: waiting_card_text(c.seq, &c.title),
                        message: format!(
                            "#{} 最後的回覆在等使用者回答，這張卡片沒有提問，也沒有標為阻擋",
                            c.seq
                        ),
                    });
                }
            }
            None => {
                let board_waits = cards
                    .iter()
                    .any(|c| c.open_questions > 0 || (c.blocked && !c.finished));
                if !board_waits {
                    out.push(Finding {
                        topic: Topic::Waiting,
                        card: None,
                        p,
                        band: b,
                        text: WAITING_BOARD_TEXT.to_string(),
                        message: "最後的回覆在等使用者回答，看板上沒有提問，也沒有卡片標為阻擋"
                            .to_string(),
                    });
                }
            }
        }
    }

    // offcard_work
    if plan.targets.contains_key("offcard_work")
        && let Some(ModelAnswer::Choice { probabilities, .. }) =
            answers.answers.get("offcard_work")
        && let Some(&p) = probabilities.get(OFFCARD_NOCARD)
        && p >= config.offcard_work.record
    {
        out.push(Finding {
            topic: Topic::OffcardWork,
            card: None,
            p,
            band: band(p, &config.offcard_work),
            text: OFFCARD_TEXT.to_string(),
            message: "這一輪的專案工作不屬於任何卡片".to_string(),
        });
    }

    // unconfirmed_work, then advice, by card number.
    let mut per_card: Vec<(i32, Topic, &str)> = plan
        .targets
        .iter()
        .filter_map(|(id, t)| match t {
            Target::Unconfirmed(seq) => Some((*seq, Topic::UnconfirmedWork, id.as_str())),
            Target::Advice(seq) => Some((*seq, Topic::Advice, id.as_str())),
            _ => None,
        })
        .collect();
    per_card.sort_by_key(|(seq, topic, _)| (*topic == Topic::Advice, *seq));
    for (seq, topic, id) in per_card {
        let (Some(p), Some(c)) = (probability(id), card(seq)) else {
            continue;
        };
        let q = if topic == Topic::Advice {
            &config.advice
        } else {
            &config.unconfirmed_work
        };
        if p < q.record {
            continue;
        }
        let (text, message) = if topic == Topic::Advice {
            (
                advice_text(seq, &c.title),
                format!("#{seq} 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄"),
            )
        } else {
            (
                unconfirmed_text(seq, &c.title),
                format!("#{seq} 還在討論，這一輪卻已經動工"),
            )
        };
        out.push(Finding {
            topic,
            card: Some((c.id, seq)),
            p,
            band: band(p, q),
            text,
            message,
        });
    }
    out
}

/// Most finding lines in the wake text.
pub const WAKE_LINES: usize = 5;

/// The text that wakes the agent: one `[kb] ` line per finding, at most [`WAKE_LINES`],
/// then one line with the number left out.
pub fn wake_text(findings: &[&Finding]) -> String {
    let mut lines: Vec<String> = findings
        .iter()
        .take(WAKE_LINES)
        .map(|f| format!("[kb] {}", f.text))
        .collect();
    let more = findings.len().saturating_sub(WAKE_LINES);
    if more > 0 {
        lines.push(format!(
            "[kb] {more} more finding{} of this check: kb activity",
            if more == 1 { "" } else { "s" }
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------------------
// The turn description (`input`)

/// Where the user message of the turn comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum UserMessage {
    Text(String),
    /// A `user.prompt` row without the text (written before the text was stored).
    NotRecorded,
    /// The turn was started by `kb board start` (`/kb`).
    BoardStart,
    /// The agent continued after a background notification (the turn continues after an
    /// earlier check of it).
    Notification,
}

impl UserMessage {
    fn line(&self) -> &str {
        match self {
            UserMessage::Text(t) => t,
            UserMessage::NotRecorded => "(not available: the text of this message was not recorded)",
            UserMessage::BoardStart => "(not available: the turn started with the /kb command)",
            UserMessage::Notification => {
                "(no user message: the agent continued after a background notification)"
            }
        }
    }
}

/// The parts of the turn description, built from the database rows of the turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnText {
    pub user_message: UserMessage,
    /// Answers and comments delivered to the agent in the turn, one line each.
    pub delivered: Vec<String>,
    /// `(tool, target)` of each `tool.use` row, in order.
    pub tools: Vec<(String, String)>,
    /// One line per board command (agent row) of the turn.
    pub board_commands: Vec<String>,
    pub final_message: String,
}

/// Most lines in each list of the description.
pub const MAX_LIST_LINES: usize = 60;
/// Longest final message kept whole; a longer one keeps its start and its end.
pub const MAX_FINAL_MESSAGE: usize = 8000;
const FINAL_MESSAGE_HEAD: usize = 1000;

const FILE_TOOLS: [&str; 5] = ["Edit", "Write", "MultiEdit", "NotebookEdit", "Read"];

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}

/// Tool lines as in the measurements (docs/behaviour-checks.md section 6): file tools with
/// the file name (repeats counted `xN`), shell tools with their command.
pub fn tool_lines(tools: &[(String, String)]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (tool, target) in tools {
        let target = target.trim();
        let line = if FILE_TOOLS.contains(&tool.as_str()) && !target.is_empty() {
            let line = format!("{tool} {}", basename(target));
            let n = counts.entry(line.clone()).or_default();
            *n += 1;
            if *n > 1 {
                continue;
            }
            line
        } else if matches!(tool.as_str(), "Bash" | "PowerShell") && !target.is_empty() {
            format!("{tool}: {}", target.replace('\n', " ⏎ "))
        } else if target.is_empty() {
            tool.clone()
        } else {
            format!("{tool} {target}")
        };
        lines.push(line);
    }
    lines
        .into_iter()
        .map(|l| match counts.get(&l) {
            Some(&n) if n > 1 => format!("{l} x{n}"),
            _ => l,
        })
        .collect()
}

fn list(lines: &[String]) -> String {
    if lines.is_empty() {
        return "- none".to_string();
    }
    let mut out: Vec<String> = lines
        .iter()
        .take(MAX_LIST_LINES)
        .map(|l| format!("- {l}"))
        .collect();
    if lines.len() > MAX_LIST_LINES {
        out.push(format!("- ... and {} more", lines.len() - MAX_LIST_LINES));
    }
    out.join("\n")
}

/// Keeps the end of a long final message (the questions look at how it ends) and its start.
pub fn cap_final_message(text: &str) -> String {
    let n = text.chars().count();
    if n <= MAX_FINAL_MESSAGE {
        return text.to_string();
    }
    let tail = MAX_FINAL_MESSAGE - FINAL_MESSAGE_HEAD;
    let head: String = text.chars().take(FINAL_MESSAGE_HEAD).collect();
    let end: String = text.chars().skip(n - tail).collect();
    format!(
        "{head}\n[... {} characters left out ...]\n{end}",
        n - FINAL_MESSAGE_HEAD - tail
    )
}

/// The `input` of the request, in the layout of the measurements: the user message,
/// delivered answers and comments (only when there are any), tools, board commands, the
/// final message.
pub fn input_text(t: &TurnText) -> String {
    let mut s = format!("User message that started the turn:\n{}\n\n", t.user_message.line());
    if !t.delivered.is_empty() {
        s.push_str("Answers and comments delivered to the agent this turn:\n");
        s.push_str(&list(&t.delivered));
        s.push_str("\n\n");
    }
    s.push_str("Tools the agent used this turn (other than board commands):\n");
    s.push_str(&list(&tool_lines(&t.tools)));
    s.push_str("\n\nBoard commands (kb) the agent ran this turn:\n");
    s.push_str(&list(&t.board_commands));
    s.push_str("\n\nAgent's final message of the turn:\n<message>\n");
    s.push_str(&cap_final_message(&t.final_message));
    s.push_str("\n</message>");
    s
}

/// At most `n` characters on one line.
pub fn one_line(text: &str, n: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let mut s: String = flat.chars().take(n.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// One line for an agent row of the turn: verb, card number, the key detail.
pub fn board_command_line(verb: &str, seq: Option<i32>, detail: &serde_json::Value) -> String {
    let s = |k: &str| detail.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let mut line = verb.to_string();
    if let Some(seq) = seq.or_else(|| detail.get("seq").and_then(|v| v.as_i64()).map(|n| n as i32))
    {
        line.push_str(&format!(" #{seq}"));
    }
    let rest = match verb {
        "card.add" => format!(" \"{}\" in {}", s("title"), s("lane")),
        "card.move" | "card.origin" | "card.epic" | "lane.rename" | "epic.rename" => {
            let v = |k: &str| match detail.get(k) {
                Some(serde_json::Value::String(x)) => x.clone(),
                Some(serde_json::Value::Null) | None => "none".to_string(),
                Some(x) => x.to_string(),
            };
            format!(" {} -> {}", v("from"), v("to"))
        }
        "card.note" => format!(" ({}): {}", s("kind"), s("excerpt")),
        "card.done" => format!(": {}", s("result")),
        "card.drop" | "card.block" => format!(": {}", s("reason")),
        "card.edit" => format!(
            " {}",
            detail
                .get("fields")
                .map(|f| f.to_string())
                .unwrap_or_default()
        ),
        "question.ask" => format!(" q{}: {}", detail["question"], s("excerpt")),
        "question.cancel" => format!(" q{}", detail["question"]),
        "link.add" => match detail.get("to_seq").and_then(|v| v.as_i64()) {
            Some(to) => format!(" {} #{to}", s("kind")),
            None => format!(" {} {}", s("kind"), s("target")),
        },
        "board.create" | "board.update" => format!(" \"{}\"", s("title")),
        "lane.add" => format!(" {} ({})", s("lane"), s("kind")),
        "lane.remove" | "lane.move" => format!(" {}", s("lane")),
        "epic.add" | "epic.remove" | "epic.move" => format!(" {}", s("epic")),
        "card.order" => format!(" {} {}", s("lane"), detail["seqs"]),
        _ => String::new(),
    };
    one_line(&format!("{line}{rest}"), 200)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(seq: i32, title: &str) -> CardFacts {
        CardFacts {
            id: 100 + seq as i64,
            seq,
            title: title.into(),
            finished: false,
            discussing: false,
            blocked: false,
            open_questions: 0,
            written: false,
            last_activity: seq as i64,
        }
    }

    fn ids(p: &Plan) -> Vec<&str> {
        p.questions.iter().map(|q| q.id.as_str()).collect()
    }

    #[test]
    fn wording_matches_the_reference() {
        let c = card(3, "加標準差");
        let p = plan(std::slice::from_ref(&c), "", &JudgeConfig::default());
        let QuestionShape::Statement { instructions } = &p.questions[3].kind else {
            panic!()
        };
        assert_eq!(
            instructions,
            &format!(
                "{CONTEXT}\n\nStatement: In the agent's final message, the agent recommends or \
                 decides something about card #3 (\"加標準差\") (drop it, keep it, change its scope, \
                 reorder it, split it, or states a decision about its work), as opposed to merely \
                 mentioning it or reporting its status.\n\nIs the statement true for this message?"
            )
        );
        let QuestionShape::Statement { instructions } = &p.questions[0].kind else {
            panic!()
        };
        assert!(instructions.ends_with("does not count.\n\nIs the statement true for this turn?"));
        let QuestionShape::Choice {
            instructions,
            options,
        } = &p.questions[1].kind
        else {
            panic!()
        };
        assert_eq!(instructions, &format!("{CONTEXT}\n\nQuestion: {WAITING_WHICH}"));
        assert_eq!(
            options,
            &[
                ("#3 加標準差".to_string(), String::new()),
                (WAITING_NONE.to_string(), WAITING_NONE_DESCRIPTION.to_string())
            ]
        );
        let QuestionShape::Choice { options, .. } = &p.questions[2].kind else {
            panic!()
        };
        assert_eq!(
            options.iter().map(|o| o.0.as_str()).collect::<Vec<_>>(),
            ["#3 加標準差", OFFCARD_NONE, OFFCARD_NOCARD]
        );
    }

    #[test]
    fn wording_is_the_reference_documents() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/behaviour-checks.md");
        // A checkout without the design documents has nothing to compare with.
        let Ok(doc) = std::fs::read_to_string(path) else {
            return;
        };
        let doc = doc.replace("\r\n", "\n");
        for text in [
            CONTEXT,
            SUFFIX_TURN,
            SUFFIX_MESSAGE,
            ADVICE,
            WAITING,
            WAITING_WHICH,
            WAITING_NONE,
            WAITING_NONE_DESCRIPTION,
            OFFCARD,
            OFFCARD_NONE,
            OFFCARD_NONE_DESCRIPTION,
            OFFCARD_NOCARD,
            OFFCARD_NOCARD_DESCRIPTION,
            UNCONFIRMED,
            WAITING_BOARD_TEXT,
        ] {
            assert!(doc.contains(text), "not in docs/behaviour-checks.md: {text}");
        }
        assert!(doc.contains(&format!("`[kb] {}`", advice_text(7, "<title>")).replace('7', "N")));
    }

    #[test]
    fn card_keys_are_cut_to_128_characters() {
        assert_eq!(card_key(5, "  a\nb  "), "#5 a b");
        let long = "字".repeat(200);
        let key = card_key(12, &long);
        assert_eq!(key.chars().count(), 128);
        assert!(key.starts_with("#12 字"));
    }

    #[test]
    fn mentions() {
        assert_eq!(
            mentioned_cards("see #3 and #12, not #, x#7"),
            BTreeSet::from([3, 7, 12])
        );
        assert!(mentioned_cards("card 3").is_empty());
    }

    #[test]
    fn which_cards_get_questions() {
        let mut cards = vec![card(1, "a"), card(2, "b"), card(3, "c"), card(4, "d"), card(5, "e")];
        cards[0].written = true; // written: no advice
        cards[1].finished = true; // finished, not mentioned: no advice
        cards[2].finished = true; // finished, mentioned: advice
        cards[3].discussing = true; // discussing: unconfirmed_work and advice
        let p = plan(&cards, "關於 #3 的建議", &JudgeConfig::default());
        assert_eq!(
            ids(&p),
            [
                "waiting",
                "waiting_which",
                "offcard_work",
                "unconfirmed_work_4",
                "advice_3",
                "advice_5",
                "advice_4"
            ]
        );
        assert_eq!(p.left_out, 0);
        assert_eq!(p.card_keys["#4 d"], 4);

        // No cards: no waiting_which, no per-card questions.
        let p = plan(&[], "x", &JudgeConfig::default());
        assert_eq!(ids(&p), ["waiting", "offcard_work"]);

        // Disabled questions are not asked.
        let mut config = JudgeConfig::default();
        config.advice.enabled = false;
        config.waiting.enabled = false;
        let p = plan(&cards, "", &config);
        assert_eq!(ids(&p), ["offcard_work", "unconfirmed_work_4"]);
    }

    #[test]
    fn the_cap_keeps_the_most_relevant_questions() {
        let mut cards: Vec<CardFacts> = (1..=10).map(|n| card(n, "t")).collect();
        cards[8].discussing = true; // #9
        cards[1].last_activity = 50; // #2 most recent
        cards[6].last_activity = 40; // #7 next
        let config = JudgeConfig {
            max_questions: 7,
            ..JudgeConfig::default()
        };
        let p = plan(&cards, "結論：#6 先不做", &config);
        assert_eq!(
            ids(&p),
            [
                "waiting",
                "waiting_which",
                "offcard_work",
                "unconfirmed_work_9",
                "advice_6",
                "advice_2",
                "advice_7"
            ]
        );
        assert_eq!(p.left_out, 10 - 3);
        assert_eq!(p.targets["advice_6"], Target::Advice(6));
    }

    fn answers(items: &[(&str, ModelAnswer)]) -> ModelAnswers {
        ModelAnswers {
            answers: items
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            ms: 10,
        }
    }

    fn prob(p: f64) -> ModelAnswer {
        ModelAnswer::Probability(p)
    }

    fn choice(first: &str, probs: &[(&str, f64)]) -> ModelAnswer {
        ModelAnswer::Choice {
            first: first.into(),
            probabilities: probs.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn advice_findings_and_bands() {
        let cards = vec![card(1, "a"), card(2, "b"), card(3, "c")];
        let config = JudgeConfig::default();
        let p = plan(&cards, "", &config);
        let a = answers(&[
            ("advice_1", prob(0.29)),
            ("advice_2", prob(0.3)),
            ("advice_3", prob(0.7)),
        ]);
        let f = findings(&p, &a, &cards, &config);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].card, f[0].band), (Some((102, 2)), Band::Record));
        assert_eq!((f[1].card, f[1].band), (Some((103, 3)), Band::Wake));
        assert_eq!(f[1].topic, Topic::Advice);
        assert_eq!(
            f[1].text,
            "card #3 \"c\": your last message recommends or decides something about it, but \
             nothing was recorded on the card. If so, record it: kb card note 3 --kind decision \
             \"<text>\""
        );
        assert_eq!(f[1].message, "#3 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄");
    }

    #[test]
    fn waiting_findings() {
        let mut cards = vec![card(1, "a"), card(2, "b")];
        let mut config = JudgeConfig::default();
        let p = plan(&cards, "", &config);
        let run = |cards: &[CardFacts], config: &JudgeConfig, w: f64, which: &str| {
            findings(
                &p,
                &answers(&[("waiting", prob(w)), ("waiting_which", choice(which, &[]))]),
                cards,
                config,
            )
        };
        // Below the record threshold: nothing.
        assert!(run(&cards, &config, 0.49, "#2 b").is_empty());
        // A card: a finding on it, record only by default (wake 1).
        let f = run(&cards, &config, 0.9, "#2 b");
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].topic, f[0].card, f[0].band), (Topic::Waiting, Some((102, 2)), Band::Record));
        assert!(f[0].text.starts_with("card #2 \"b\": your last message ends with a question"));
        // The card is blocked, or has an open question: no finding.
        cards[1].blocked = true;
        assert!(run(&cards, &config, 0.9, "#2 b").is_empty());
        cards[1].blocked = false;
        cards[1].open_questions = 1;
        assert!(run(&cards, &config, 0.9, "#2 b").is_empty());
        // "none": a board-level finding, unless the board has an open question or a
        // blocked card.
        assert!(run(&cards, &config, 0.9, WAITING_NONE).is_empty());
        cards[1].open_questions = 0;
        let f = run(&cards, &config, 0.9, WAITING_NONE);
        assert_eq!((f[0].card, f[0].text.as_str()), (None, WAITING_BOARD_TEXT));
        cards[0].blocked = true;
        assert!(run(&cards, &config, 0.9, WAITING_NONE).is_empty());
        // A blocked card that is finished does not count.
        cards[0].finished = true;
        assert_eq!(run(&cards, &config, 0.9, WAITING_NONE).len(), 1);
        // With a wake threshold below 1.
        config.waiting.wake = 0.7;
        assert_eq!(run(&cards, &config, 0.7, WAITING_NONE)[0].band, Band::Wake);
        // No cards: board level.
        let p = plan(&[], "", &config);
        let f = findings(&p, &answers(&[("waiting", prob(0.6))]), &[], &config);
        assert_eq!((f.len(), f[0].card, f[0].band), (1, None, Band::Record));
    }

    #[test]
    fn offcard_and_unconfirmed_findings() {
        let mut cards = vec![card(1, "a")];
        cards[0].discussing = true;
        let config = JudgeConfig::default();
        let p = plan(&cards, "", &config);
        let a = answers(&[
            (
                "offcard_work",
                choice(OFFCARD_NOCARD, &[(OFFCARD_NOCARD, 0.82), (OFFCARD_NONE, 0.1)]),
            ),
            ("unconfirmed_work_1", prob(0.5)),
        ]);
        let f = findings(&p, &a, &cards, &config);
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].topic, f[0].card, f[0].p), (Topic::OffcardWork, None, 0.82));
        assert_eq!(f[0].band, Band::Record);
        assert_eq!(f[0].message, "這一輪的專案工作不屬於任何卡片");
        assert_eq!((f[1].topic, f[1].card), (Topic::UnconfirmedWork, Some((101, 1))));
        // The first choice does not matter for offcard_work, only the probability.
        let a = answers(&[(
            "offcard_work",
            choice("#1 a", &[(OFFCARD_NOCARD, 0.49), ("#1 a", 0.51)]),
        )]);
        assert!(findings(&p, &a, &cards, &config).is_empty());
    }

    #[test]
    fn wake_text_has_at_most_five_lines_and_a_count() {
        let f: Vec<Finding> = (1..=7)
            .map(|n| Finding {
                topic: Topic::Advice,
                card: Some((n, n as i32)),
                p: 0.8,
                band: Band::Wake,
                text: advice_text(n as i32, "t"),
                message: String::new(),
            })
            .collect();
        let refs: Vec<&Finding> = f.iter().collect();
        let text = wake_text(&refs);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6);
        assert!(lines[0].starts_with("[kb] card #1 \"t\": your last message"));
        assert_eq!(lines[5], "[kb] 2 more findings of this check: kb activity");
        assert_eq!(wake_text(&refs[..1]).lines().count(), 1);
        assert_eq!(
            wake_text(&refs[..6]).lines().last(),
            Some("[kb] 1 more finding of this check: kb activity")
        );
    }

    #[test]
    fn input_text_layout() {
        let t = TurnText {
            user_message: UserMessage::Text("加一個標準差".into()),
            delivered: vec![],
            tools: vec![
                ("Edit".into(), "D:\\p\\summarize.py".into()),
                ("Edit".into(), "/p/summarize.py".into()),
                ("PowerShell".into(), "python -m unittest\nx".into()),
                ("Write".into(), "/p/SPEC.md".into()),
            ],
            board_commands: vec!["card.note #8 (decision): 改成兩張表".into()],
            final_message: "完成了。".into(),
        };
        assert_eq!(
            input_text(&t),
            "User message that started the turn:\n加一個標準差\n\n\
             Tools the agent used this turn (other than board commands):\n\
             - Edit summarize.py x2\n- PowerShell: python -m unittest ⏎ x\n- Write SPEC.md\n\n\
             Board commands (kb) the agent ran this turn:\n- card.note #8 (decision): 改成兩張表\n\n\
             Agent's final message of the turn:\n<message>\n完成了。\n</message>"
        );
        let t = TurnText {
            user_message: UserMessage::Notification,
            delivered: vec!["comment on card #1: 先查 log".into()],
            tools: vec![],
            board_commands: vec![],
            final_message: "好".into(),
        };
        assert_eq!(
            input_text(&t),
            "User message that started the turn:\n(no user message: the agent continued after a \
             background notification)\n\n\
             Answers and comments delivered to the agent this turn:\n- comment on card #1: 先查 log\n\n\
             Tools the agent used this turn (other than board commands):\n- none\n\n\
             Board commands (kb) the agent ran this turn:\n- none\n\n\
             Agent's final message of the turn:\n<message>\n好\n</message>"
        );
        let many: Vec<String> = (0..65).map(|i| format!("c{i}")).collect();
        let l = list(&many);
        assert_eq!(l.lines().count(), 61);
        assert!(l.ends_with("- ... and 5 more"));
    }

    #[test]
    fn long_final_messages_keep_their_end() {
        let text = format!("{}{}", "a".repeat(9000), "END?");
        let capped = cap_final_message(&text);
        assert!(capped.starts_with(&"a".repeat(1000)));
        assert!(capped.ends_with("aEND?"));
        assert!(capped.contains("\n[... 1004 characters left out ...]\n"));
        assert_eq!(cap_final_message("short"), "short");
    }

    #[test]
    fn board_command_lines() {
        assert_eq!(
            board_command_line(
                "card.note",
                Some(8),
                &json!({"seq": 8, "kind": "decision", "excerpt": "改成\n兩張表"})
            ),
            "card.note #8 (decision): 改成 兩張表"
        );
        assert_eq!(
            board_command_line("card.move", None, &json!({"seq": 2, "from": "待辦", "to": "進行中"})),
            "card.move #2 待辦 -> 進行中"
        );
        assert_eq!(
            board_command_line("card.add", Some(9), &json!({"seq": 9, "title": "t", "lane": "待辦"})),
            "card.add #9 \"t\" in 待辦"
        );
        assert_eq!(
            board_command_line("question.ask", Some(3), &json!({"question": 12, "seq": 3, "excerpt": "要哪個？"})),
            "question.ask #3 q12: 要哪個？"
        );
        assert_eq!(
            board_command_line("lane.add", None, &json!({"lane": "驗證", "kind": "doing"})),
            "lane.add 驗證 (doing)"
        );
        assert_eq!(board_command_line("card.unblock", Some(4), &json!({"seq": 4})), "card.unblock #4");
    }
}
