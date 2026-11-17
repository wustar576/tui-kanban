//! Read queries. Everything here is plain `SELECT`, so it works when connected as
//! `kb_reader`. This is the API the TUI consumes.
//!
//! All functions take `&mut PgConnection`; a pooled connection or a transaction can be
//! passed with `&mut *conn`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::postgres::PgConnection;

use crate::error::Result;
use crate::model::{
    Activity, Board, BoardStatus, Card, Cause, CauseKind, DecisionKind, Epic, Lane, LaneKind,
    LinkKind, Note, NoteKind, Origin, Outcome, Question, QuestionOption, QuestionStatus, Workspace,
    WorkspaceSource,
};
use crate::resolve::{HerdrContext, current_herdr_context};

/// Number of cards per lane kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct KindCounts {
    pub todo: i64,
    pub doing: i64,
    pub done: i64,
    pub dropped: i64,
}

/// A workspace with its board counts (`kb workspace list`, TUI workspace menu).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WorkspaceSummary {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub source: WorkspaceSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub active_boards: i64,
    pub archived_boards: i64,
    /// Latest of the workspace's own `updated_at` and its boards' `updated_at`.
    pub last_updated_at: DateTime<Utc>,
}

/// A board with per-lane-kind card counts (`kb board list`, TUI board menu).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct BoardSummary {
    pub id: i64,
    pub workspace_id: i64,
    pub workspace_name: String,
    pub session_key: String,
    pub title: String,
    pub goal: String,
    pub status: BoardStatus,
    pub outcome: Option<Outcome>,
    pub rev: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    #[sqlx(flatten)]
    pub cards: KindCounts,
}

/// Which boards [`list_boards`] returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoardQuery {
    /// Only boards of this workspace; `None` for all workspaces.
    pub workspace_id: Option<i64>,
    /// Also return archived boards.
    pub include_archived: bool,
}

/// A card as shown on the board: all fields (including `origin` and `epic_id`) plus counts
/// and the relations the board draws (docs/board-v2.md sections 4.3 and 5.4).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CardSummary {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub card: Card,
    pub note_count: i64,
    /// Links in both directions (this card as source or as target).
    pub link_count: i64,
    /// Questions on this card with status `open` (waiting for the human).
    pub open_question_count: i64,
    /// Name of the card's epic; `None` when the card has no epic.
    pub epic_name: Option<String>,
    /// Number of the card this one was spawned from (`spawned` link pointing at it).
    pub spawned_from: Option<i32>,
    /// Number of cards spawned from this one (`spawned` links from it).
    pub spawned_count: i64,
    /// Numbers of the cards blocking this one: sources of `blocks` links pointing at it
    /// that are not in a `done` or `dropped` lane. Ascending.
    pub blocked_by: Vec<i32>,
    /// Comments from the human on this card without a newer `reply` note on it
    /// (docs/board-v2.md section 13.9). The TUI shows a tag when this is above 0.
    pub unreplied_comment_count: i64,
}

impl CardSummary {
    /// Blocked: a blocked reason, or an unfinished card blocking it through a `blocks`
    /// link (docs/board-v2.md section 10.2).
    pub fn is_blocked(&self) -> bool {
        self.card.blocked_reason.is_some() || !self.blocked_by.is_empty()
    }

    /// Ready to start: neither blocked nor under discussion (docs/board-v2.md sections 10.2
    /// and 17.1). Only such a card can be the head of the queue.
    pub fn is_ready(&self) -> bool {
        !self.is_blocked() && self.card.origin != Origin::Discussing
    }
}

/// Card counts of an epic (docs/board-v2.md section 3.4). Cards in `dropped` lanes are
/// never counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EpicCounts {
    /// Cards of the epic.
    pub total: i64,
    /// Of those, cards in `done` lanes.
    pub done: i64,
    /// Of those, cards with origin `derived`.
    pub derived: i64,
    /// Of those, cards with origin `discussing`.
    pub discussing: i64,
}

impl EpicCounts {
    /// Counts one card in a lane of `kind`.
    pub fn add(&mut self, kind: LaneKind, origin: Origin) {
        if kind == LaneKind::Dropped {
            return;
        }
        self.total += 1;
        if kind == LaneKind::Done {
            self.done += 1;
        }
        match origin {
            Origin::Derived => self.derived += 1,
            Origin::Discussing => self.discussing += 1,
            Origin::Agreed => {}
        }
    }

    /// Every counted card is done, and there is at least one.
    pub fn is_finished(&self) -> bool {
        self.total > 0 && self.done == self.total
    }
}

/// An epic with its counts.
#[derive(Debug, Clone, Serialize)]
pub struct EpicSummary {
    #[serde(flatten)]
    pub epic: Epic,
    #[serde(flatten)]
    pub counts: EpicCounts,
}

/// A lane and its cards in display order.
#[derive(Debug, Clone, Serialize)]
pub struct LaneWithCards {
    #[serde(flatten)]
    pub lane: Lane,
    pub cards: Vec<CardSummary>,
}

/// Everything the board screen draws, except activity.
#[derive(Debug, Clone, Serialize)]
pub struct BoardSnapshot {
    pub workspace: Workspace,
    pub board: Board,
    /// Lanes left to right (`position`); cards in the default order of
    /// [`default_card_order`].
    pub lanes: Vec<LaneWithCards>,
    /// Questions on this board with status `open`.
    pub open_question_count: i64,
    /// Epics in `position` order, with their counts.
    pub epics: Vec<EpicSummary>,
    /// Counts of the cards without an epic; `None` when every card has an epic.
    pub no_epic: Option<EpicCounts>,
    /// Cards with origin `discussing` that are not in a `done` or `dropped` lane.
    pub discussing_count: i64,
    /// Comments on this board without a reply (sum of the cards' counts).
    pub unreplied_comment_count: i64,
}

impl BoardSnapshot {
    /// `(done, total)` for the header: cards in `done` lanes, and all cards except those in
    /// `dropped` lanes (docs/tui.md section 4.1).
    pub fn progress(&self) -> (usize, usize) {
        let count = |pred: fn(LaneKind) -> bool| {
            self.lanes
                .iter()
                .filter(|l| pred(l.lane.kind))
                .map(|l| l.cards.len())
                .sum::<usize>()
        };
        (
            count(|k| k == LaneKind::Done),
            count(|k| k != LaneKind::Dropped),
        )
    }

    /// The head of the queue, the card that will be done next (docs/board-v2.md section
    /// 10.2): the first card that is ready ([`CardSummary::is_ready`]: neither blocked nor
    /// under discussion), taking the `todo` lanes left to right and each lane in display
    /// order. `None` when no todo card is ready.
    pub fn next_card(&self) -> Option<(&Lane, &CardSummary)> {
        self.todo_lanes()
            .find_map(|l| l.cards.iter().find(|c| c.is_ready()).map(|c| (&l.lane, c)))
    }

    /// Cards in the `todo` lanes, ready or not.
    pub fn todo_card_count(&self) -> usize {
        self.todo_lanes().map(|l| l.cards.len()).sum()
    }

    fn todo_lanes(&self) -> impl Iterator<Item = &LaneWithCards> {
        self.lanes.iter().filter(|l| l.lane.kind == LaneKind::Todo)
    }

    /// Finds a card by its number.
    pub fn card(&self, seq: i32) -> Option<(&Lane, &CardSummary)> {
        self.lanes.iter().find_map(|l| {
            l.cards
                .iter()
                .find(|c| c.card.seq == seq)
                .map(|c| (&l.lane, c))
        })
    }
}

/// Direction of a link relative to the card being shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkDirection {
    /// This card is the link's source (`from_card_id`).
    Outgoing,
    /// This card is the link's target (`to_card_id`).
    Incoming,
}

/// The card at the other end of a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedCard {
    pub id: i64,
    pub seq: i32,
    pub title: String,
}

/// A link as seen from one card.
#[derive(Debug, Clone, Serialize)]
pub struct LinkView {
    pub id: i64,
    pub kind: LinkKind,
    pub direction: LinkDirection,
    /// The other card; `None` for `ref` links.
    pub card: Option<LinkedCard>,
    /// External target of a `ref` link.
    pub target: Option<String>,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Everything the card screen draws.
#[derive(Debug, Clone, Serialize)]
pub struct CardDetail {
    /// Includes `origin` and `epic_id`.
    pub card: Card,
    pub lane: Lane,
    /// Name of the card's epic; `None` when the card has no epic.
    pub epic_name: Option<String>,
    /// Oldest first.
    pub notes: Vec<Note>,
    /// Both directions, in creation order.
    pub links: Vec<LinkView>,
    /// The card's questions: `open` ones first, then the others; each group oldest first.
    pub questions: Vec<QuestionView>,
    /// Ids of the comments in `notes` that have no newer `reply` note (oldest first).
    pub unreplied_comments: Vec<i64>,
    /// What the card's current `result` follows from: the causes of its latest drop or
    /// done (docs/board-v2.md section 18.4). The causes of notes are in `notes[].because`.
    pub result_because: Vec<Cause>,
    /// Decisions that follow from this card, one of its questions or one of its comments,
    /// oldest first (docs/board-v2.md section 18.4).
    pub led_to: Vec<Consequence>,
}

/// A decision that follows from a card, as seen from that card (docs/board-v2.md 18.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Consequence {
    /// What on this card the decision cites: the card itself (`card`), or one of its
    /// questions or comments.
    pub via: CauseKind,
    /// The question or comment id; `None` for `card`.
    pub via_id: Option<i64>,
    /// The card that made the decision.
    pub card_seq: i32,
    pub card_title: String,
    pub decision: DecisionKind,
    /// The note, for `note` decisions.
    pub note_id: Option<i64>,
    pub note_kind: Option<NoteKind>,
    /// The note's text, the drop reason or the done result, as written then.
    pub text: String,
    pub at: DateTime<Utc>,
}

/// A comment from the human with the card it is on (docs/board-v2.md section 13.8).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CommentView {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub note: Note,
    /// Number of the card the comment is on.
    pub card_seq: i32,
    pub card_title: String,
}

/// A question with its options, as the card screen and `kb question show` draw it.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionView {
    #[serde(flatten)]
    pub question: Question,
    /// Number of the card the question is on.
    pub card_seq: i32,
    /// In `position` order; `selected` marks the human's choice.
    pub options: Vec<QuestionOption>,
}

impl QuestionView {
    pub fn is_open(&self) -> bool {
        self.question.status == QuestionStatus::Open
    }

    /// Labels of the selected options, in position order.
    pub fn selected_labels(&self) -> Vec<&str> {
        self.options
            .iter()
            .filter(|o| o.selected)
            .map(|o| o.label.as_str())
            .collect()
    }
}

pub async fn board_by_session(conn: &mut PgConnection, session_key: &str) -> Result<Option<Board>> {
    Ok(
        sqlx::query_as::<_, Board>("SELECT * FROM board WHERE session_key = $1")
            .bind(session_key)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

pub async fn board_by_id(conn: &mut PgConnection, board_id: i64) -> Result<Option<Board>> {
    Ok(
        sqlx::query_as::<_, Board>("SELECT * FROM board WHERE id = $1")
            .bind(board_id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

pub async fn workspace_by_id(conn: &mut PgConnection, id: i64) -> Result<Option<Workspace>> {
    Ok(
        sqlx::query_as::<_, Workspace>("SELECT * FROM workspace WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

pub async fn workspace_by_key(conn: &mut PgConnection, key: &str) -> Result<Option<Workspace>> {
    Ok(
        sqlx::query_as::<_, Workspace>("SELECT * FROM workspace WHERE key = $1")
            .bind(key)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// Workspaces whose name or key equals `name_or_key` (names are not unique).
pub async fn workspaces_by_name(
    conn: &mut PgConnection,
    name_or_key: &str,
) -> Result<Vec<Workspace>> {
    Ok(sqlx::query_as::<_, Workspace>(
        "SELECT * FROM workspace WHERE name = $1 OR key = $1 ORDER BY updated_at DESC",
    )
    .bind(name_or_key)
    .fetch_all(&mut *conn)
    .await?)
}

/// The herdr context of the current terminal (via `HC_TERMINAL_ID` and `hc get`) and the
/// workspace stored for that herdr space, if any. `None` outside herdr or when `hc` fails.
pub async fn workspace_for_current_terminal(
    conn: &mut PgConnection,
) -> Result<Option<(HerdrContext, Option<Workspace>)>> {
    let Some(ctx) = current_herdr_context() else {
        return Ok(None);
    };
    let ws = workspace_by_key(conn, &ctx.workspace_key()).await?;
    Ok(Some((ctx, ws)))
}

/// All workspaces with board counts: those with active boards first, then most recently
/// changed first.
pub async fn list_workspaces(conn: &mut PgConnection) -> Result<Vec<WorkspaceSummary>> {
    Ok(sqlx::query_as::<_, WorkspaceSummary>(
        "SELECT w.id, w.key, w.name, w.source, w.created_at, w.updated_at,
                count(b.id) FILTER (WHERE b.status = 'active')   AS active_boards,
                count(b.id) FILTER (WHERE b.status = 'archived') AS archived_boards,
                greatest(w.updated_at, max(b.updated_at))        AS last_updated_at
         FROM workspace w
         LEFT JOIN board b ON b.workspace_id = w.id
         GROUP BY w.id
         ORDER BY count(b.id) FILTER (WHERE b.status = 'active') > 0 DESC,
                  greatest(w.updated_at, max(b.updated_at)) DESC, w.id",
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Boards with card counts: active boards first, then most recently changed first.
pub async fn list_boards(conn: &mut PgConnection, query: BoardQuery) -> Result<Vec<BoardSummary>> {
    Ok(sqlx::query_as::<_, BoardSummary>(
        "SELECT b.id, b.workspace_id, w.name AS workspace_name, b.session_key, b.title, b.goal,
                b.status, b.outcome, b.rev, b.created_at, b.updated_at, b.archived_at,
                count(c.id) FILTER (WHERE l.kind = 'todo')    AS todo,
                count(c.id) FILTER (WHERE l.kind = 'doing')   AS doing,
                count(c.id) FILTER (WHERE l.kind = 'done')    AS done,
                count(c.id) FILTER (WHERE l.kind = 'dropped') AS dropped
         FROM board b
         JOIN workspace w ON w.id = b.workspace_id
         LEFT JOIN card c ON c.board_id = b.id
         LEFT JOIN lane l ON l.id = c.lane_id
         WHERE ($1::bigint IS NULL OR b.workspace_id = $1)
           AND ($2::boolean OR b.status = 'active')
         GROUP BY b.id, w.name
         ORDER BY b.status = 'active' DESC, b.updated_at DESC, b.id DESC",
    )
    .bind(query.workspace_id)
    .bind(query.include_archived)
    .fetch_all(&mut *conn)
    .await?)
}

/// The most recently changed active board, in one workspace or in all of them
/// (docs/tui.md section 2.1).
pub async fn latest_active_board(
    conn: &mut PgConnection,
    workspace_id: Option<i64>,
) -> Result<Option<BoardSummary>> {
    let boards = list_boards(
        conn,
        BoardQuery {
            workspace_id,
            include_archived: false,
        },
    )
    .await?;
    Ok(boards.into_iter().next())
}

/// Full board: workspace, board, lanes in order, cards in display order with counts.
/// `None` when the board does not exist.
pub async fn board_snapshot(
    conn: &mut PgConnection,
    board_id: i64,
) -> Result<Option<BoardSnapshot>> {
    let Some(board) = board_by_id(conn, board_id).await? else {
        return Ok(None);
    };
    let workspace = sqlx::query_as::<_, Workspace>("SELECT * FROM workspace WHERE id = $1")
        .bind(board.workspace_id)
        .fetch_one(&mut *conn)
        .await?;
    let lanes =
        sqlx::query_as::<_, Lane>("SELECT * FROM lane WHERE board_id = $1 ORDER BY position")
            .bind(board_id)
            .fetch_all(&mut *conn)
            .await?;
    let cards = sqlx::query_as::<_, CardSummary>(
        "SELECT c.*,
                (SELECT count(*) FROM card_note n WHERE n.card_id = c.id) AS note_count,
                (SELECT count(*) FROM link k
                  WHERE k.from_card_id = c.id OR k.to_card_id = c.id)    AS link_count,
                (SELECT count(*) FROM question q
                  WHERE q.card_id = c.id AND q.status = 'open')          AS open_question_count,
                e.name                                                   AS epic_name,
                (SELECT min(f.seq) FROM link k JOIN card f ON f.id = k.from_card_id
                  WHERE k.to_card_id = c.id AND k.kind = 'spawned')      AS spawned_from,
                (SELECT count(*) FROM link k
                  WHERE k.from_card_id = c.id AND k.kind = 'spawned')    AS spawned_count,
                ARRAY(SELECT f.seq FROM link k
                        JOIN card f ON f.id = k.from_card_id
                        JOIN lane fl ON fl.id = f.lane_id
                       WHERE k.to_card_id = c.id AND k.kind = 'blocks'
                         AND fl.kind NOT IN ('done', 'dropped')
                       ORDER BY f.seq)                                   AS blocked_by,
                (SELECT count(*) FROM card_note m
                  WHERE m.card_id = c.id AND m.kind = 'comment'
                    AND NOT EXISTS (SELECT 1 FROM card_note r
                                     WHERE r.card_id = c.id AND r.kind = 'reply'
                                       AND r.id > m.id))                 AS unreplied_comment_count
         FROM card c
         LEFT JOIN epic e ON e.id = c.epic_id
         WHERE c.board_id = $1
         ORDER BY c.position, c.seq",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?;
    let epics =
        sqlx::query_as::<_, Epic>("SELECT * FROM epic WHERE board_id = $1 ORDER BY position")
            .bind(board_id)
            .fetch_all(&mut *conn)
            .await?;

    let mut lanes: Vec<LaneWithCards> = lanes
        .into_iter()
        .map(|lane| LaneWithCards {
            lane,
            cards: Vec::new(),
        })
        .collect();
    let mut epics: Vec<EpicSummary> = epics
        .into_iter()
        .map(|epic| EpicSummary {
            epic,
            counts: EpicCounts::default(),
        })
        .collect();
    let mut no_epic = None;
    let mut discussing_count = 0;
    let unreplied_comment_count = cards.iter().map(|c| c.unreplied_comment_count).sum();
    for card in cards {
        let Some(l) = lanes.iter_mut().find(|l| l.lane.id == card.card.lane_id) else {
            continue;
        };
        let (kind, origin) = (l.lane.kind, card.card.origin);
        if origin == Origin::Discussing && !kind.is_closed() {
            discussing_count += 1;
        }
        let counts = match card.card.epic_id {
            Some(id) => epics
                .iter_mut()
                .find(|e| e.epic.id == id)
                .map(|e| &mut e.counts),
            None => Some(no_epic.get_or_insert_with(EpicCounts::default)),
        };
        if let Some(counts) = counts {
            counts.add(kind, origin);
        }
        l.cards.push(card);
    }
    for l in &mut lanes {
        default_card_order(l);
    }
    let open_question_count = open_question_count(conn, board_id).await?;
    Ok(Some(BoardSnapshot {
        workspace,
        board,
        lanes,
        open_question_count,
        epics,
        no_epic,
        discussing_count,
        unreplied_comment_count,
    }))
}

/// Puts a lane's cards in the default display order (docs/board-v2.md sections 10.2 and
/// 16.3): `todo` lanes by `position` with blocked cards after the others; `doing` lanes by
/// `position`; `done` and `dropped` lanes by the time the card entered the lane
/// (`closed_at`), newest first, then `position` and number descending. Cards without
/// `closed_at` there (which should not happen) count as the oldest.
///
/// [`board_snapshot`] applies it; the TUI applies it again when it returns from another
/// display order.
pub fn default_card_order(lane: &mut LaneWithCards) {
    let by_position = |a: &CardSummary, b: &CardSummary| {
        (a.card.position, a.card.seq).cmp(&(b.card.position, b.card.seq))
    };
    match lane.lane.kind {
        LaneKind::Todo => lane.cards.sort_by(|a, b| {
            a.is_blocked()
                .cmp(&b.is_blocked())
                .then_with(|| by_position(a, b))
        }),
        LaneKind::Doing => lane.cards.sort_by(by_position),
        LaneKind::Done | LaneKind::Dropped => lane.cards.sort_by(|a, b| {
            b.card
                .closed_at
                .cmp(&a.card.closed_at)
                .then_with(|| by_position(b, a))
        }),
    }
}

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: i64,
    kind: LinkKind,
    outgoing: bool,
    other_id: Option<i64>,
    other_seq: Option<i32>,
    other_title: Option<String>,
    target: Option<String>,
    label: Option<String>,
    created_at: DateTime<Utc>,
}

/// One card with its lane, notes (oldest first) and links in both directions.
/// `None` when the board has no card with number `seq`.
pub async fn card_detail(
    conn: &mut PgConnection,
    board_id: i64,
    seq: i32,
) -> Result<Option<CardDetail>> {
    let Some(card) =
        sqlx::query_as::<_, Card>("SELECT * FROM card WHERE board_id = $1 AND seq = $2")
            .bind(board_id)
            .bind(seq)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(None);
    };
    let lane = sqlx::query_as::<_, Lane>("SELECT * FROM lane WHERE id = $1")
        .bind(card.lane_id)
        .fetch_one(&mut *conn)
        .await?;
    let epic_name: Option<String> = sqlx::query_scalar("SELECT name FROM epic WHERE id = $1")
        .bind(card.epic_id)
        .fetch_optional(&mut *conn)
        .await?;
    let mut notes =
        sqlx::query_as::<_, Note>("SELECT * FROM card_note WHERE card_id = $1 ORDER BY id")
            .bind(card.id)
            .fetch_all(&mut *conn)
            .await?;
    let (result_because, led_to) = if has_cause_table(conn).await? {
        let forward = causes_of_card(conn, card.id).await?;
        // The current result comes from the latest drop or done of the card.
        let last_close: Option<i64> = sqlx::query_scalar(
            "SELECT max(id) FROM activity
             WHERE card_id = $1 AND verb IN ('card.drop', 'card.done')",
        )
        .bind(card.id)
        .fetch_one(&mut *conn)
        .await?;
        let mut result_because = Vec::new();
        for (note_id, activity_id, cause) in forward {
            match note_id {
                Some(id) => {
                    if let Some(n) = notes.iter_mut().find(|n| n.id == id) {
                        n.because.push(cause);
                    }
                }
                None if Some(activity_id) == last_close => result_because.push(cause),
                None => {}
            }
        }
        (result_because, consequences_of_card(conn, card.id).await?)
    } else {
        (Vec::new(), Vec::new())
    };
    let rows = sqlx::query_as::<_, LinkRow>(
        "SELECT l.id, l.kind, l.from_card_id = $1 AS outgoing,
                o.id AS other_id, o.seq AS other_seq, o.title AS other_title,
                l.target, l.label, l.created_at
         FROM link l
         LEFT JOIN card o
           ON o.id = CASE WHEN l.from_card_id = $1 THEN l.to_card_id ELSE l.from_card_id END
         WHERE l.from_card_id = $1 OR l.to_card_id = $1
         ORDER BY l.id",
    )
    .bind(card.id)
    .fetch_all(&mut *conn)
    .await?;
    let links = rows
        .into_iter()
        .map(|r| LinkView {
            id: r.id,
            kind: r.kind,
            direction: if r.outgoing {
                LinkDirection::Outgoing
            } else {
                LinkDirection::Incoming
            },
            card: match (r.other_id, r.other_seq, r.other_title) {
                (Some(id), Some(seq), Some(title)) => Some(LinkedCard { id, seq, title }),
                _ => None,
            },
            target: r.target,
            label: r.label,
            created_at: r.created_at,
        })
        .collect();
    let questions = load_questions(
        conn,
        "SELECT q.*, c.seq AS card_seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.card_id = $1
         ORDER BY q.status = 'open' DESC, q.id",
        card.id,
    )
    .await?;
    let unreplied_comments = notes
        .iter()
        .filter(|n| n.kind == NoteKind::Comment)
        .filter(|c| {
            !notes
                .iter()
                .any(|r| r.kind == NoteKind::Reply && r.id > c.id)
        })
        .map(|c| c.id)
        .collect();
    Ok(Some(CardDetail {
        card,
        lane,
        epic_name,
        notes,
        links,
        questions,
        unreplied_comments,
        result_because,
        led_to,
    }))
}

/// True when the schema has the `cause` table (version 7). Older schemas read as "no
/// causes" (docs/board-v2.md section 18.4).
async fn has_cause_table(conn: &mut PgConnection) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT to_regclass('public.cause') IS NOT NULL")
            .fetch_one(&mut *conn)
            .await?,
    )
}

#[derive(sqlx::FromRow)]
struct CauseRow {
    note_id: Option<i64>,
    activity_id: i64,
    kind: CauseKind,
    target_id: Option<i64>,
    card_seq: Option<i32>,
    card_title: Option<String>,
}

/// The causes of every decision of card `card_id`, in insertion order:
/// (note id, activity id, cause).
async fn causes_of_card(
    conn: &mut PgConnection,
    card_id: i64,
) -> Result<Vec<(Option<i64>, i64, Cause)>> {
    let rows = sqlx::query_as::<_, CauseRow>(
        "SELECT x.note_id, x.activity_id, x.kind,
                coalesce(x.cause_question_id, x.cause_note_id) AS target_id,
                h.seq AS card_seq, h.title AS card_title
         FROM cause x
         LEFT JOIN question q ON q.id = x.cause_question_id
         LEFT JOIN card_note cn ON cn.id = x.cause_note_id
         LEFT JOIN card h ON h.id = coalesce(x.cause_card_id, q.card_id, cn.card_id)
         WHERE x.card_id = $1
         ORDER BY x.id",
    )
    .bind(card_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.note_id,
                r.activity_id,
                Cause {
                    kind: r.kind,
                    id: r.target_id,
                    card_seq: r.card_seq,
                    card_title: r.card_title,
                },
            )
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct ConsequenceRow {
    via: CauseKind,
    via_id: Option<i64>,
    card_seq: i32,
    card_title: String,
    verb: String,
    note_id: Option<i64>,
    note_kind: Option<NoteKind>,
    text: String,
    at: DateTime<Utc>,
}

/// Decisions citing card `card_id`, one of its questions or one of its comments, oldest
/// first. `user` causes have no reverse direction.
async fn consequences_of_card(conn: &mut PgConnection, card_id: i64) -> Result<Vec<Consequence>> {
    let rows = sqlx::query_as::<_, ConsequenceRow>(
        "SELECT x.kind AS via, coalesce(x.cause_question_id, x.cause_note_id) AS via_id,
                d.seq AS card_seq, d.title AS card_title, a.verb,
                x.note_id, n.kind AS note_kind,
                coalesce(n.body, a.detail->>'reason', a.detail->>'result', '') AS text,
                a.at
         FROM cause x
         JOIN activity a ON a.id = x.activity_id
         JOIN card d ON d.id = x.card_id
         LEFT JOIN card_note n ON n.id = x.note_id
         LEFT JOIN question q ON q.id = x.cause_question_id
         LEFT JOIN card_note cn ON cn.id = x.cause_note_id
         WHERE x.cause_card_id = $1 OR q.card_id = $1 OR cn.card_id = $1
         ORDER BY x.activity_id, x.id",
    )
    .bind(card_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Consequence {
            via: r.via,
            via_id: r.via_id,
            card_seq: r.card_seq,
            card_title: r.card_title,
            decision: match r.verb.as_str() {
                "card.drop" => DecisionKind::Drop,
                "card.done" => DecisionKind::Done,
                _ => DecisionKind::Note,
            },
            note_id: r.note_id,
            note_kind: r.note_kind,
            text: r.text,
            at: r.at,
        })
        .collect())
}

/// The newest `limit` activity entries of a board, newest first.
pub async fn recent_activity(
    conn: &mut PgConnection,
    board_id: i64,
    limit: i64,
) -> Result<Vec<Activity>> {
    Ok(sqlx::query_as::<_, Activity>(
        "SELECT * FROM activity WHERE board_id = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(board_id)
    .bind(limit.max(0))
    .fetch_all(&mut *conn)
    .await?)
}

/// The newest `limit` activity entries of a board without `tool.use` and without the
/// `judge.run` rows that have no `error`, newest first: the TUI's live feed (docs/tui.md
/// sections 1 and 12.1, docs/board-v2.md section 5.2).
pub async fn recent_feed(
    conn: &mut PgConnection,
    board_id: i64,
    limit: i64,
) -> Result<Vec<Activity>> {
    Ok(sqlx::query_as::<_, Activity>(
        "SELECT * FROM activity WHERE board_id = $1 AND verb <> 'tool.use'
           AND (verb <> 'judge.run' OR detail ? 'error')
         ORDER BY id DESC LIMIT $2",
    )
    .bind(board_id)
    .bind(limit.max(0))
    .fetch_all(&mut *conn)
    .await?)
}

/// The model-judge questions the TUI shows as tags (docs/tui.md section 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeTag {
    /// `waiting`: the agent ended its last message waiting for the user's reply in the chat.
    Waiting,
    /// `advice`: the agent's last message recommends or decides something about a card that
    /// the card does not record.
    Advice,
}

/// A `judge.finding` of question `waiting` or `advice` that is still active: the latest one
/// per card and question, when nothing after it settled it (docs/tui.md section 12.5). Read
/// for the TUI only; it is not part of [`BoardSnapshot`] or [`CardDetail`], which the CLI
/// prints for the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveFinding {
    /// The `judge.finding` row.
    pub id: i64,
    /// The card; `None` for a board-level finding.
    pub card_id: Option<i64>,
    pub tag: JudgeTag,
    /// `detail.p`.
    pub p: f64,
}

#[derive(sqlx::FromRow)]
struct ActiveFindingRow {
    id: i64,
    card_id: Option<i64>,
    question: String,
    p: f64,
}

/// The active `waiting` and `advice` findings of a board, oldest first (docs/tui.md
/// section 12.5). Per card (or the board) and question only the latest finding counts. It
/// is active while:
///
/// - `waiting`: no `user.prompt` of `kb hook prompt` (actor `user`; the one of
///   `kb board start` has `detail.source = "board.start"`) came after it;
/// - `advice`: the card has no agent row after it (actor `agent`, other than the delivery
///   of answers and comments).
pub async fn active_judge_findings(
    conn: &mut PgConnection,
    board_id: i64,
) -> Result<Vec<ActiveFinding>> {
    let rows = sqlx::query_as::<_, ActiveFindingRow>(
        "SELECT f.id, f.card_id, f.detail->>'question' AS question,
                CASE WHEN jsonb_typeof(f.detail->'p') = 'number'
                     THEN (f.detail->>'p')::float8 ELSE 0 END AS p
         FROM (SELECT DISTINCT ON (card_id, detail->>'question') id, card_id, detail
               FROM activity
               WHERE board_id = $1 AND verb = 'judge.finding'
                 AND detail->>'question' IN ('waiting', 'advice')
               ORDER BY card_id, detail->>'question', id DESC) f
         WHERE CASE f.detail->>'question'
                 WHEN 'waiting' THEN NOT EXISTS (
                   SELECT 1 FROM activity u
                   WHERE u.board_id = $1 AND u.id > f.id AND u.verb = 'user.prompt'
                     AND u.actor = 'user'
                     AND u.detail->>'source' IS DISTINCT FROM 'board.start')
                 ELSE f.card_id IS NOT NULL AND NOT EXISTS (
                   SELECT 1 FROM activity w
                   WHERE w.board_id = $1 AND w.id > f.id AND w.card_id = f.card_id
                     AND w.actor = 'agent'
                     AND w.verb NOT IN ('question.deliver', 'comment.deliver'))
               END
         ORDER BY f.id",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let tag = match r.question.as_str() {
                "waiting" => JudgeTag::Waiting,
                "advice" => JudgeTag::Advice,
                _ => return None,
            };
            Some(ActiveFinding {
                id: r.id,
                card_id: r.card_id,
                tag,
                p: r.p,
            })
        })
        .collect())
}

/// How a recorded rule failure ended (docs/board-v2.md section 16.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// `check.block`: the hook blocked.
    Blocked,
    /// `check.warn` of a rule at level `warn`.
    Warned,
    /// `check.warn` written after a block in the same turn (`after_block`) for a rule at
    /// level `block`: it would have blocked.
    NotEnforced,
}

impl CheckOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckOutcome::Blocked => "blocked",
            CheckOutcome::Warned => "warned",
            CheckOutcome::NotEnforced => "not enforced",
        }
    }
}

/// One past rule failure: a `check.warn` or `check.block` row (`kb board check --history`).
#[derive(Debug, Clone, Serialize)]
pub struct CheckRecord {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub turn_key: Option<String>,
    pub rule: String,
    pub outcome: CheckOutcome,
    /// The rule's level when the row has it (`after_block` rows).
    pub level: Option<String>,
    pub after_block: bool,
    /// The English sentence for the agent; empty on rows written before section 14.2.
    pub text: String,
    pub fix: String,
    /// The Traditional Chinese explanation.
    pub message: String,
}

impl CheckRecord {
    fn from_activity(a: Activity) -> CheckRecord {
        let s = |k: &str| {
            a.detail
                .get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let level = a
            .detail
            .get("level")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let after_block = a
            .detail
            .get("after_block")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let outcome = if a.verb == "check.block" {
            CheckOutcome::Blocked
        } else if after_block && level.as_deref() == Some("block") {
            CheckOutcome::NotEnforced
        } else {
            CheckOutcome::Warned
        };
        CheckRecord {
            id: a.id,
            at: a.at,
            turn_key: a.turn_key,
            rule: s("rule"),
            outcome,
            level,
            after_block,
            text: s("text"),
            fix: s("fix"),
            message: s("message"),
        }
    }
}

/// The newest `limit` rule failures of a board (`check.warn` and `check.block`), newest
/// first (docs/board-v2.md section 16.2).
pub async fn check_history(
    conn: &mut PgConnection,
    board_id: i64,
    limit: i64,
) -> Result<Vec<CheckRecord>> {
    let rows = sqlx::query_as::<_, Activity>(
        "SELECT * FROM activity
         WHERE board_id = $1 AND verb IN ('check.warn', 'check.block')
         ORDER BY id DESC LIMIT $2",
    )
    .bind(board_id)
    .bind(limit.max(0))
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(CheckRecord::from_activity).collect())
}

// ---------------------------------------------------------------------------------------
// Questions (docs/questions.md)

#[derive(sqlx::FromRow)]
struct QuestionRow {
    #[sqlx(flatten)]
    question: Question,
    card_seq: i32,
}

/// Runs `sql` (one `$1` parameter, selecting `q.*` and `card_seq`) and attaches options.
async fn load_questions(
    conn: &mut PgConnection,
    sql: &'static str,
    param: i64,
) -> Result<Vec<QuestionView>> {
    let rows = sqlx::query_as::<_, QuestionRow>(sql)
        .bind(param)
        .fetch_all(&mut *conn)
        .await?;
    attach_options(conn, rows).await
}

async fn attach_options(
    conn: &mut PgConnection,
    rows: Vec<QuestionRow>,
) -> Result<Vec<QuestionView>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.question.id).collect();
    let options = sqlx::query_as::<_, QuestionOption>(
        "SELECT * FROM question_option WHERE question_id = ANY($1) ORDER BY question_id, position",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| QuestionView {
            options: options
                .iter()
                .filter(|o| o.question_id == r.question.id)
                .cloned()
                .collect(),
            card_seq: r.card_seq,
            question: r.question,
        })
        .collect())
}

/// Number of `open` questions on a board.
pub async fn open_question_count(conn: &mut PgConnection, board_id: i64) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM question WHERE board_id = $1 AND status = 'open'")
            .bind(board_id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// All questions of a board, oldest first (`open_only`: only `open` ones).
pub async fn board_questions(
    conn: &mut PgConnection,
    board_id: i64,
    open_only: bool,
) -> Result<Vec<QuestionView>> {
    let sql = if open_only {
        "SELECT q.*, c.seq AS card_seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.board_id = $1 AND q.status = 'open' ORDER BY q.id"
    } else {
        "SELECT q.*, c.seq AS card_seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.board_id = $1 ORDER BY q.id"
    };
    load_questions(conn, sql, board_id).await
}

/// One question of a board by id; `None` when the board has no such question.
pub async fn question(
    conn: &mut PgConnection,
    board_id: i64,
    question_id: i64,
) -> Result<Option<QuestionView>> {
    let rows = sqlx::query_as::<_, QuestionRow>(
        "SELECT q.*, c.seq AS card_seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.board_id = $1 AND q.id = $2",
    )
    .bind(board_id)
    .bind(question_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(attach_options(conn, rows).await?.into_iter().next())
}

/// Answered questions (`answered` or `chat`) the agent has not received yet, in the order
/// they were answered.
pub async fn undelivered_answers(
    conn: &mut PgConnection,
    board_id: i64,
) -> Result<Vec<QuestionView>> {
    load_questions(
        conn,
        "SELECT q.*, c.seq AS card_seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.board_id = $1 AND q.status IN ('answered', 'chat') AND q.delivered_at IS NULL
         ORDER BY q.answered_at, q.id",
        board_id,
    )
    .await
}

/// Answered questions delivered at or after `since` (the turn's start), in the order they
/// were answered (docs/board-v2.md section 19.2).
pub async fn answers_delivered_since(
    conn: &mut PgConnection,
    board_id: i64,
    since: DateTime<Utc>,
) -> Result<Vec<QuestionView>> {
    let rows = sqlx::query_as::<_, QuestionRow>(
        "SELECT q.*, c.seq AS card_seq FROM question q JOIN card c ON c.id = q.card_id
         WHERE q.board_id = $1 AND q.status IN ('answered', 'chat') AND q.delivered_at >= $2
         ORDER BY q.answered_at, q.id",
    )
    .bind(board_id)
    .bind(since)
    .fetch_all(&mut *conn)
    .await?;
    attach_options(conn, rows).await
}

// ---------------------------------------------------------------------------------------
// Comments (docs/board-v2.md section 13)

/// Comments delivered at or after `since` (the turn's start), oldest first
/// (docs/board-v2.md section 19.2).
pub async fn comments_delivered_since(
    conn: &mut PgConnection,
    board_id: i64,
    since: DateTime<Utc>,
) -> Result<Vec<CommentView>> {
    Ok(sqlx::query_as::<_, CommentView>(
        "SELECT n.*, c.seq AS card_seq, c.title AS card_title
         FROM card_note n JOIN card c ON c.id = n.card_id
         WHERE c.board_id = $1 AND n.kind = 'comment' AND n.delivered_at >= $2
         ORDER BY n.id",
    )
    .bind(board_id)
    .bind(since)
    .fetch_all(&mut *conn)
    .await?)
}

/// Comments on a board that the agent has not received yet, oldest first.
pub async fn undelivered_comments(
    conn: &mut PgConnection,
    board_id: i64,
) -> Result<Vec<CommentView>> {
    Ok(sqlx::query_as::<_, CommentView>(
        "SELECT n.*, c.seq AS card_seq, c.title AS card_title
         FROM card_note n JOIN card c ON c.id = n.card_id
         WHERE c.board_id = $1 AND n.kind = 'comment' AND n.delivered_at IS NULL
         ORDER BY n.id",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Comments on a board without a newer `reply` note on the same card, by card number, then
/// oldest first (docs/board-v2.md section 13.9).
pub async fn unreplied_comments(
    conn: &mut PgConnection,
    board_id: i64,
) -> Result<Vec<CommentView>> {
    Ok(sqlx::query_as::<_, CommentView>(
        "SELECT n.*, c.seq AS card_seq, c.title AS card_title
         FROM card_note n JOIN card c ON c.id = n.card_id
         WHERE c.board_id = $1 AND n.kind = 'comment'
           AND NOT EXISTS (SELECT 1 FROM card_note r
                            WHERE r.card_id = n.card_id AND r.kind = 'reply' AND r.id > n.id)
         ORDER BY c.seq, n.id",
    )
    .bind(board_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The most recently changed active board whose `herdr_terminal` is `terminal`
/// (session key source 4, docs/hooks.md section 5.3).
pub async fn active_board_by_terminal(
    conn: &mut PgConnection,
    terminal: &str,
) -> Result<Option<Board>> {
    Ok(sqlx::query_as::<_, Board>(
        "SELECT * FROM board WHERE herdr_terminal = $1 AND status = 'active'
         ORDER BY updated_at DESC, id DESC LIMIT 1",
    )
    .bind(terminal)
    .fetch_optional(&mut *conn)
    .await?)
}
