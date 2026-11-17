//! Screen state and the pure `update` function (docs/architecture.md section 5.2).
//!
//! `update` never touches the terminal or the database. It changes [`App`] and returns
//! the [`Effect`]s (data loads, answering a question) the IO layer should run. Time comes
//! in through events, so the rules of docs/tui.md, docs/board-v2.md and docs/questions.md
//! section 7 are testable without a terminal.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use kb_core::model::{Activity, BoardStatus, LaneKind};
use kb_core::read::{
    ActiveFinding, BoardSnapshot, BoardSummary, CardDetail, CardSummary, JudgeTag, LaneWithCards,
    QuestionView, WorkspaceSummary,
};

use crate::layout::{self, CardJudge, ChoiceRow, QuestionUi, Section};

/// Quiet time after the last notification before reloading (docs/tui.md section 8.3).
pub const DEBOUNCE: Duration = Duration::from_millis(100);
/// How long a changed card stays green (docs/tui.md section 8.5).
pub const HIGHLIGHT: Duration = Duration::from_secs(2);
/// Retry delays after a failed query: 1 s, 2 s, 5 s, then every 5 s (docs/tui.md 8.1).
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// Bottom-line texts (docs/questions.md sections 6.1 and 7).
pub const MSG_NO_QUESTIONS: &str = "沒有待回答的提問";
pub const MSG_MARK_FIRST: &str = "先勾選選項或輸入文字";
pub const MSG_SENDING: &str = "送出中…";
pub const MSG_SENT: &str = "回答已送出";
/// Bottom-line texts of the comment row (docs/board-v2.md section 13.15).
pub const MSG_COMMENT_SENT: &str = "留言已送出";
pub const MSG_TYPE_FIRST: &str = "先輸入文字";
pub const MSG_ARCHIVED_NO_COMMENT: &str = "看板已封存，不能留言";
/// The most characters a comment may have (the database's limit).
pub const COMMENT_LIMIT: usize = kb_core::comment::MAX_COMMENT_CHARS;

/// A key press, already filtered to presses (and repeats) by the IO layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    BackTab,
    CtrlC,
    /// `Ctrl+U`: clears the input row (docs/board-v2.md section 7.1).
    CtrlU,
    Char(char),
    Other,
}

/// Which boards an empty screen waits for (docs/tui.md section 2.1, rule 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Workspaces(Vec<i64>),
}

impl Scope {
    pub fn contains(&self, workspace: i64) -> bool {
        match self {
            Scope::All => true,
            Scope::Workspaces(ids) => ids.contains(&workspace),
        }
    }
}

/// Which board to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardTarget {
    Id(i64),
    /// The most recently changed active board in the scope.
    Latest(Scope),
}

/// The display-only card order of the board view, cycled with `o` (docs/board-v2.md
/// section 16.4). It never writes and is not saved: the TUI starts with [`SortMode::Default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortMode {
    /// The read API's order (docs/board-v2.md section 16.3).
    #[default]
    Default,
    /// Card `updated_at`, newest first, in every lane.
    Updated,
    /// Card number ascending, in every lane.
    Number,
}

impl SortMode {
    /// `預設` → `最後異動` → `編號` → `預設`.
    pub fn next(self) -> SortMode {
        match self {
            SortMode::Default => SortMode::Updated,
            SortMode::Updated => SortMode::Number,
            SortMode::Number => SortMode::Default,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Default => "預設",
            SortMode::Updated => "最後異動",
            SortMode::Number => "編號",
        }
    }

    /// Orders the cards of every lane; lanes keep their order. Outside the default the
    /// rule that blocked cards sink in todo lanes does not apply.
    pub fn apply(self, snap: &mut BoardSnapshot) {
        for l in &mut snap.lanes {
            match self {
                SortMode::Default => kb_core::read::default_card_order(l),
                SortMode::Updated => l.cards.sort_by(|a, b| {
                    (b.card.updated_at, b.card.seq).cmp(&(a.card.updated_at, a.card.seq))
                }),
                SortMode::Number => l.cards.sort_by_key(|c| c.card.seq),
            }
        }
    }
}

/// Everything the board screen shows.
#[derive(Debug, Clone)]
pub struct BoardData {
    pub snapshot: BoardSnapshot,
    /// Newest first, without `tool.use` and without `judge.run` rows that have no error.
    pub activity: Vec<Activity>,
    /// The model judge's active findings, oldest first (docs/tui.md section 12). Kept out
    /// of `snapshot`, which the CLI prints for the agent.
    pub findings: Vec<ActiveFinding>,
}

impl BoardData {
    pub fn id(&self) -> i64 {
        self.snapshot.board.id
    }

    /// The model judge's tags of card `card_id`.
    pub fn judge(&self, card_id: i64) -> CardJudge {
        CardJudge::of(&self.findings, card_id)
    }

    /// The probability of the active board-level `waiting` finding (the header's 等回覆).
    pub fn board_waiting(&self) -> Option<f64> {
        self.findings
            .iter()
            .rev()
            .find(|f| f.card_id.is_none() && f.tag == JudgeTag::Waiting)
            .map(|f| f.p)
    }
}

/// An answer to send (docs/questions.md section 7.3), and where to notify the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRequest {
    pub board_id: i64,
    pub seq: i32,
    pub question_id: i64,
    /// Ids of the marked options, in option order; empty when `chat`.
    pub option_ids: Vec<i64>,
    /// 「在 session 裡說明」.
    pub chat: bool,
    /// Typed text (trimmed); `None` without text or with `chat`.
    pub text: Option<String>,
    /// The board's `herdr_terminal`.
    pub terminal: Option<String>,
}

/// A comment to add (docs/board-v2.md section 13.15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRequest {
    pub board_id: i64,
    /// The card's `id` (what `kb_add_comment` takes).
    pub card_id: i64,
    pub seq: i32,
    /// The text as typed; the database trims it and turns line breaks into spaces.
    pub text: String,
}

/// Work for the IO layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadBoard(BoardTarget),
    LoadCard {
        board_id: i64,
        seq: i32,
    },
    LoadWorkspaces,
    /// Boards of one workspace, or of all workspaces; archived boards included.
    LoadBoards {
        workspace_id: Option<i64>,
    },
    /// Answer a question through `kb_core::answer::answer_question`; with 「在 session
    /// 裡說明」, then focus the agent's pane when herdr is there.
    Answer(AnswerRequest),
    /// Add a comment through `kb_core::comment::add_comment`.
    Comment(CommentRequest),
}

/// Inputs of [`App::update`].
#[derive(Debug, Clone)]
pub enum Event {
    Key(Key),
    /// Pasted text (bracketed paste).
    Paste(String),
    Resize(u16, u16),
    Tick(Instant),
    /// A `kb_events` notification.
    Notify {
        workspace: i64,
        board: i64,
        at: Instant,
    },
    ListenerDown,
    ListenerUp,
    BoardLoaded {
        target: BoardTarget,
        data: Option<BoardData>,
        at: Instant,
    },
    CardLoaded {
        board_id: i64,
        seq: i32,
        detail: Option<CardDetail>,
    },
    WorkspacesLoaded(Vec<WorkspaceSummary>),
    BoardsLoaded {
        workspace_id: Option<i64>,
        boards: Vec<BoardSummary>,
    },
    /// A query failed (the connection is re-established on the next query).
    LoadFailed {
        at: Instant,
    },
    /// The answer was stored (`error: None`) or refused (the text to show).
    Answered {
        question_id: i64,
        error: Option<String>,
    },
    /// After 「在 session 裡說明」: the agent's pane got the keyboard (a bottom-line text).
    Notified(String),
    /// The comment was stored (`error: None`) or refused (the text to show).
    Commented {
        card_id: i64,
        error: Option<String>,
    },
}

/// Choices marked with Space and text typed in the input row, not sent yet. They live
/// only on screen (docs/questions.md section 7.4, docs/board-v2.md section 7.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    pub options: BTreeSet<i64>,
    /// 「在 session 裡說明」.
    pub chat: bool,
    /// The draft of the input row (no line breaks, at most 500 characters).
    pub text: String,
    /// Position of the text cursor, in characters.
    pub text_cursor: usize,
}

/// Byte offset of character `i` of `text`.
fn byte_at(text: &str, i: usize) -> usize {
    text.char_indices().nth(i).map_or(text.len(), |(b, _)| b)
}

/// Inserts `s` into a one-line text at the text cursor: line breaks and tabs become
/// spaces, other control characters are dropped, and nothing beyond `limit` characters is
/// accepted. Shared by the question input rows and the comment row.
fn insert_text(text: &mut String, cursor: &mut usize, s: &str, limit: usize) {
    let room = limit.saturating_sub(text.chars().count());
    let clean: String = s
        .replace("\r\n", " ")
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .take(room)
        .collect();
    *cursor = (*cursor).min(text.chars().count());
    let at = byte_at(text, *cursor);
    text.insert_str(at, &clean);
    *cursor += clean.chars().count();
}

/// The editing keys of a one-line text (docs/board-v2.md section 7.1): characters,
/// Backspace / Delete, ← → Home End, Ctrl+U. Returns false for any other key.
fn edit_text(text: &mut String, cursor: &mut usize, key: Key, limit: usize) -> bool {
    let n = text.chars().count();
    *cursor = (*cursor).min(n);
    match key {
        Key::Char(c) => insert_text(text, cursor, &c.to_string(), limit),
        Key::Backspace => {
            if *cursor > 0 {
                let at = byte_at(text, *cursor - 1);
                text.remove(at);
                *cursor -= 1;
            }
        }
        Key::Delete => {
            if *cursor < n {
                let at = byte_at(text, *cursor);
                text.remove(at);
            }
        }
        Key::Left => *cursor = cursor.saturating_sub(1),
        Key::Right => *cursor = (*cursor + 1).min(n),
        Key::Home => *cursor = 0,
        Key::End => *cursor = n,
        Key::CtrlU => {
            text.clear();
            *cursor = 0;
        }
        _ => return false,
    }
    true
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.options.is_empty() && !self.chat && self.text.is_empty()
    }

    /// What Enter would send: an option, typed text, or 「在 session 裡說明」.
    pub fn submittable(&self) -> bool {
        self.chat || !self.options.is_empty() || !self.text.trim().is_empty()
    }

    /// Inserts `s` at the text cursor: line breaks and tabs become spaces, other control
    /// characters are dropped, and nothing beyond 500 characters is accepted.
    pub fn insert(&mut self, s: &str) {
        insert_text(&mut self.text, &mut self.text_cursor, s, layout::TEXT_LIMIT);
    }

    /// An editing key on the input row; false when `key` is not one.
    fn edit(&mut self, key: Key) -> bool {
        edit_text(
            &mut self.text,
            &mut self.text_cursor,
            key,
            layout::TEXT_LIMIT,
        )
    }
}

/// The open comment row (docs/board-v2.md section 13.15): one line of text for one card.
/// It lives only on screen; `Esc` drops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentInput {
    /// The card's `id` and number.
    pub card_id: i64,
    pub seq: i32,
    /// No line breaks, at most [`COMMENT_LIMIT`] characters.
    pub text: String,
    /// Position of the text cursor, in characters.
    pub cursor: usize,
    /// Sent, waiting for [`Event::Commented`]; keys do nothing meanwhile.
    pub sending: bool,
}

impl CommentInput {
    pub fn new(card_id: i64, seq: i32) -> CommentInput {
        CommentInput {
            card_id,
            seq,
            text: String::new(),
            cursor: 0,
            sending: false,
        }
    }

    /// Inserts `s` at the text cursor (pasted text: line breaks become spaces).
    pub fn insert(&mut self, s: &str) {
        insert_text(&mut self.text, &mut self.cursor, s, COMMENT_LIMIT);
    }
}

/// State of an open card screen. Pushed on the history when following a link.
#[derive(Debug, Clone)]
pub struct CardView {
    pub seq: i32,
    pub detail: Option<CardDetail>,
    pub focus: Section,
    /// Whether the initial focus was chosen (done once, when the card first loads).
    pub focus_set: bool,
    /// Question cursor: (question id, choice index); the last choice of a question is
    /// 「在 session 裡說明」.
    pub q_cursor: Option<(i64, usize)>,
    pub q_scroll: usize,
    /// `↑` on a question's first option scrolled up into the question's text, which does
    /// not fit with that option (docs/questions.md §7.3); the option may be off screen.
    pub q_peek: bool,
    pub body_scroll: usize,
    pub notes_scroll: usize,
    pub link_sel: usize,
    /// Full text of an external link target, shown at the bottom after Enter.
    pub message: Option<String>,
}

impl CardView {
    pub fn new(seq: i32) -> CardView {
        CardView {
            seq,
            detail: None,
            focus: Section::Body,
            focus_set: false,
            q_cursor: None,
            q_scroll: 0,
            q_peek: false,
            body_scroll: 0,
            notes_scroll: 0,
            link_sel: 0,
            message: None,
        }
    }

    fn has_questions(&self) -> bool {
        self.detail
            .as_ref()
            .is_some_and(|d| !d.questions.is_empty())
    }

    fn open_question(&self, id: i64) -> Option<&QuestionView> {
        self.detail
            .as_ref()?
            .questions
            .iter()
            .find(|q| q.question.id == id && q.is_open())
    }
}

/// A row of the side panel: an epic, or 「未歸類」 (cards without an epic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EpicKey {
    Epic(i64),
    NoEpic,
}

impl EpicKey {
    /// Whether `card` belongs to this row.
    pub fn contains(self, card: &CardSummary) -> bool {
        match self {
            EpicKey::Epic(id) => card.card.epic_id == Some(id),
            EpicKey::NoEpic => card.card.epic_id.is_none(),
        }
    }
}

/// State of an open epic screen (docs/board-v2.md section 5.8). Its rows are built from
/// the board snapshot; the selection follows the card id across reloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpicView {
    pub key: EpicKey,
    /// Index of the selected row.
    pub sel: usize,
    /// Id of the selected card, so that the selection follows it.
    pub card_id: Option<i64>,
    /// First visible line of the list, which holds the card rows, then the 已捨棄 title
    /// with its blank rows and the dropped rows ([`layout::epic_row_line`]).
    pub first: usize,
}

impl EpicView {
    pub fn new(key: EpicKey) -> EpicView {
        EpicView {
            key,
            sel: 0,
            card_id: None,
            first: 0,
        }
    }

    /// The selected row in `rows`: the same card when it is still there.
    pub fn index_in(&self, rows: &[&CardSummary]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        let same = self
            .card_id
            .and_then(|id| rows.iter().position(|c| c.card.id == id));
        Some(same.unwrap_or(self.sel).min(rows.len() - 1))
    }

    /// Puts the selection on the same card in `rows` (or the nearest row) and returns it.
    fn resolve(&mut self, rows: &[&CardSummary]) -> Option<usize> {
        if rows.is_empty() {
            self.sel = 0;
            self.card_id = None;
            return None;
        }
        if let Some(i) = self
            .card_id
            .and_then(|id| rows.iter().position(|c| c.card.id == id))
        {
            self.sel = i;
        }
        self.sel = self.sel.min(rows.len() - 1);
        self.card_id = Some(rows[self.sel].card.id);
        Some(self.sel)
    }
}

/// A screen above the board, kept in the shared history.
#[derive(Debug, Clone)]
pub enum Screen {
    Card(Box<CardView>),
    Epic(EpicView),
}

/// The cards of an epic row in two groups (docs/board-v2.md section 5.8): those not in a
/// `dropped` lane, then those in one. Each group in screen order: lanes left to right, then
/// the board order inside each lane.
pub fn epic_card_groups(
    snap: &BoardSnapshot,
    key: EpicKey,
) -> (Vec<&CardSummary>, Vec<&CardSummary>) {
    let group = |dropped: bool| -> Vec<&CardSummary> {
        snap.lanes
            .iter()
            .filter(|l| (l.lane.kind == LaneKind::Dropped) == dropped)
            .flat_map(|l| l.cards.iter())
            .filter(|c| key.contains(c))
            .collect()
    };
    (group(false), group(true))
}

/// All the rows of an epic screen, which share one selection: the cards, then the dropped
/// cards.
pub fn epic_cards(snap: &BoardSnapshot, key: EpicKey) -> Vec<&CardSummary> {
    let (mut cards, dropped) = epic_card_groups(snap, key);
    cards.extend(dropped);
    cards
}

/// The side-panel rows in order: the epics, then 「未歸類」 when some card has no epic.
pub fn side_rows(snap: &BoardSnapshot) -> Vec<EpicKey> {
    let mut rows: Vec<EpicKey> = snap
        .epics
        .iter()
        .map(|e| EpicKey::Epic(e.epic.id))
        .collect();
    if snap.no_epic.is_some() {
        rows.push(EpicKey::NoEpic);
    }
    rows
}

pub const MSG_EPIC_REMOVED: &str = "這個 epic 已經被移除";

/// A list shown in a menu; `items` is `None` while loading.
#[derive(Debug, Clone)]
pub struct Picker<T> {
    pub items: Option<Vec<T>>,
    pub sel: usize,
}

impl<T> Picker<T> {
    fn loading() -> Picker<T> {
        Picker {
            items: None,
            sel: 0,
        }
    }

    fn len(&self) -> usize {
        self.items.as_ref().map_or(0, Vec::len)
    }

    fn step(&mut self, delta: isize) {
        let n = self.len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    fn selected(&self) -> Option<&T> {
        self.items.as_ref().and_then(|v| v.get(self.sel))
    }

    /// Replaces the items, keeping the selection on the item with the same id.
    fn replace(&mut self, items: Vec<T>, id: impl Fn(&T) -> i64, prefer: Option<i64>) {
        let keep = self.selected().map(&id).or(prefer);
        self.sel = keep
            .and_then(|k| items.iter().position(|t| id(t) == k))
            .unwrap_or(0)
            .min(items.len().saturating_sub(1));
        self.items = Some(items);
    }
}

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    Workspaces(Picker<WorkspaceSummary>),
    Boards {
        workspace_id: Option<i64>,
        /// Workspace name for the title; `None` for all workspaces.
        workspace_name: Option<String>,
        picker: Picker<BoardSummary>,
    },
}

/// Layout numbers of the open card screen, for scrolling.
struct CardMetrics {
    shown: &'static [Section],
    natural: Vec<usize>,
    heights: Vec<usize>,
    choices: Vec<ChoiceRow>,
}

impl CardMetrics {
    fn of(&self, s: Section) -> (usize, usize) {
        match self.shown.iter().position(|x| *x == s) {
            Some(i) => (self.natural[i], self.heights[i]),
            None => (0, 1),
        }
    }

    /// The row of the question cursor.
    fn cursor_row(&self, cursor: Option<(i64, usize)>) -> Option<&ChoiceRow> {
        self.choices
            .iter()
            .find(|r| Some((r.question, r.index)) == cursor)
    }
}

/// `↑` / `↓` (`step` -1 / 1) with the cursor on a question's first option, in a question
/// section of `h` rows, when the question's text does not fit with that option
/// (docs/questions.md §7.3): scrolls the view one line up into the text, or back down
/// while the option is not fully shown, without moving the cursor. `peek`: the view was
/// scrolled into the text before this key. Returns whether it scrolled.
fn peek_scroll(
    view: &mut CardView,
    m: Option<&CardMetrics>,
    step: isize,
    h: usize,
    peek: bool,
) -> bool {
    let Some(row) = m.and_then(|m| m.cursor_row(view.q_cursor)) else {
        return false;
    };
    if row.index != 0 || row.fits_with_text(h) {
        return false;
    }
    let h = h.max(1);
    let end = row.first_line + row.lines;
    if step < 0 && view.q_scroll > row.head {
        view.q_scroll -= 1;
        view.q_peek = true;
        true
    } else if step > 0 && peek && end > view.q_scroll + h {
        view.q_scroll += 1;
        view.q_peek = end > view.q_scroll + h;
        true
    } else {
        false
    }
}

/// The whole screen state.
#[derive(Debug, Clone)]
pub struct App {
    pub size: (u16, u16),
    /// The local date, for time formatting (set by the IO layer).
    pub today: NaiveDate,
    /// Boards an empty screen waits for.
    pub scope: Scope,
    /// The board that should be shown; `None` = the latest in `scope`.
    pub want: Option<i64>,
    pub data: Option<BoardData>,
    /// False until the first board load finished (shows 「載入中」 instead of the empty text).
    pub loaded: bool,

    /// Index of the selected lane.
    pub sel_lane: usize,
    /// Last selected card per lane: lane id → card id (docs/tui.md section 4.5).
    pub lane_memory: HashMap<i64, i64>,
    /// First visible lane.
    pub lane_offset: usize,
    /// First visible card per lane id.
    pub card_offsets: HashMap<i64, usize>,
    /// Changed cards: card id → when the green highlight ends.
    pub highlights: HashMap<i64, Instant>,
    /// The side panel is wanted (`s`); it is on screen only from 80 columns.
    pub show_side: bool,
    /// The selected side-panel row (`e` / `E`, or the arrows in the side panel).
    pub sel_epic: Option<EpicKey>,
    /// The side panel has the focus instead of the lanes (docs/board-v2.md section 5.7).
    pub side_focus: bool,
    /// The display-only card order (`o`, docs/board-v2.md section 16.4). Kept when the
    /// board changes; reset only by restarting the TUI.
    pub sort: SortMode,

    /// The open card screen.
    pub card: Option<CardView>,
    /// The open epic screen (only when no card screen is open).
    pub epic: Option<EpicView>,
    /// Screens left by opening a card or following a link; shared by the card and epic
    /// screens (docs/tui.md section 5.4, docs/board-v2.md section 5.8).
    pub history: Vec<Screen>,
    pub overlay: Option<Overlay>,
    /// Marked, unsent choices per question id (docs/questions.md section 7.4).
    pub marks: HashMap<i64, Marks>,
    /// The question being sent.
    pub submitting: Option<i64>,
    /// A one-line message at the bottom, cleared by the next key.
    pub flash: Option<String>,
    /// The open comment row (docs/board-v2.md section 13.15).
    pub comment: Option<CommentInput>,
    /// Section-title rule and table style of the card and epic screens.
    pub look: layout::Look,

    pub listen_ok: bool,
    pub query_ok: bool,
    retry_attempt: usize,
    pub retry_at: Option<Instant>,
    /// Debounce deadline and what to reload when it passes.
    pub pending_at: Option<Instant>,
    pending_board: bool,
    pending_picker: bool,

    pub quit: bool,
}

impl App {
    pub fn new(size: (u16, u16), today: NaiveDate, scope: Scope, want: Option<i64>) -> App {
        App {
            size,
            today,
            scope,
            want,
            data: None,
            loaded: false,
            sel_lane: 0,
            lane_memory: HashMap::new(),
            lane_offset: 0,
            card_offsets: HashMap::new(),
            highlights: HashMap::new(),
            show_side: true,
            sel_epic: None,
            side_focus: false,
            sort: SortMode::Default,
            card: None,
            epic: None,
            history: Vec::new(),
            overlay: None,
            marks: HashMap::new(),
            submitting: None,
            flash: None,
            comment: None,
            look: layout::Look::default(),
            listen_ok: true,
            query_ok: true,
            retry_attempt: 0,
            retry_at: None,
            pending_at: None,
            pending_board: false,
            pending_picker: false,
            quit: false,
        }
    }

    /// `● 已連線` when both the listener and the queries work.
    pub fn connected(&self) -> bool {
        self.listen_ok && self.query_ok
    }

    /// Whether the side panel is on screen.
    pub fn side_shown(&self) -> bool {
        layout::side_shown(self.size, self.show_side)
    }

    /// The load that refreshes what the board screen shows.
    pub fn board_target(&self) -> BoardTarget {
        match (self.want, &self.data) {
            (Some(id), _) => BoardTarget::Id(id),
            (None, Some(d)) => BoardTarget::Id(d.id()),
            (None, None) => BoardTarget::Latest(self.scope.clone()),
        }
    }

    /// Initial load at startup.
    pub fn start(&self) -> Vec<Effect> {
        vec![Effect::LoadBoard(self.board_target())]
    }

    // ----- board selection --------------------------------------------------------

    pub fn lanes(&self) -> &[LaneWithCards] {
        self.data.as_ref().map_or(&[], |d| &d.snapshot.lanes)
    }

    /// Index of the selected card in lane `lane`: the remembered card if it is still in
    /// that lane, else the first card.
    pub fn card_index(&self, lane: usize) -> Option<usize> {
        self.card_index_in(self.lanes().get(lane)?)
    }

    /// [`App::card_index`] for a lane given by reference.
    pub fn card_index_in(&self, l: &LaneWithCards) -> Option<usize> {
        if l.cards.is_empty() {
            return None;
        }
        let remembered = self
            .lane_memory
            .get(&l.lane.id)
            .and_then(|cid| l.cards.iter().position(|c| c.card.id == *cid));
        Some(remembered.unwrap_or(0))
    }

    pub fn selected_card(&self) -> Option<&CardSummary> {
        let i = self.card_index(self.sel_lane)?;
        self.lanes().get(self.sel_lane)?.cards.get(i)
    }

    /// The lanes on screen, with their widths.
    pub fn lane_strip(&self) -> layout::LaneStrip {
        let areas = layout::board_areas(self.size, self.side_shown());
        let kinds: Vec<LaneKind> = self.lanes().iter().map(|l| l.lane.kind).collect();
        layout::lane_strip(areas.lanes, &kinds, self.sel_lane, self.lane_offset)
    }

    /// Scrolls lanes and cards so that the selection is on screen.
    fn keep_selection_visible(&mut self) {
        let n = self.lanes().len();
        if n == 0 {
            self.sel_lane = 0;
            self.lane_offset = 0;
            return;
        }
        self.sel_lane = self.sel_lane.min(n - 1);
        let strip = self.lane_strip();
        self.lane_offset = strip.first;
        let Some(slot) = strip.slots.iter().find(|s| s.index == self.sel_lane) else {
            return;
        };
        let room = layout::board_areas(self.size, self.side_shown())
            .lanes
            .height
            .saturating_sub(2);
        let Some(i) = self.card_index(self.sel_lane) else {
            return;
        };
        let lane = &self.lanes()[self.sel_lane];
        let lane_id = lane.lane.id;
        let findings = self
            .data
            .as_ref()
            .map_or(&[][..], |d| d.findings.as_slice());
        let heights = layout::card_heights(&lane.cards, slot.width, slot.narrow, findings);
        let off = self.card_offsets.get(&lane_id).copied().unwrap_or(0);
        let (first, _) = layout::card_window(&heights, room, Some(i), off);
        self.card_offsets.insert(lane_id, first);
    }

    fn select_card_at(&mut self, idx: usize) {
        let Some(l) = self.lanes().get(self.sel_lane) else {
            return;
        };
        if let Some(c) = l.cards.get(idx) {
            let (lid, cid) = (l.lane.id, c.card.id);
            self.lane_memory.insert(lid, cid);
        }
        self.keep_selection_visible();
    }

    /// Selects the card with id `card_id`, wherever it is.
    fn select_card_id(&mut self, card_id: i64) -> bool {
        let found = self.lanes().iter().enumerate().find_map(|(li, l)| {
            l.cards
                .iter()
                .any(|c| c.card.id == card_id)
                .then_some((li, l.lane.id))
        });
        match found {
            Some((li, lane_id)) => {
                self.sel_lane = li;
                self.lane_memory.insert(lane_id, card_id);
                self.keep_selection_visible();
                true
            }
            None => false,
        }
    }

    fn reset_board_view(&mut self) {
        self.sel_lane = 0;
        self.lane_memory.clear();
        self.lane_offset = 0;
        self.card_offsets.clear();
        self.highlights.clear();
        self.sel_epic = None;
        self.side_focus = false;
    }

    /// The side-panel rows of the shown board.
    pub fn side_rows(&self) -> Vec<EpicKey> {
        self.data
            .as_ref()
            .map(|d| side_rows(&d.snapshot))
            .unwrap_or_default()
    }

    /// Whether the side panel has the focus (it must be on screen).
    pub fn side_focused(&self) -> bool {
        self.side_focus && self.side_shown()
    }

    /// The selected epic's id (none when 「未歸類」 or nothing is selected).
    pub fn sel_epic_id(&self) -> Option<i64> {
        match self.sel_epic {
            Some(EpicKey::Epic(id)) => Some(id),
            _ => None,
        }
    }

    /// Drops a side-panel selection whose row is gone, and the focus of a panel that is
    /// hidden or empty.
    fn check_side(&mut self) {
        let rows = self.side_rows();
        if self.sel_epic.is_some_and(|k| !rows.contains(&k)) {
            self.sel_epic = None;
        }
        if !self.side_shown() || rows.is_empty() {
            self.side_focus = false;
        }
        if self.side_focus && self.sel_epic.is_none() {
            self.sel_epic = rows.first().copied();
        }
    }

    /// `e` (`delta` 1) / `E` (`delta` -1): the next or previous epic; after the last one,
    /// back to no selection (docs/board-v2.md section 5.2). 「未歸類」 is not part of the
    /// cycle; from it, `e` starts again at the first epic.
    fn cycle_epic(&mut self, delta: isize) {
        let ids: Vec<i64> = self
            .data
            .as_ref()
            .map(|d| d.snapshot.epics.iter().map(|e| e.epic.id).collect())
            .unwrap_or_default();
        let n = ids.len();
        if n == 0 {
            self.sel_epic = None;
            return;
        }
        let pos = self
            .sel_epic_id()
            .and_then(|id| ids.iter().position(|x| *x == id));
        let next = match (pos, delta > 0) {
            (None, true) => Some(0),
            (Some(i), true) => (i + 1 < n).then_some(i + 1),
            (None, false) => Some(n - 1),
            (Some(i), false) => i.checked_sub(1),
        };
        self.sel_epic = next.map(|i| EpicKey::Epic(ids[i]));
    }

    /// `o`: the next display order (docs/board-v2.md section 16.4). Writes nothing; the
    /// selection stays on the same card and every lane shows its top again.
    fn cycle_sort(&mut self) {
        let keep = self.selected_card().map(|c| c.card.id);
        self.sort = self.sort.next();
        if let Some(d) = self.data.as_mut() {
            self.sort.apply(&mut d.snapshot);
        }
        self.card_offsets.clear();
        if !keep.is_some_and(|id| self.select_card_id(id)) {
            self.keep_selection_visible();
        }
    }

    // ----- update -----------------------------------------------------------------

    pub fn update(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(k) => self.on_key(k),
            Event::Paste(text) => {
                self.on_paste(&text);
                Vec::new()
            }
            Event::Resize(w, h) => {
                self.size = (w, h);
                self.check_side();
                self.keep_selection_visible();
                self.clamp_card_scroll();
                Vec::new()
            }
            Event::Tick(now) => self.on_tick(now),
            Event::Notify {
                workspace,
                board,
                at,
            } => {
                let shown = self.data.as_ref().map(BoardData::id).or(self.want);
                let relevant = match shown {
                    Some(id) => id == board,
                    None => self.scope.contains(workspace),
                };
                if relevant {
                    self.pending_board = true;
                }
                if matches!(
                    self.overlay,
                    Some(Overlay::Workspaces(_) | Overlay::Boards { .. })
                ) {
                    self.pending_picker = true;
                }
                if self.pending_board || self.pending_picker {
                    self.pending_at = Some(at + DEBOUNCE);
                }
                Vec::new()
            }
            Event::ListenerDown => {
                self.listen_ok = false;
                Vec::new()
            }
            Event::ListenerUp => {
                self.listen_ok = true;
                self.full_reload()
            }
            Event::BoardLoaded { target, data, at } => {
                self.query_succeeded();
                self.on_board_loaded(target, data, at)
            }
            Event::CardLoaded {
                board_id,
                seq,
                detail,
            } => {
                self.query_succeeded();
                self.on_card_loaded(board_id, seq, detail);
                Vec::new()
            }
            Event::WorkspacesLoaded(list) => {
                self.query_succeeded();
                if let Some(Overlay::Workspaces(p)) = &mut self.overlay {
                    p.replace(list, |w| w.id, None);
                }
                Vec::new()
            }
            Event::BoardsLoaded {
                workspace_id,
                boards,
            } => {
                self.query_succeeded();
                let current = self.data.as_ref().map(BoardData::id);
                if let Some(Overlay::Boards {
                    workspace_id: ws,
                    picker,
                    ..
                }) = &mut self.overlay
                    && *ws == workspace_id
                {
                    picker.replace(boards, |b| b.id, current);
                }
                Vec::new()
            }
            Event::LoadFailed { at } => {
                self.query_ok = false;
                let delay = RETRY_DELAYS[self.retry_attempt.min(RETRY_DELAYS.len() - 1)];
                self.retry_attempt += 1;
                self.retry_at = Some(at + delay);
                Vec::new()
            }
            Event::Answered { question_id, error } => {
                if self.submitting == Some(question_id) {
                    self.submitting = None;
                }
                match error {
                    None => {
                        self.marks.remove(&question_id);
                        self.flash = Some(MSG_SENT.to_string());
                    }
                    Some(e) => self.flash = Some(format!("送出失敗：{e}")),
                }
                let mut fx = vec![Effect::LoadBoard(self.board_target())];
                fx.extend(self.card_reload());
                fx
            }
            Event::Notified(message) => {
                self.flash = Some(message);
                Vec::new()
            }
            Event::Commented { card_id, error } => {
                let mine = self
                    .comment
                    .as_ref()
                    .is_some_and(|c| c.card_id == card_id && c.sending);
                match error {
                    None => {
                        if mine {
                            self.comment = None;
                        }
                        self.flash = Some(MSG_COMMENT_SENT.to_string());
                    }
                    // The row stays open with its text, so it can be fixed and sent again.
                    Some(e) => {
                        if mine && let Some(c) = self.comment.as_mut() {
                            c.sending = false;
                        }
                        self.flash = Some(e);
                    }
                }
                self.clamp_card_scroll();
                self.scroll_epic();
                let mut fx = vec![Effect::LoadBoard(self.board_target())];
                fx.extend(self.card_reload());
                fx
            }
        }
    }

    fn query_succeeded(&mut self) {
        self.query_ok = true;
        self.retry_attempt = 0;
        self.retry_at = None;
    }

    /// Reloads everything that is shown (after reconnecting).
    fn full_reload(&mut self) -> Vec<Effect> {
        let mut fx = vec![Effect::LoadBoard(self.board_target())];
        fx.extend(self.card_reload());
        fx.extend(self.picker_reload());
        self.pending_at = None;
        self.pending_board = false;
        self.pending_picker = false;
        fx
    }

    fn card_reload(&self) -> Option<Effect> {
        let card = self.card.as_ref()?;
        let board_id = self.data.as_ref()?.id();
        Some(Effect::LoadCard {
            board_id,
            seq: card.seq,
        })
    }

    fn picker_reload(&self) -> Option<Effect> {
        match &self.overlay {
            Some(Overlay::Workspaces(_)) => Some(Effect::LoadWorkspaces),
            Some(Overlay::Boards { workspace_id, .. }) => Some(Effect::LoadBoards {
                workspace_id: *workspace_id,
            }),
            _ => None,
        }
    }

    fn on_tick(&mut self, now: Instant) -> Vec<Effect> {
        self.highlights.retain(|_, until| *until > now);
        let mut fx = Vec::new();
        if self.pending_at.is_some_and(|t| t <= now) {
            self.pending_at = None;
            if std::mem::take(&mut self.pending_board) {
                fx.push(Effect::LoadBoard(self.board_target()));
                fx.extend(self.card_reload());
            }
            if std::mem::take(&mut self.pending_picker) {
                fx.extend(self.picker_reload());
            }
        }
        if !self.query_ok && self.retry_at.is_some_and(|t| t <= now) {
            self.retry_at = None;
            for e in self.full_reload() {
                if !fx.contains(&e) {
                    fx.push(e);
                }
            }
        }
        fx
    }

    fn on_board_loaded(
        &mut self,
        target: BoardTarget,
        mut data: Option<BoardData>,
        at: Instant,
    ) -> Vec<Effect> {
        // The read API gives the default order; another display order is applied here.
        if self.sort != SortMode::Default
            && let Some(d) = data.as_mut()
        {
            self.sort.apply(&mut d.snapshot);
        }
        // A load for a board that is no longer wanted (the user switched boards).
        if let (BoardTarget::Id(id), Some(want)) = (&target, self.want)
            && *id != want
        {
            return Vec::new();
        }
        self.loaded = true;
        let Some(new) = data else {
            if matches!(target, BoardTarget::Latest(_)) {
                self.data = None;
                self.reset_board_view();
            }
            return Vec::new();
        };
        let same_board = self.data.as_ref().is_some_and(|d| d.id() == new.id());
        self.want = Some(new.id());
        if !same_board {
            self.data = Some(new);
            self.reset_board_view();
            self.card = None;
            self.epic = None;
            self.history.clear();
            self.marks.clear();
            // A comment row belongs to a card of the previous board.
            self.comment = None;
            self.keep_selection_visible();
            return Vec::new();
        }

        // Same board: highlight changed cards, keep the selection on the same card.
        let old = self.data.take().expect("same_board implies data");
        let prev_card = self.selected_card_in(&old).map(|c| c.card.id);
        let prev_lane = old.snapshot.lanes.get(self.sel_lane).map(|l| l.lane.id);
        for c in changed_cards(&old.snapshot, &new.snapshot) {
            self.highlights.insert(c, at + HIGHLIGHT);
        }
        self.data = Some(new);
        self.check_side();
        self.refresh_epic_view();
        let followed = prev_card.is_some_and(|cid| self.select_card_id(cid));
        if !followed {
            if let Some(li) =
                prev_lane.and_then(|lid| self.lanes().iter().position(|l| l.lane.id == lid))
            {
                self.sel_lane = li;
            }
            self.keep_selection_visible();
        }
        Vec::new()
    }

    fn selected_card_in<'a>(&self, data: &'a BoardData) -> Option<&'a CardSummary> {
        let l = data.snapshot.lanes.get(self.sel_lane)?;
        let idx = self
            .lane_memory
            .get(&l.lane.id)
            .and_then(|cid| l.cards.iter().position(|c| c.card.id == *cid))
            .unwrap_or(0);
        l.cards.get(idx)
    }

    fn on_card_loaded(&mut self, board_id: i64, seq: i32, detail: Option<CardDetail>) {
        if self.data.as_ref().map(BoardData::id) != Some(board_id) {
            return;
        }
        let Some(view) = self.card.as_mut() else {
            return;
        };
        if view.seq != seq {
            return;
        }
        let Some(detail) = detail else {
            return;
        };
        // Unsent marks of questions that are no longer open are dropped (questions 7.4).
        for q in &detail.questions {
            if !q.is_open() {
                self.marks.remove(&q.question.id);
            }
        }
        // Keep the link selection on the same row when the rows change (links, causes and
        // the decisions this card led to: docs/board-v2.md section 18.6).
        let rows = layout::link_rows(&detail);
        let prev_link = view
            .detail
            .as_ref()
            .and_then(|d| layout::link_rows(d).into_iter().nth(view.link_sel))
            .map(|r| r.key);
        if let Some(pos) = prev_link.and_then(|key| rows.iter().position(|r| r.key == key)) {
            view.link_sel = pos;
        }
        // The question cursor stays where it was while that question is open.
        view.q_cursor = match view.q_cursor.and_then(|(id, i)| {
            detail
                .questions
                .iter()
                .find(|q| q.question.id == id && q.is_open())
                .map(|q| (id, i.min(layout::input_index(q))))
        }) {
            Some(c) => Some(c),
            None => layout::choices(&detail.questions, &self.marks)
                .first()
                .copied(),
        };
        let has_open = detail.questions.iter().any(QuestionView::is_open);
        if !view.focus_set {
            view.focus = if has_open {
                Section::Questions
            } else if rows.is_empty() {
                Section::Body
            } else {
                Section::Links
            };
            view.focus_set = true;
        }
        if view.focus == Section::Questions && detail.questions.is_empty() {
            view.focus = Section::Body;
        }
        view.detail = Some(detail);
        self.clamp_card_scroll();
    }

    // ----- keys -------------------------------------------------------------------

    fn on_key(&mut self, key: Key) -> Vec<Effect> {
        if key == Key::CtrlC {
            self.quit = true;
            return Vec::new();
        }
        // While a comment is being sent, keys do nothing (the message stays).
        if self.comment.as_ref().is_some_and(|c| c.sending) {
            return Vec::new();
        }
        self.flash = None;
        if self.comment.is_some() {
            return self.on_comment_key(key);
        }
        if let Some(overlay) = self.overlay.take() {
            return self.on_overlay_key(overlay, key);
        }
        if let Some(qid) = self.input_target()
            && let Some(fx) = self.on_input_key(qid, key)
        {
            return fx;
        }
        if self.card.is_some() {
            return self.on_card_key(key);
        }
        if self.epic.is_some() {
            return self.on_epic_key(key);
        }
        if self.side_focused() {
            return self.on_side_key(key);
        }
        self.on_board_key(key)
    }

    fn on_board_key(&mut self, key: Key) -> Vec<Effect> {
        let n = self.lanes().len();
        match key {
            Key::Char('q') => self.quit = true,
            Key::Left | Key::Char('h') => {
                if self.sel_lane > 0 {
                    self.sel_lane -= 1;
                } else if self.side_shown() && !self.side_rows().is_empty() {
                    // From the leftmost lane into the side panel (docs/board-v2.md 5.7).
                    self.side_focus = true;
                    self.check_side();
                }
                self.keep_selection_visible();
            }
            Key::Right | Key::Char('l') => {
                if self.sel_lane + 1 < n {
                    self.sel_lane += 1;
                }
                self.keep_selection_visible();
            }
            Key::Up | Key::Char('k') => {
                if let Some(i) = self.card_index(self.sel_lane) {
                    self.select_card_at(i.saturating_sub(1));
                }
            }
            Key::Down | Key::Char('j') => {
                if let Some(i) = self.card_index(self.sel_lane) {
                    let len = self.lanes()[self.sel_lane].cards.len();
                    self.select_card_at((i + 1).min(len - 1));
                }
            }
            Key::Home => self.select_card_at(0),
            Key::End => {
                if let Some(l) = self.lanes().get(self.sel_lane)
                    && !l.cards.is_empty()
                {
                    let last = l.cards.len() - 1;
                    self.select_card_at(last);
                }
            }
            Key::Enter => {
                if let Some(seq) = self.selected_card().map(|c| c.card.seq) {
                    return self.open_card(seq);
                }
            }
            Key::Char('a') => return self.jump_to_question(),
            Key::Char('i') => {
                if let Some((id, seq)) = self.selected_card().map(|c| (c.card.id, c.card.seq)) {
                    self.open_comment(id, seq);
                }
            }
            // Clears the epic selection; the focus goes back to the lanes (board-v2 5.7).
            Key::Esc if self.sel_epic.is_some() => {
                self.sel_epic = None;
                self.side_focus = false;
            }
            Key::Char('e') => self.cycle_epic(1),
            Key::Char('E') => self.cycle_epic(-1),
            Key::Char('o') => self.cycle_sort(),
            // Under 80 columns the panel is hidden and `s` does nothing.
            Key::Char('s') if self.size.0 >= layout::SIDE_MIN_SCREEN => {
                self.show_side = !self.show_side;
                // Hiding the panel while it has the focus returns the focus to the lanes.
                self.check_side();
                self.keep_selection_visible();
            }
            Key::Char('w') => {
                self.overlay = Some(Overlay::Workspaces(Picker::loading()));
                return vec![Effect::LoadWorkspaces];
            }
            Key::Char('b') => {
                let (workspace_id, workspace_name) = match &self.data {
                    Some(d) => (
                        Some(d.snapshot.workspace.id),
                        Some(d.snapshot.workspace.name.clone()),
                    ),
                    None => (None, None),
                };
                self.overlay = Some(Overlay::Boards {
                    workspace_id,
                    workspace_name,
                    picker: Picker::loading(),
                });
                return vec![Effect::LoadBoards { workspace_id }];
            }
            Key::Char('r') => return vec![Effect::LoadBoard(self.board_target())],
            Key::Char('?') => self.overlay = Some(Overlay::Help),
            _ => {}
        }
        Vec::new()
    }

    /// Opens the card screen of card `seq` of the shown board.
    pub fn open_card(&mut self, seq: i32) -> Vec<Effect> {
        let Some(board_id) = self.data.as_ref().map(BoardData::id) else {
            return Vec::new();
        };
        self.card = Some(CardView::new(seq));
        self.epic = None;
        self.history.clear();
        vec![Effect::LoadCard { board_id, seq }]
    }

    /// `a`: the next card (in board order, after the selected one, wrapping) that has an
    /// open question; select it and open it (docs/questions.md section 7.1).
    fn jump_to_question(&mut self) -> Vec<Effect> {
        let cur = self.card_index(self.sel_lane);
        let mut found: Vec<(bool, i64, i32)> = Vec::new();
        for (li, l) in self.lanes().iter().enumerate() {
            for (ci, c) in l.cards.iter().enumerate() {
                if c.open_question_count > 0 {
                    let after =
                        li > self.sel_lane || (li == self.sel_lane && cur.is_none_or(|x| ci > x));
                    found.push((after, c.card.id, c.card.seq));
                }
            }
        }
        let next = found
            .iter()
            .find(|(after, ..)| *after)
            .or_else(|| found.first())
            .map(|(_, id, seq)| (*id, *seq));
        match next {
            Some((id, seq)) => {
                self.select_card_id(id);
                self.open_card(seq)
            }
            None => {
                self.flash = Some(MSG_NO_QUESTIONS.to_string());
                Vec::new()
            }
        }
    }

    fn on_overlay_key(&mut self, overlay: Overlay, key: Key) -> Vec<Effect> {
        match overlay {
            // Any key closes the help screen.
            Overlay::Help => Vec::new(),
            Overlay::Workspaces(mut p) => match key {
                Key::Esc => Vec::new(),
                Key::Char('q') => {
                    self.quit = true;
                    Vec::new()
                }
                Key::Up | Key::Char('k') => {
                    p.step(-1);
                    self.overlay = Some(Overlay::Workspaces(p));
                    Vec::new()
                }
                Key::Down | Key::Char('j') => {
                    p.step(1);
                    self.overlay = Some(Overlay::Workspaces(p));
                    Vec::new()
                }
                Key::Enter => match p.selected() {
                    Some(ws) => {
                        let workspace_id = Some(ws.id);
                        self.overlay = Some(Overlay::Boards {
                            workspace_id,
                            workspace_name: Some(ws.name.clone()),
                            picker: Picker::loading(),
                        });
                        vec![Effect::LoadBoards { workspace_id }]
                    }
                    None => {
                        self.overlay = Some(Overlay::Workspaces(p));
                        Vec::new()
                    }
                },
                _ => {
                    self.overlay = Some(Overlay::Workspaces(p));
                    Vec::new()
                }
            },
            Overlay::Boards {
                workspace_id,
                workspace_name,
                mut picker,
            } => {
                let mut keep = true;
                let mut fx = Vec::new();
                match key {
                    Key::Esc => keep = false,
                    Key::Char('q') => {
                        self.quit = true;
                        keep = false;
                    }
                    Key::Up | Key::Char('k') => picker.step(-1),
                    Key::Down | Key::Char('j') => picker.step(1),
                    Key::Enter => {
                        if let Some(b) = picker.selected() {
                            keep = false;
                            fx = self.switch_board(b.id);
                        }
                    }
                    _ => {}
                }
                if keep {
                    self.overlay = Some(Overlay::Boards {
                        workspace_id,
                        workspace_name,
                        picker,
                    });
                }
                fx
            }
        }
    }

    fn switch_board(&mut self, board_id: i64) -> Vec<Effect> {
        if self.data.as_ref().map(BoardData::id) == Some(board_id) {
            return Vec::new();
        }
        self.want = Some(board_id);
        self.card = None;
        self.epic = None;
        self.history.clear();
        vec![Effect::LoadBoard(BoardTarget::Id(board_id))]
    }

    // ----- card screen ------------------------------------------------------------

    /// The bottom message of the card screen: an external link target, or the flash.
    pub fn card_message(&self) -> Option<&str> {
        self.card
            .as_ref()
            .and_then(|v| v.message.as_deref())
            .or(self.flash.as_deref())
    }

    /// Blockers of card `seq` (sources of `blocks` links that are still open).
    pub fn blocked_by(&self, seq: i32) -> &[i32] {
        self.data
            .as_ref()
            .and_then(|d| d.snapshot.card(seq))
            .map_or(&[], |(_, c)| c.blocked_by.as_slice())
    }

    /// The model judge's tags of card `card_id` (docs/tui.md section 12).
    pub fn card_judge(&self, card_id: i64) -> CardJudge {
        self.data
            .as_ref()
            .map_or_else(CardJudge::default, |d| d.judge(card_id))
    }

    /// Rows under the card screen's info row: the blocked row, then one row per active
    /// model-judge tag (docs/tui.md section 12.4).
    pub fn card_status_rows(&self, d: &CardDetail) -> usize {
        let blocked = d.card.blocked_reason.is_some() || !self.blocked_by(d.card.seq).is_empty();
        usize::from(blocked) + self.card_judge(d.card.id).detail_lines().len()
    }

    fn card_metrics(&self) -> Option<CardMetrics> {
        let view = self.card.as_ref()?;
        let d = view.detail.as_ref()?;
        let ui = QuestionUi {
            marks: &self.marks,
            cursor: view.q_cursor,
            focused: view.focus == Section::Questions,
        };
        let content = layout::card_content(d, self.size.0, self.today, ui, self.look.tables);
        let shown = Section::shown(!d.questions.is_empty());
        let natural: Vec<usize> = shown.iter().map(|s| content.lines(*s)).collect();
        let msg =
            layout::message_lines(self.card_message(), self.size.0).len() + self.comment_rows();
        let head = layout::card_head_rows(self.size, content.title.len(), self.card_status_rows(d));
        let heights = layout::section_heights(self.size, head, &natural, msg);
        Some(CardMetrics {
            shown,
            natural,
            heights,
            choices: content.choices,
        })
    }

    fn clamp_card_scroll(&mut self) {
        let Some(m) = self.card_metrics() else {
            return;
        };
        let Some(view) = self.card.as_mut() else {
            return;
        };
        let (n, h) = m.of(Section::Body);
        view.body_scroll = view.body_scroll.min(n.saturating_sub(h));
        let (n, h) = m.of(Section::Notes);
        view.notes_scroll = view.notes_scroll.min(n.saturating_sub(h));
        // The question cursor is always visible, with its question's text when they fit
        // together (docs/questions.md §7.2).
        let (n, h) = m.of(Section::Questions);
        match m.cursor_row(view.q_cursor) {
            Some(row) => {
                view.q_peek &= row.index == 0;
                view.q_scroll = layout::question_scroll(view.q_scroll, row, h, n, view.q_peek);
            }
            None => {
                view.q_peek = false;
                view.q_scroll = view.q_scroll.min(n.saturating_sub(h));
            }
        }
        let links = view
            .detail
            .as_ref()
            .map_or(0, |d| layout::link_rows(d).len());
        view.link_sel = view.link_sel.min(links.saturating_sub(1));
    }

    fn on_card_key(&mut self, key: Key) -> Vec<Effect> {
        let metrics = self.card_metrics();
        let board_id = self.data.as_ref().map(BoardData::id);
        let Some(view) = self.card.as_mut() else {
            return Vec::new();
        };
        let had_message = view.message.take().is_some();
        // Only `↑` `↓` keep the view scrolled into a question's text.
        let peek = std::mem::take(&mut view.q_peek);
        let with_questions = view.has_questions();
        let links = view
            .detail
            .as_ref()
            .map_or(0, |d| layout::link_rows(d).len());
        let of = |s: Section| metrics.as_ref().map_or((0, 1), |m| m.of(s));
        // On an option row, `i` and a typed non-ASCII character go to that question's
        // input row (docs/board-v2.md section 13.17).
        if let Some(qid) = self.option_row_question() {
            match key {
                Key::Char('i') => {
                    self.jump_to_input(qid);
                    return Vec::new();
                }
                Key::Char(c) if !c.is_ascii() && !c.is_control() => {
                    self.jump_to_input(qid);
                    self.marks.entry(qid).or_default().insert(&c.to_string());
                    self.clamp_card_scroll();
                    return Vec::new();
                }
                _ => {}
            }
        }
        let Some(view) = self.card.as_mut() else {
            return Vec::new();
        };
        match key {
            Key::Char('q') => self.quit = true,
            Key::Char('i') => {
                let seq = view.seq;
                let id = view.detail.as_ref().map(|d| d.card.id).or_else(|| {
                    self.data
                        .as_ref()
                        .and_then(|d| d.snapshot.card(seq))
                        .map(|(_, c)| c.card.id)
                });
                if let Some(id) = id {
                    self.open_comment(id, seq);
                }
            }
            Key::Esc => return self.close_to_board(),
            Key::Backspace | Key::Left | Key::Char('h') => return self.go_back(),
            Key::Tab => view.focus = view.focus.next(with_questions),
            Key::BackTab => view.focus = view.focus.prev(with_questions),
            Key::Up | Key::Down | Key::Char('k') | Key::Char('j') | Key::PageUp | Key::PageDown => {
                let (step, page) = match key {
                    Key::Up | Key::Char('k') => (-1, false),
                    Key::Down | Key::Char('j') => (1, false),
                    Key::PageUp => (-1, true),
                    _ => (1, true),
                };
                let (natural, height) = of(view.focus);
                let amount = if page {
                    step * height.max(1) as isize
                } else {
                    step
                };
                let scroll = |v: &mut usize| {
                    let max = natural.saturating_sub(height);
                    *v = (*v as isize + amount).clamp(0, max as isize) as usize;
                };
                match view.focus {
                    Section::Body => scroll(&mut view.body_scroll),
                    Section::Notes => scroll(&mut view.notes_scroll),
                    Section::Links => {
                        if links > 0 {
                            view.link_sel = (view.link_sel as isize + amount)
                                .clamp(0, links as isize - 1)
                                as usize;
                        }
                    }
                    Section::Questions => {
                        // On a question's first option, when the text does not fit with
                        // it, `↑` scrolls up into the text line by line and `↓` scrolls
                        // back before the cursor moves (docs/questions.md §7.3).
                        if !page && peek_scroll(view, metrics.as_ref(), step, height, peek) {
                            return Vec::new();
                        }
                        let choices = view
                            .detail
                            .as_ref()
                            .map(|d| layout::choices(&d.questions, &self.marks))
                            .unwrap_or_default();
                        if !choices.is_empty() {
                            let at = view
                                .q_cursor
                                .and_then(|c| choices.iter().position(|x| *x == c))
                                .unwrap_or(0) as isize;
                            let to = (at + amount).clamp(0, choices.len() as isize - 1);
                            view.q_cursor = Some(choices[to as usize]);
                        }
                    }
                }
            }
            Key::Char(' ') if view.focus == Section::Questions => self.toggle_mark(),
            Key::Enter if view.focus == Section::Questions => {
                let fx = self.submit();
                self.clamp_card_scroll();
                return fx;
            }
            Key::Enter if view.focus == Section::Links => {
                let row = view
                    .detail
                    .as_ref()
                    .and_then(|d| layout::link_rows(d).into_iter().nth(view.link_sel));
                match row.map(|r| (r.card, r.external)) {
                    Some((Some(seq), _)) => {
                        let Some(board_id) = board_id else {
                            return Vec::new();
                        };
                        let from = self.card.take().expect("card view is open");
                        self.history.push(Screen::Card(Box::new(from)));
                        self.card = Some(CardView::new(seq));
                        return vec![Effect::LoadCard { board_id, seq }];
                    }
                    // Pressing Enter again hides the message.
                    Some((None, Some(t))) if !had_message => view.message = Some(t),
                    _ => {}
                }
            }
            Key::Char('?') => self.overlay = Some(Overlay::Help),
            _ => {}
        }
        self.clamp_card_scroll();
        Vec::new()
    }

    /// The question whose option (or 「在 session 裡說明」) row has the cursor, while the
    /// card screen's question section has the focus.
    fn option_row_question(&self) -> Option<i64> {
        if self.overlay.is_some() {
            return None;
        }
        let view = self.card.as_ref()?;
        if view.focus != Section::Questions {
            return None;
        }
        let (qid, idx) = view.q_cursor?;
        let q = view.open_question(qid)?;
        (idx < layout::input_index(q)).then_some(qid)
    }

    /// Moves the question cursor to question `qid`'s input row. Always allowed: a marked
    /// 「在 session 裡說明」 is unmarked (docs/board-v2.md section 13.17).
    fn jump_to_input(&mut self, qid: i64) {
        let Some(view) = self.card.as_mut() else {
            return;
        };
        let Some(index) = view.open_question(qid).map(layout::input_index) else {
            return;
        };
        if let Some(m) = self.marks.get_mut(&qid) {
            m.chat = false;
            if m.is_empty() {
                self.marks.remove(&qid);
            }
        }
        view.q_cursor = Some((qid, index));
        self.clamp_card_scroll();
    }

    // ----- comment row --------------------------------------------------------------

    /// Rows the comment row takes at the bottom of the screen.
    pub fn comment_rows(&self) -> usize {
        usize::from(self.comment.is_some())
    }

    /// `i`: opens the comment row for card `card_id` (number `seq`), unless the board is
    /// archived (docs/board-v2.md section 13.15).
    fn open_comment(&mut self, card_id: i64, seq: i32) {
        let archived = self
            .data
            .as_ref()
            .is_some_and(|d| d.snapshot.board.status == BoardStatus::Archived);
        if archived {
            self.flash = Some(MSG_ARCHIVED_NO_COMMENT.to_string());
            return;
        }
        self.comment = Some(CommentInput::new(card_id, seq));
        self.clamp_card_scroll();
        self.scroll_epic();
    }

    /// Keys while the comment row is open: they type, except Enter (send) and Esc (close,
    /// the text is dropped); Ctrl+C quits before this. Other keys do nothing.
    fn on_comment_key(&mut self, key: Key) -> Vec<Effect> {
        let Some(c) = self.comment.as_mut() else {
            return Vec::new();
        };
        match key {
            Key::Esc => {
                self.comment = None;
                self.clamp_card_scroll();
                self.scroll_epic();
            }
            Key::Enter => return self.send_comment(),
            k => {
                edit_text(&mut c.text, &mut c.cursor, k, COMMENT_LIMIT);
            }
        }
        Vec::new()
    }

    /// Enter on the comment row: sends it, unless there is no text.
    fn send_comment(&mut self) -> Vec<Effect> {
        let Some(board_id) = self.data.as_ref().map(BoardData::id) else {
            return Vec::new();
        };
        let Some(c) = self.comment.as_mut() else {
            return Vec::new();
        };
        if kb_core::comment::normalize_text(&c.text).is_empty() {
            self.flash = Some(MSG_TYPE_FIRST.to_string());
            return Vec::new();
        }
        c.sending = true;
        let request = CommentRequest {
            board_id,
            card_id: c.card_id,
            seq: c.seq,
            text: c.text.clone(),
        };
        self.flash = Some(MSG_SENDING.to_string());
        vec![Effect::Comment(request)]
    }

    /// Space on a choice: marks it. Single choice: it replaces the previous mark. Multiple
    /// choice: it toggles. 「在 session 裡說明」 and the options exclude each other
    /// (docs/questions.md section 7.2).
    fn toggle_mark(&mut self) {
        let Some(view) = self.card.as_ref() else {
            return;
        };
        let Some((qid, idx)) = view.q_cursor else {
            return;
        };
        let Some(q) = view.open_question(qid) else {
            return;
        };
        if idx > q.options.len() {
            return; // the input row is not marked
        }
        let multi = q.question.multi;
        let option = q.options.get(idx).map(|o| o.id);
        let m = self.marks.entry(qid).or_default();
        match option {
            None => {
                m.chat = if multi { !m.chat } else { true };
                if m.chat {
                    m.options.clear();
                }
            }
            Some(oid) => {
                if multi {
                    if !m.options.remove(&oid) {
                        m.options.insert(oid);
                    }
                } else {
                    m.options.clear();
                    m.options.insert(oid);
                }
                if !m.options.is_empty() {
                    m.chat = false;
                }
            }
        }
        if m.is_empty() {
            self.marks.remove(&qid);
        }
    }

    /// The question whose input row has the cursor, when the card screen's question
    /// section has the focus and 「在 session 裡說明」 is not marked: then keys type.
    pub fn input_target(&self) -> Option<i64> {
        if self.overlay.is_some() {
            return None;
        }
        let view = self.card.as_ref()?;
        if view.focus != Section::Questions {
            return None;
        }
        let (qid, idx) = view.q_cursor?;
        let q = view.open_question(qid)?;
        let chat = self.marks.get(&qid).is_some_and(|m| m.chat);
        (idx == layout::input_index(q) && !chat).then_some(qid)
    }

    /// Keys while the cursor is on an input row (docs/board-v2.md section 7.1): they type,
    /// except ↑ ↓ Tab Shift+Tab Enter Esc, which return `None` and act as on the card
    /// screen (Ctrl+C quits before this).
    fn on_input_key(&mut self, qid: i64, key: Key) -> Option<Vec<Effect>> {
        let m = self.marks.entry(qid).or_default();
        let passes = matches!(
            key,
            Key::Up | Key::Down | Key::Tab | Key::BackTab | Key::Enter | Key::Esc
        );
        m.edit(key);
        if m.is_empty() {
            self.marks.remove(&qid);
        }
        if passes {
            return None;
        }
        if let Some(view) = self.card.as_mut() {
            view.message = None;
        }
        self.clamp_card_scroll();
        Some(Vec::new())
    }

    /// Pasted text goes into the open comment row, or into the input row with the cursor;
    /// line breaks become spaces.
    fn on_paste(&mut self, text: &str) {
        if let Some(c) = self.comment.as_mut() {
            if !c.sending {
                self.flash = None;
                c.insert(text);
            }
            return;
        }
        if let Some(qid) = self.input_target() {
            self.flash = None;
            self.marks.entry(qid).or_default().insert(text);
            self.clamp_card_scroll();
        }
    }

    /// Enter on a choice: sends the marks of the cursor's question.
    fn submit(&mut self) -> Vec<Effect> {
        let Some(view) = self.card.as_ref() else {
            return Vec::new();
        };
        let Some((qid, _)) = view.q_cursor else {
            return Vec::new();
        };
        let Some(q) = view.open_question(qid) else {
            return Vec::new();
        };
        let Some(m) = self.marks.get(&qid).filter(|m| m.submittable()) else {
            self.flash = Some(MSG_MARK_FIRST.to_string());
            return Vec::new();
        };
        if self.submitting == Some(qid) {
            return Vec::new();
        }
        let Some(data) = self.data.as_ref() else {
            return Vec::new();
        };
        let request = AnswerRequest {
            board_id: data.id(),
            seq: view.seq,
            question_id: qid,
            option_ids: if m.chat {
                Vec::new()
            } else {
                q.options
                    .iter()
                    .filter(|o| m.options.contains(&o.id))
                    .map(|o| o.id)
                    .collect()
            },
            chat: m.chat,
            // The text goes with the options; with 「在 session 裡說明」 it is not sent.
            text: Some(m.text.trim())
                .filter(|t| !m.chat && !t.is_empty())
                .map(str::to_string),
            terminal: data.snapshot.board.herdr_terminal.clone(),
        };
        self.submitting = Some(qid);
        self.flash = Some(MSG_SENDING.to_string());
        vec![Effect::Answer(request)]
    }

    /// Backspace / Left on a card or epic screen: one step back along the shared history,
    /// restoring that screen with its selection; with an empty history, to the board
    /// (docs/tui.md section 5.4, docs/board-v2.md section 5.8).
    fn go_back(&mut self) -> Vec<Effect> {
        match self.history.pop() {
            Some(Screen::Card(prev)) => {
                let seq = prev.seq;
                self.epic = None;
                self.card = Some(*prev);
                self.clamp_card_scroll();
                match self.data.as_ref() {
                    Some(d) => vec![Effect::LoadCard {
                        board_id: d.id(),
                        seq,
                    }],
                    None => Vec::new(),
                }
            }
            Some(Screen::Epic(prev)) => {
                self.card = None;
                self.epic = Some(prev);
                self.refresh_epic_view();
                Vec::new()
            }
            None => self.close_to_board(),
        }
    }

    /// Esc: straight to the board, however deep the history is. From a card screen the
    /// focus goes to the lanes, on that card; from an epic screen to the side panel, on
    /// that epic (docs/board-v2.md section 5.8).
    fn close_to_board(&mut self) -> Vec<Effect> {
        self.history.clear();
        if let Some(v) = self.card.take() {
            let card_id = v.detail.as_ref().map(|d| d.card.id).or_else(|| {
                self.data
                    .as_ref()
                    .and_then(|d| d.snapshot.card(v.seq))
                    .map(|(_, c)| c.card.id)
            });
            self.side_focus = false;
            if let Some(id) = card_id {
                self.select_card_id(id);
            }
        } else if let Some(v) = self.epic.take() {
            self.sel_epic = Some(v.key);
            self.side_focus = true;
            self.check_side();
        }
        Vec::new()
    }

    // ----- epic screen ------------------------------------------------------------

    /// Opens the epic screen of a side-panel row, with an empty history.
    pub fn open_epic(&mut self, key: EpicKey) {
        self.card = None;
        self.history.clear();
        // The side panel is not on screen; returning to the board gives it the focus.
        self.side_focus = false;
        let mut view = EpicView::new(key);
        if let Some(d) = self.data.as_ref() {
            view.resolve(&epic_cards(&d.snapshot, key));
        }
        self.epic = Some(view);
    }

    /// After a reload: keeps the open epic screen's selection on the same card, or goes
    /// back to the board when its epic was removed.
    fn refresh_epic_view(&mut self) {
        let Some(view) = self.epic.as_mut() else {
            return;
        };
        let Some(d) = self.data.as_ref() else {
            return;
        };
        let gone = match view.key {
            EpicKey::Epic(id) => !d.snapshot.epics.iter().any(|e| e.epic.id == id),
            EpicKey::NoEpic => false,
        };
        if gone {
            self.epic = None;
            self.card = None;
            self.history.clear();
            self.side_focus = false;
            self.flash = Some(MSG_EPIC_REMOVED.to_string());
            self.check_side();
            return;
        }
        view.resolve(&epic_cards(&d.snapshot, view.key));
        // The selected card may have moved to the other group.
        self.scroll_epic();
    }

    /// Keeps the open epic screen's selected row on screen; the 卡片 and 已捨棄 groups
    /// scroll together, in lines (docs/board-v2.md section 5.8).
    fn scroll_epic(&mut self) {
        let h = self.epic_list_height();
        let size = self.size;
        let (Some(view), Some(d)) = (self.epic.as_mut(), self.data.as_ref()) else {
            return;
        };
        let (cards, dropped) = epic_card_groups(&d.snapshot, view.key);
        view.first = layout::scroll_to_show(
            view.first,
            layout::epic_row_line(view.sel, cards.len(), size),
            h,
            layout::epic_total_lines(cards.len(), dropped.len(), size),
        );
    }

    /// Rows of the epic list on screen (for paging).
    fn epic_list_height(&self) -> usize {
        let (Some(view), Some(d)) = (self.epic.as_ref(), self.data.as_ref()) else {
            return 1;
        };
        let msg =
            layout::message_lines(self.flash.as_deref(), self.size.0).len() + self.comment_rows();
        let name = layout::epic_screen_title(&d.snapshot, view.key);
        let name_rows = layout::screen_title_lines(&name, self.size.0).len();
        layout::epic_layout(self.size, name_rows, msg)
    }

    fn on_epic_key(&mut self, key: Key) -> Vec<Effect> {
        let page = self.epic_list_height().max(1) as isize;
        let (Some(view), Some(d)) = (self.epic.as_mut(), self.data.as_ref()) else {
            return Vec::new();
        };
        // One selection over both groups: the cards, then the dropped cards.
        let rows = epic_cards(&d.snapshot, view.key);
        let n = rows.len() as isize;
        let step = |view: &mut EpicView, to: isize| {
            if n > 0 {
                view.sel = to.clamp(0, n - 1) as usize;
                view.card_id = Some(rows[view.sel].card.id);
            }
        };
        let at = view.sel as isize;
        match key {
            Key::Up | Key::Char('k') => step(view, at - 1),
            Key::Down | Key::Char('j') => step(view, at + 1),
            Key::Home => step(view, 0),
            Key::End => step(view, n - 1),
            Key::PageUp => step(view, at - page),
            Key::PageDown => step(view, at + page),
            Key::Enter => {
                let Some(seq) = rows.get(view.sel).map(|c| c.card.seq) else {
                    return Vec::new();
                };
                let board_id = d.id();
                let from = self.epic.take().expect("epic view is open");
                self.history.push(Screen::Epic(from));
                self.card = Some(CardView::new(seq));
                return vec![Effect::LoadCard { board_id, seq }];
            }
            Key::Left | Key::Char('h') | Key::Backspace => return self.go_back(),
            Key::Esc => return self.close_to_board(),
            Key::Char('i') => {
                if let Some((id, seq)) = rows.get(view.sel).map(|c| (c.card.id, c.card.seq)) {
                    self.open_comment(id, seq);
                }
            }
            Key::Char('q') => self.quit = true,
            Key::Char('?') => self.overlay = Some(Overlay::Help),
            _ => {}
        }
        self.scroll_epic();
        Vec::new()
    }

    // ----- side panel -------------------------------------------------------------

    /// Keys while the side panel has the focus (docs/board-v2.md section 5.7). Keys it
    /// does not use work as on the board.
    fn on_side_key(&mut self, key: Key) -> Vec<Effect> {
        let rows = self.side_rows();
        let at = self
            .sel_epic
            .and_then(|k| rows.iter().position(|r| *r == k));
        let pick = |i: isize| {
            rows.get(i.clamp(0, rows.len() as isize - 1) as usize)
                .copied()
        };
        match key {
            Key::Up | Key::Char('k') => {
                self.sel_epic = pick(at.map_or(0, |i| i as isize - 1));
            }
            Key::Down | Key::Char('j') => {
                self.sel_epic = pick(at.map_or(0, |i| i as isize + 1));
            }
            Key::Home => self.sel_epic = pick(0),
            Key::End => self.sel_epic = pick(rows.len() as isize - 1),
            Key::Enter => {
                if let Some(k) = self.sel_epic {
                    self.open_epic(k);
                }
            }
            Key::Right | Key::Char('l') => self.side_focus = false,
            Key::Left | Key::Char('h') | Key::PageUp | Key::PageDown => {}
            _ => return self.on_board_key(key),
        }
        Vec::new()
    }
}

/// Ids of cards that are new in `new` or whose shown content differs from `old`.
pub fn changed_cards(old: &BoardSnapshot, new: &BoardSnapshot) -> Vec<i64> {
    let mut before: HashMap<i64, &CardSummary> = HashMap::new();
    for l in &old.lanes {
        for c in &l.cards {
            before.insert(c.card.id, c);
        }
    }
    let mut out = Vec::new();
    for l in &new.lanes {
        for c in &l.cards {
            match before.get(&c.card.id) {
                Some(o) if same_card(o, c) => {}
                _ => out.push(c.card.id),
            }
        }
    }
    out
}

fn same_card(a: &CardSummary, b: &CardSummary) -> bool {
    let (x, y) = (&a.card, &b.card);
    x.lane_id == y.lane_id
        && x.title == y.title
        && x.body == y.body
        && x.priority == y.priority
        && x.owner == y.owner
        && x.blocked_reason == y.blocked_reason
        && x.result == y.result
        && x.updated_at == y.updated_at
        && x.origin == y.origin
        && x.epic_id == y.epic_id
        && a.note_count == b.note_count
        && a.link_count == b.link_count
        && a.open_question_count == b.open_question_count
        && a.epic_name == b.epic_name
        && a.spawned_from == b.spawned_from
        && a.spawned_count == b.spawned_count
        && a.blocked_by == b.blocked_by
        && a.unreplied_comment_count == b.unreplied_comment_count
}
