//! What the TUI reads of the model judge (docs/tui.md section 12): the active `waiting`
//! and `advice` findings and the live feed without quiet `judge.run` rows. Rows are
//! inserted as `kb hook judge`, `kb hook prompt`, `kb board start` and the CLI write them;
//! reads go through the reader role. Database: `kanban_test` only.

mod common;

use std::path::Path;

use kb_core::read::{self, ActiveFinding, JudgeTag};
use kb_core::write::{self, BoardInit, CardAdd};
use kb_core::{PgConnection, resolve};
use serde_json::{Value, json};

async fn new_board(conn: &mut PgConnection) -> (String, i64) {
    let session = common::unique("judge-read");
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    let args = BoardInit {
        title: Some("評測".into()),
        goal: None,
        lanes: Some(write::parse_lanes("待辦:todo,實作:doing,完成:done,捨棄:dropped").unwrap()),
        epics: None,
        cwd: None,
    };
    let out = write::board_init(conn, &session, args, || ws)
        .await
        .expect("board init");
    (session, out.board_id)
}

async fn add_card(conn: &mut PgConnection, session: &str, title: &str) -> i64 {
    let seq = write::card_add(conn, session, CardAdd::new("待辦", title))
        .await
        .expect("card add")
        .card
        .unwrap();
    sqlx::query_scalar(
        "SELECT c.id FROM card c JOIN board b ON b.id = c.board_id
         WHERE b.session_key = $1 AND c.seq = $2",
    )
    .bind(session)
    .bind(seq)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

/// Inserts one activity row and returns its id.
async fn row(
    conn: &mut PgConnection,
    board: i64,
    card: Option<i64>,
    actor: &str,
    verb: &str,
    detail: Value,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO activity (board_id, card_id, actor, verb, detail)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(board)
    .bind(card)
    .bind(actor)
    .bind(verb)
    .bind(sqlx::types::Json(detail))
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

/// A `judge.finding` as `kb hook judge` writes it.
async fn finding(
    conn: &mut PgConnection,
    board: i64,
    card: Option<i64>,
    question: &str,
    p: f64,
) -> i64 {
    let detail = json!({"question": question, "p": p, "band": "record", "woke": false,
                        "text": "card #1 ...", "message": "#1 ..."});
    row(conn, board, card, "hook", "judge.finding", detail).await
}

/// `kb hook prompt`: a user message in the chat.
async fn user_prompt(conn: &mut PgConnection, board: i64) -> i64 {
    let detail = json!({"excerpt": "好", "text": "好"});
    row(conn, board, None, "user", "user.prompt", detail).await
}

async fn active(board: i64) -> Vec<(Option<i64>, JudgeTag, f64)> {
    let mut reader = common::reader().await;
    read::active_judge_findings(&mut reader, board)
        .await
        .unwrap()
        .into_iter()
        .map(|f: ActiveFinding| (f.card_id, f.tag, f.p))
        .collect()
}

#[tokio::test]
async fn waiting_is_active_until_the_next_user_message() {
    let mut conn = common::writer().await;
    let (session, board) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &session, "登入逾時").await;
    finding(&mut conn, board, Some(c1), "waiting", 0.93).await;
    finding(&mut conn, board, None, "waiting", 0.6).await;
    assert_eq!(
        active(board).await,
        [
            (Some(c1), JudgeTag::Waiting, 0.93),
            (None, JudgeTag::Waiting, 0.6)
        ]
    );

    // The user.prompt of `kb board start` is not a user message (docs/board-v2.md 12.4).
    let start = json!({"excerpt": "kb board start", "source": "board.start"});
    row(
        &mut conn,
        board,
        None,
        "agent",
        "user.prompt",
        start.clone(),
    )
    .await;
    // Nor is one with that source whatever its actor.
    row(&mut conn, board, None, "user", "user.prompt", start).await;
    // Nor are rows by the hooks or the agent, nor an answer in the TUI.
    row(&mut conn, board, None, "hook", "turn.summary", json!({})).await;
    row(
        &mut conn,
        board,
        Some(c1),
        "agent",
        "card.note",
        json!({"seq": 1, "kind": "progress", "excerpt": "x"}),
    )
    .await;
    row(
        &mut conn,
        board,
        Some(c1),
        "user",
        "comment.add",
        json!({"seq": 1, "excerpt": "x"}),
    )
    .await;
    assert_eq!(active(board).await.len(), 2);

    // The next message of `kb hook prompt` settles both.
    user_prompt(&mut conn, board).await;
    assert!(active(board).await.is_empty());

    // A later finding is active again.
    finding(&mut conn, board, Some(c1), "waiting", 0.7).await;
    assert_eq!(active(board).await, [(Some(c1), JudgeTag::Waiting, 0.7)]);
}

#[tokio::test]
async fn advice_is_active_until_the_agent_writes_on_the_card() {
    let mut conn = common::writer().await;
    let (session, board) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &session, "登入逾時").await;
    let c2 = add_card(&mut conn, &session, "加測試").await;
    finding(&mut conn, board, Some(c1), "advice", 0.82).await;
    finding(&mut conn, board, Some(c2), "advice", 0.4).await;

    // Rows by the hooks or the user on the card, a user message, an agent write on
    // another card, and the delivery of answers and comments do not settle it.
    row(
        &mut conn,
        board,
        Some(c1),
        "hook",
        "check.warn",
        json!({"rule": "R3"}),
    )
    .await;
    row(
        &mut conn,
        board,
        Some(c1),
        "user",
        "comment.add",
        json!({"seq": 1, "excerpt": "x"}),
    )
    .await;
    user_prompt(&mut conn, board).await;
    row(
        &mut conn,
        board,
        Some(c2),
        "agent",
        "card.note",
        json!({"seq": 2, "kind": "progress", "excerpt": "x"}),
    )
    .await;
    row(
        &mut conn,
        board,
        Some(c1),
        "agent",
        "comment.deliver",
        json!({"seq": 1, "note": 1}),
    )
    .await;
    row(
        &mut conn,
        board,
        Some(c1),
        "agent",
        "question.deliver",
        json!({"seq": 1, "question": 1}),
    )
    .await;
    assert_eq!(active(board).await, [(Some(c1), JudgeTag::Advice, 0.82)]);

    // An agent write on the card settles it.
    row(
        &mut conn,
        board,
        Some(c1),
        "agent",
        "card.note",
        json!({"seq": 1, "kind": "decision", "excerpt": "先查 log"}),
    )
    .await;
    assert!(active(board).await.is_empty());
}

#[tokio::test]
async fn only_the_latest_finding_per_card_and_question_counts() {
    let mut conn = common::writer().await;
    let (session, board) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &session, "登入逾時").await;
    finding(&mut conn, board, Some(c1), "advice", 0.4).await;
    finding(&mut conn, board, Some(c1), "advice", 0.75).await;
    finding(&mut conn, board, Some(c1), "waiting", 0.6).await;
    // offcard_work and unconfirmed_work have no tag.
    finding(&mut conn, board, None, "offcard_work", 0.9).await;
    finding(&mut conn, board, Some(c1), "unconfirmed_work", 0.9).await;
    assert_eq!(
        active(board).await,
        [
            (Some(c1), JudgeTag::Advice, 0.75),
            (Some(c1), JudgeTag::Waiting, 0.6)
        ]
    );
}

#[tokio::test]
async fn the_feed_leaves_out_judge_runs_without_an_error() {
    let mut conn = common::writer().await;
    let (session, board) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &session, "登入逾時").await;
    let run = |error: Option<&str>| {
        let mut d = json!({"questions": 4, "findings": 1, "ms": 900, "input_chars": 300,
                           "model": "gemini-decision-1"});
        if let Some(e) = error {
            d["error"] = json!(e);
        }
        d
    };
    let f = finding(&mut conn, board, Some(c1), "advice", 0.8).await;
    let quiet = row(&mut conn, board, None, "hook", "judge.run", run(None)).await;
    let failed = row(
        &mut conn,
        board,
        None,
        "hook",
        "judge.run",
        run(Some("timeout after 8000 ms")),
    )
    .await;
    let mut reader = common::reader().await;
    let ids: Vec<i64> = read::recent_feed(&mut reader, board, 10)
        .await
        .unwrap()
        .iter()
        .map(|a| a.id)
        .collect();
    assert!(ids.contains(&f) && ids.contains(&failed), "{ids:?}");
    assert!(!ids.contains(&quiet), "{ids:?}");
    // `kb activity` still lists every row.
    let all: Vec<i64> = read::recent_activity(&mut reader, board, 10)
        .await
        .unwrap()
        .iter()
        .map(|a| a.id)
        .collect();
    assert!(all.contains(&quiet));
}
