//! What a decision follows from: `--because` (docs/board-v2.md section 18) against
//! `kanban_test`; the parsing checks need no database.

mod common;

use std::path::Path;

use kb_core::comment;
use kb_core::model::{Cause, CauseKind, CauseRef, DecisionKind, NoteKind};
use kb_core::read::{self, CardDetail};
use kb_core::write::{self, BECAUSE_HINT, BoardInit, CardAdd};
use kb_core::{Error, PgConnection, resolve};
use serde_json::json;

async fn new_board(conn: &mut PgConnection) -> (String, i64) {
    let session = common::unique("because");
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    let args = BoardInit {
        title: Some("依據".into()),
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

async fn add_card(conn: &mut PgConnection, session: &str, title: &str) -> i32 {
    write::card_add(conn, session, CardAdd::new("待辦", title))
        .await
        .expect("card add")
        .card
        .unwrap()
}

async fn detail(conn: &mut PgConnection, board_id: i64, seq: i32) -> CardDetail {
    read::card_detail(conn, board_id, seq)
        .await
        .unwrap()
        .unwrap()
}

async fn ask(conn: &mut PgConnection, session: &str, seq: i32) -> i64 {
    let options = vec![
        write::parse_option("甲").unwrap(),
        write::parse_option("乙").unwrap(),
    ];
    write::question_ask(conn, session, seq, "選哪個？", options, false)
        .await
        .unwrap()
        .question
        .unwrap()
}

async fn comment_on(conn: &mut PgConnection, board_id: i64, seq: i32, text: &str) -> i64 {
    let card_id = detail(conn, board_id, seq).await.card.id;
    comment::add_comment(conn, card_id, text).await.unwrap()
}

fn cause(kind: CauseKind, id: Option<i64>, seq: Option<i32>, title: Option<&str>) -> Cause {
    Cause {
        kind,
        id,
        card_seq: seq,
        card_title: title.map(str::to_string),
    }
}

async fn rev(conn: &mut PgConnection, session: &str) -> i64 {
    read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap()
        .rev
}

async fn cause_rows(conn: &mut PgConnection, board_id: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM cause WHERE board_id = $1")
        .bind(board_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

async fn last_activity(conn: &mut PgConnection, board_id: i64) -> serde_json::Value {
    read::recent_activity(conn, board_id, 1).await.unwrap()[0]
        .detail
        .clone()
}

// --- parsing ------------------------------------------------------------------------------------

#[test]
fn every_ref_form_parses() {
    assert_eq!(CauseRef::parse("30").unwrap(), CauseRef::Card(30));
    assert_eq!(CauseRef::parse("#30").unwrap(), CauseRef::Card(30));
    assert_eq!(CauseRef::parse(" #7 ").unwrap(), CauseRef::Card(7));
    assert_eq!(CauseRef::parse("q5").unwrap(), CauseRef::Question(5));
    assert_eq!(CauseRef::parse("Q5").unwrap(), CauseRef::Question(5));
    assert_eq!(CauseRef::parse("c75").unwrap(), CauseRef::Comment(75));
    assert_eq!(CauseRef::parse("C75").unwrap(), CauseRef::Comment(75));
    assert_eq!(CauseRef::parse("user").unwrap(), CauseRef::User);
    assert_eq!(CauseRef::parse("USER").unwrap(), CauseRef::User);
    // The canonical spelling of messages.
    let shown: Vec<String> = ["#30", "Q5", "c75", "User"]
        .iter()
        .map(|s| CauseRef::parse(s).unwrap().to_string())
        .collect();
    assert_eq!(shown, ["30", "q5", "c75", "user"]);
}

#[test]
fn bad_refs_are_data_errors() {
    for bad in [
        "", "0", "#0", "q", "q0", "c-1", "x5", "5a", "+5", "q 5", "users", "#q5",
    ] {
        let e = CauseRef::parse(bad).unwrap_err();
        assert_eq!(e.exit_code(), 1, "{bad}");
        assert_eq!(
            e.to_string(),
            format!(
                "invalid --because: {bad} (expected a card number like 30 or #30, \
                 q<question id>, c<comment id> or user)"
            )
        );
    }
}

// --- storage and both directions ----------------------------------------------------------------

#[tokio::test]
async fn a_note_stores_several_causes_and_both_directions_read_back() {
    let mut conn = common::writer().await;
    let (s, b) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &s, "統一提問畫面").await;
    let c2 = add_card(&mut conn, &s, "舊的提問畫面").await;
    let c3 = add_card(&mut conn, &s, "留言的卡片").await;
    let q = ask(&mut conn, &s, c1).await;
    let cm = comment_on(&mut conn, b, c3, "先別做舊畫面").await;

    let refs = [
        CauseRef::Card(c1),
        CauseRef::Question(q),
        CauseRef::Comment(cm),
        CauseRef::User,
        CauseRef::Card(c1), // repeated: stored once
    ];
    let out = write::card_note_because(&mut conn, &s, c2, NoteKind::Decision, "不做舊畫面", &refs)
        .await
        .unwrap();
    assert_eq!(out.hint, None, "causes given: no hint");
    assert_eq!(cause_rows(&mut conn, b).await, 4);

    // The activity carries the causes in order (section 18.3).
    assert_eq!(
        last_activity(&mut conn, b).await["because"],
        json!([
            {"kind": "card", "seq": c1},
            {"kind": "question", "id": q, "seq": c1},
            {"kind": "comment", "id": cm, "seq": c3},
            {"kind": "user"},
        ])
    );

    // Forward: on the note.
    let d = detail(&mut conn, b, c2).await;
    assert_eq!(
        d.notes[0].because,
        vec![
            cause(CauseKind::Card, None, Some(c1), Some("統一提問畫面")),
            cause(CauseKind::Question, Some(q), Some(c1), Some("統一提問畫面")),
            cause(CauseKind::Comment, Some(cm), Some(c3), Some("留言的卡片")),
            cause(CauseKind::User, None, None, None),
        ]
    );
    assert!(d.result_because.is_empty());
    assert!(d.led_to.is_empty());

    // Reverse: on the card that was cited, and on the cards holding the question and the
    // comment. The user cause has no reverse entry.
    let d1 = detail(&mut conn, b, c1).await;
    assert_eq!(d1.led_to.len(), 2, "card and question of #{c1}");
    assert_eq!(
        (d1.led_to[0].via, d1.led_to[0].via_id),
        (CauseKind::Card, None)
    );
    assert_eq!(
        (d1.led_to[1].via, d1.led_to[1].via_id),
        (CauseKind::Question, Some(q))
    );
    for c in &d1.led_to {
        assert_eq!(c.card_seq, c2);
        assert_eq!(c.card_title, "舊的提問畫面");
        assert_eq!(c.decision, DecisionKind::Note);
        assert_eq!(c.note_kind, Some(NoteKind::Decision));
        assert_eq!(c.note_id, Some(d.notes[0].id));
        assert_eq!(c.text, "不做舊畫面");
    }
    let d3 = detail(&mut conn, b, c3).await;
    assert_eq!(d3.led_to.len(), 1);
    assert_eq!(
        (d3.led_to[0].via, d3.led_to[0].via_id),
        (CauseKind::Comment, Some(cm))
    );

    // JSON: notes[].because, result_because and led_to.
    let v = serde_json::to_value(&d1).unwrap();
    assert_eq!(v["result_because"], json!([]));
    assert_eq!(v["led_to"][0]["decision"], "note");
    assert_eq!(v["led_to"][1]["via"], "question");
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(
        v["notes"][0]["because"][3],
        json!({"kind": "user", "id": null, "card_seq": null, "card_title": null})
    );
}

#[tokio::test]
async fn drop_and_done_attach_causes_to_the_current_result() {
    let mut conn = common::writer().await;
    let (s, b) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &s, "統一提問畫面").await;
    let c2 = add_card(&mut conn, &s, "舊的提問畫面").await;

    let out = write::card_drop_because(
        &mut conn,
        &s,
        c2,
        "改由 #1 一起處理\n細節略",
        &[CauseRef::Card(c1)],
    )
    .await
    .unwrap();
    assert_eq!(out.hint, None);
    assert_eq!(
        last_activity(&mut conn, b).await,
        json!({"seq": c2, "reason": "改由 #1 一起處理\n細節略", "because": [{"kind": "card", "seq": c1}]})
    );
    let d2 = detail(&mut conn, b, c2).await;
    assert_eq!(
        d2.result_because,
        vec![cause(CauseKind::Card, None, Some(c1), Some("統一提問畫面"))]
    );
    let d1 = detail(&mut conn, b, c1).await;
    assert_eq!(d1.led_to.len(), 1);
    assert_eq!(d1.led_to[0].decision, DecisionKind::Drop);
    assert_eq!(d1.led_to[0].text, "改由 #1 一起處理\n細節略");

    // done keeps its result in the activity, with or without causes.
    write::card_done_because(&mut conn, &s, c2, "其實做完了", &[CauseRef::User])
        .await
        .unwrap();
    assert_eq!(
        last_activity(&mut conn, b).await,
        json!({"seq": c2, "lane": "完成", "result": "其實做完了", "because": [{"kind": "user"}]})
    );
    let d2 = detail(&mut conn, b, c2).await;
    assert_eq!(
        d2.result_because,
        vec![cause(CauseKind::User, None, None, None)]
    );

    // A later done without causes: the current result has none; the history stays on #1.
    let out = write::card_done(&mut conn, &s, c2, "最後的結果")
        .await
        .unwrap();
    assert_eq!(out.hint, None, "done never hints");
    assert_eq!(last_activity(&mut conn, b).await["result"], "最後的結果");
    assert!(detail(&mut conn, b, c2).await.result_because.is_empty());
    let d1 = detail(&mut conn, b, c1).await;
    assert_eq!(d1.led_to.len(), 1, "user causes have no reverse entry");
    assert_eq!(d1.led_to[0].text, "改由 #1 一起處理\n細節略");
}

#[tokio::test]
async fn a_question_on_the_same_card_is_a_valid_cause() {
    let mut conn = common::writer().await;
    let (s, b) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &s, "甲").await;
    let q = ask(&mut conn, &s, c1).await;
    write::card_note_because(
        &mut conn,
        &s,
        c1,
        NoteKind::Decision,
        "選甲",
        &[CauseRef::Question(q)],
    )
    .await
    .unwrap();
    let d = detail(&mut conn, b, c1).await;
    assert_eq!(
        d.notes[0].because,
        vec![cause(CauseKind::Question, Some(q), Some(c1), Some("甲"))]
    );
    assert_eq!(d.led_to.len(), 1);
    assert_eq!(d.led_to[0].card_seq, c1);
}

// --- validation ---------------------------------------------------------------------------------

#[tokio::test]
async fn bad_targets_fail_and_write_nothing() {
    let mut conn = common::writer().await;
    let (s, b) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &s, "甲").await;
    let c2 = add_card(&mut conn, &s, "乙").await;
    let decision_note = {
        write::card_note(&mut conn, &s, c1, NoteKind::Decision, "一個決定")
            .await
            .unwrap();
        detail(&mut conn, b, c1).await.notes[0].id
    };
    // Another board's question and comment.
    let (s_other, b_other) = new_board(&mut conn).await;
    let o1 = add_card(&mut conn, &s_other, "別的看板").await;
    let q_other = ask(&mut conn, &s_other, o1).await;
    let cm_other = comment_on(&mut conn, b_other, o1, "別的留言").await;

    let cases: Vec<(Vec<CauseRef>, String)> = vec![
        (
            vec![CauseRef::Card(99)],
            "--because 99: card 99 not found on this board".into(),
        ),
        (
            vec![CauseRef::Card(c2)],
            format!(
                "--because {c2}: a decision cannot follow from its own card; name a question \
                 (q<id>) or a comment (c<id>) on it instead"
            ),
        ),
        (
            vec![CauseRef::Question(q_other)],
            format!("--because q{q_other}: question {q_other} not found on this board"),
        ),
        (
            vec![CauseRef::Comment(cm_other)],
            format!("--because c{cm_other}: comment {cm_other} not found on this board"),
        ),
        (
            vec![CauseRef::Comment(decision_note)],
            format!(
                "--because c{decision_note}: note {decision_note} is a decision note, not a comment"
            ),
        ),
        (
            // The first bad one fails the whole write, even after good ones.
            vec![
                CauseRef::User,
                CauseRef::Card(c1),
                CauseRef::Question(9_999_999_999),
            ],
            "--because q9999999999: question 9999999999 not found on this board".into(),
        ),
    ];
    for (refs, message) in cases {
        let before = rev(&mut conn, &s).await;
        let notes_before = detail(&mut conn, b, c2).await.notes.len();
        for result in [
            write::card_note_because(&mut conn, &s, c2, NoteKind::Decision, "x", &refs).await,
            write::card_drop_because(&mut conn, &s, c2, "x", &refs).await,
            write::card_done_because(&mut conn, &s, c2, "x", &refs).await,
        ] {
            let e = result.unwrap_err();
            assert!(matches!(e, Error::Invalid(_)), "{e:?}");
            assert_eq!(e.exit_code(), 1);
            assert_eq!(e.to_string(), message);
        }
        assert_eq!(rev(&mut conn, &s).await, before, "nothing written");
        let d2 = detail(&mut conn, b, c2).await;
        assert_eq!(d2.notes.len(), notes_before);
        assert_eq!(d2.lane.name, "待辦", "not moved");
    }
    assert_eq!(cause_rows(&mut conn, b).await, 0);
}

// --- the hint -----------------------------------------------------------------------------------

#[tokio::test]
async fn the_hint_follows_section_18_5() {
    let mut conn = common::writer().await;
    let (s, _) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &s, "甲").await;
    let c2 = add_card(&mut conn, &s, "乙").await;
    let hint = Some(BECAUSE_HINT.to_string());
    assert_eq!(
        BECAUSE_HINT,
        "if this follows from a card, a question, a comment or the user, \
         name it with --because <card> | q<id> | c<id> | user. Your own choice needs no --because."
    );

    for (kind, expected) in [
        (NoteKind::Decision, hint.clone()),
        (NoteKind::Progress, None),
        (NoteKind::Result, None),
        (NoteKind::Blocker, None),
        (NoteKind::Reply, None),
    ] {
        let out = write::card_note(&mut conn, &s, c1, kind, "x")
            .await
            .unwrap();
        assert_eq!(out.hint, expected, "{kind}");
    }
    let out = write::card_note_because(
        &mut conn,
        &s,
        c1,
        NoteKind::Decision,
        "x",
        &[CauseRef::User],
    )
    .await
    .unwrap();
    assert_eq!(out.hint, None);
    // JSON: a field only when there is a hint.
    assert!(serde_json::to_value(&out).unwrap().get("hint").is_none());

    let out = write::card_drop(&mut conn, &s, c2, "x").await.unwrap();
    assert_eq!(out.hint, hint);
    assert_eq!(serde_json::to_value(&out).unwrap()["hint"], BECAUSE_HINT);
    let out = write::card_drop_because(&mut conn, &s, c2, "x", &[CauseRef::User])
        .await
        .unwrap();
    assert_eq!(out.hint, None);
    let out = write::card_done(&mut conn, &s, c2, "x").await.unwrap();
    assert_eq!(out.hint, None);
}

// --- old rows and the reader role ---------------------------------------------------------------

#[tokio::test]
async fn rows_without_causes_from_an_old_binary_read_fine() {
    let mut conn = common::writer().await;
    let (s, b) = new_board(&mut conn).await;
    let c1 = add_card(&mut conn, &s, "舊資料").await;
    let card_id = detail(&mut conn, b, c1).await.card.id;
    // What a version-6 binary wrote: a note, and a done without `result` in the activity.
    sqlx::query("INSERT INTO card_note (card_id, kind, body) VALUES ($1, 'decision', '舊的決定')")
        .bind(card_id)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("UPDATE card SET result = '舊的結果' WHERE id = $1")
        .bind(card_id)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO activity (board_id, card_id, verb, detail)
         VALUES ($1, $2, 'card.done', jsonb_build_object('seq', $3::int, 'lane', '完成'))",
    )
    .bind(b)
    .bind(card_id)
    .bind(c1)
    .execute(&mut conn)
    .await
    .unwrap();

    // The reader (the TUI's role) reads the new table and the old rows.
    let mut reader = common::reader().await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM cause WHERE board_id = $1")
        .bind(b)
        .fetch_one(&mut reader)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let d = detail(&mut reader, b, c1).await;
    assert_eq!(d.notes.len(), 1);
    assert!(d.notes[0].because.is_empty());
    assert_eq!(d.card.result.as_deref(), Some("舊的結果"));
    assert!(d.result_because.is_empty());
    assert!(d.led_to.is_empty());
    // The reader cannot write causes.
    let err = sqlx::query(
        "INSERT INTO cause (board_id, card_id, activity_id, kind) VALUES (1, 1, 1, 'user')",
    )
    .execute(&mut reader)
    .await
    .unwrap_err();
    let code = err
        .as_database_error()
        .and_then(|e| e.code())
        .map(|c| c.to_string());
    assert_eq!(code.as_deref(), Some("42501"), "{err}");
}
