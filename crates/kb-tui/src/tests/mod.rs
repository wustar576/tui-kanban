//! Tests without a terminal or a database: state transitions (`update`) and rendering
//! through ratatui's `TestBackend` / `Buffer`, with Chinese content.

mod render;
mod state;
mod v10;
mod v11;
mod v12;
mod v2_render;
mod v2_state;
mod v3;
mod v4;
mod v5;
mod v6;
mod v7;
mod v8;
mod v9;

use std::time::Instant;

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use kb_core::model::{
    Activity, Board, BoardStatus, Card, Epic, Lane, LaneKind, LinkKind, Note, NoteKind, Origin,
    Question, QuestionOption, QuestionStatus, Workspace, WorkspaceSource,
};
use kb_core::read::{
    BoardSnapshot, CardDetail, CardSummary, EpicCounts, EpicSummary, LaneWithCards, LinkDirection,
    LinkView, LinkedCard, QuestionView,
};

use crate::app::{App, BoardData, BoardTarget, Effect, Event, Key, Scope};

pub const BOARD_ID: i64 = 1;

pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 2, 30, 0).unwrap()
}

pub fn at(min: i64) -> DateTime<Utc> {
    t0() + Duration::minutes(min)
}

pub fn today() -> NaiveDate {
    t0().with_timezone(&Local).date_naive()
}

pub fn workspace() -> Workspace {
    Workspace {
        id: 10,
        key: "manual:Acme_DevOps".into(),
        name: "Acme_DevOps".into(),
        source: WorkspaceSource::Manual,
        created_at: t0(),
        updated_at: t0(),
    }
}

pub fn board(goal: &str) -> Board {
    Board {
        id: BOARD_ID,
        workspace_id: 10,
        session_key: "test".into(),
        title: "修正登入逾時".into(),
        goal: goal.into(),
        status: BoardStatus::Active,
        outcome: None,
        summary: None,
        terminal_space: None,
        terminal_tab: None,
        terminal_id: None,
        cwd: None,
        prev_board_id: None,
        next_card_seq: 1,
        rev: 1,
        created_at: t0(),
        updated_at: t0(),
        archived_at: None,
        turn_key: None,
        turn_started_at: None,
    }
}

pub fn lane(id: i64, position: i32, name: &str, kind: LaneKind) -> Lane {
    Lane {
        id,
        board_id: BOARD_ID,
        name: name.into(),
        position,
        kind,
        wip_limit: None,
    }
}

/// Card ids are `100 + seq`.
pub fn card(seq: i32, lane_id: i64, title: &str, priority: i16) -> CardSummary {
    CardSummary {
        card: Card {
            id: 100 + seq as i64,
            board_id: BOARD_ID,
            seq,
            lane_id,
            title: title.into(),
            body: String::new(),
            priority,
            position: seq,
            owner: "agent".into(),
            blocked_reason: None,
            result: None,
            created_at: at(seq as i64),
            updated_at: at(seq as i64),
            closed_at: None,
            epic_id: None,
            origin: Origin::Agreed,
        },
        note_count: 0,
        link_count: 0,
        open_question_count: 0,
        epic_name: None,
        spawned_from: None,
        spawned_count: 0,
        blocked_by: Vec::new(),
        unreplied_comment_count: 0,
    }
}

pub fn snapshot(goal: &str, lanes: Vec<(Lane, Vec<CardSummary>)>) -> BoardSnapshot {
    BoardSnapshot {
        workspace: workspace(),
        board: board(goal),
        lanes: lanes
            .into_iter()
            .map(|(lane, mut cards)| {
                for c in &mut cards {
                    c.card.lane_id = lane.id;
                }
                LaneWithCards { lane, cards }
            })
            .collect(),
        open_question_count: 0,
        epics: Vec::new(),
        no_epic: None,
        discussing_count: 0,
        unreplied_comment_count: 0,
    }
}

pub fn activity(id: i64, verb: &str, detail: serde_json::Value) -> Activity {
    Activity {
        id,
        board_id: BOARD_ID,
        card_id: None,
        at: at(id),
        actor: "agent".into(),
        verb: verb.into(),
        detail,
        turn_key: None,
    }
}

/// Lanes 待辦 (#1, #2), 實作 (#3), 完成 (#4), 捨棄 (empty).
pub fn demo_snapshot() -> BoardSnapshot {
    let mut c4 = card(4, 0, "重現登入逾時的問題", 1);
    c4.note_count = 1;
    let mut c3 = card(3, 0, "修正 token 更新時間", 1);
    c3.card.blocked_reason = Some("等待確認 API 行為".into());
    c3.link_count = 2;
    c3.note_count = 3;
    snapshot(
        "找到並修正登入逾時的原因",
        vec![
            (
                lane(1, 1, "待辦", LaneKind::Todo),
                vec![card(1, 0, "補上逾時測試", 2), card(2, 0, "更新文件", 3)],
            ),
            (lane(2, 2, "實作", LaneKind::Doing), vec![c3]),
            (lane(3, 3, "完成", LaneKind::Done), vec![c4]),
            (lane(4, 4, "捨棄", LaneKind::Dropped), vec![]),
        ],
    )
}

pub fn data(snapshot: BoardSnapshot) -> BoardData {
    BoardData {
        snapshot,
        activity: vec![
            activity(
                3,
                "card.note",
                serde_json::json!({"seq": 3, "kind": "decision", "excerpt": "改用滑動視窗"}),
            ),
            activity(
                2,
                "card.move",
                serde_json::json!({"seq": 3, "from": "待辦", "to": "實作"}),
            ),
            activity(
                1,
                "link.add",
                serde_json::json!({"seq": 3, "kind": "spawned", "to_seq": 1}),
            ),
        ],
        findings: Vec::new(),
    }
}

/// An app showing `snapshot` at `size`.
pub fn app_with(snapshot: BoardSnapshot, size: (u16, u16)) -> App {
    let mut app = App::new(size, today(), Scope::All, Some(BOARD_ID));
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(BOARD_ID),
        data: Some(data(snapshot)),
        at: Instant::now(),
    });
    app
}

pub fn press(app: &mut App, keys: &[Key]) -> Vec<Effect> {
    let mut fx = Vec::new();
    for k in keys {
        fx.extend(app.update(Event::Key(*k)));
    }
    fx
}

pub fn selected_seq(app: &App) -> Option<i32> {
    app.selected_card().map(|c| c.card.seq)
}

pub fn note(id: i64, kind: NoteKind, body: &str) -> Note {
    Note {
        id,
        card_id: 0,
        kind,
        body: body.into(),
        author: "agent".into(),
        created_at: at(id),
        delivered_at: None,
        because: Vec::new(),
    }
}

pub fn card_link(id: i64, kind: LinkKind, dir: LinkDirection, seq: i32, title: &str) -> LinkView {
    LinkView {
        id,
        kind,
        direction: dir,
        card: Some(LinkedCard {
            id: 100 + seq as i64,
            seq,
            title: title.into(),
        }),
        target: None,
        label: None,
        created_at: at(id),
    }
}

pub fn ref_link(id: i64, target: &str) -> LinkView {
    LinkView {
        id,
        kind: LinkKind::Ref,
        direction: LinkDirection::Outgoing,
        card: None,
        target: Some(target.into()),
        label: None,
        created_at: at(id),
    }
}

/// Detail of card `seq` from the demo board, with the given notes and links.
pub fn detail(seq: i32, body: &str, notes: Vec<Note>, links: Vec<LinkView>) -> CardDetail {
    let snap = demo_snapshot();
    let (lane, c) = snap
        .card(seq)
        .map(|(l, c)| (l.clone(), c.card.clone()))
        .unwrap_or_else(|| {
            let c = card(seq, 2, &format!("卡片 {seq}"), 2);
            (lane(2, 2, "實作", LaneKind::Doing), c.card)
        });
    let mut card = c;
    card.body = body.into();
    CardDetail {
        card,
        lane,
        epic_name: None,
        notes,
        links,
        questions: Vec::new(),
        unreplied_comments: Vec::new(),
        result_because: Vec::new(),
        led_to: Vec::new(),
    }
}

/// `_summary` is ignored: epics no longer carry a summary (docs/board-v2.md 10.1).
pub fn epic(id: i64, name: &str, _summary: Option<&str>, counts: EpicCounts) -> EpicSummary {
    EpicSummary {
        epic: Epic {
            id,
            board_id: BOARD_ID,
            name: name.into(),
            position: id as i32,
            created_at: t0(),
        },
        counts,
    }
}

pub fn counts(total: i64, done: i64, derived: i64) -> EpicCounts {
    EpicCounts {
        total,
        done,
        derived,
        discussing: 0,
    }
}

/// A question on card `seq`; option ids are `id * 10 + position`.
pub fn question(id: i64, seq: i32, text: &str, multi: bool, labels: &[&str]) -> QuestionView {
    QuestionView {
        question: Question {
            id,
            board_id: BOARD_ID,
            card_id: 100 + seq as i64,
            text: text.into(),
            multi,
            status: QuestionStatus::Open,
            asked_by: "agent".into(),
            asked_at: at(id),
            answered_at: None,
            delivered_at: None,
            answer_text: None,
        },
        card_seq: seq,
        options: labels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let (label, description) = match l.split_once("::") {
                    Some((a, b)) => (a.to_string(), Some(b.to_string())),
                    None => (l.to_string(), None),
                };
                QuestionOption {
                    id: id * 10 + i as i64 + 1,
                    question_id: id,
                    position: i as i32 + 1,
                    label,
                    description,
                    selected: false,
                }
            })
            .collect(),
    }
}

pub fn deliver_card(app: &mut App, d: CardDetail) {
    let seq = d.card.seq;
    app.update(Event::CardLoaded {
        board_id: BOARD_ID,
        seq,
        detail: Some(d),
    });
}
