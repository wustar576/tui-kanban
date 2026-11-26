//! P2 integration tests against `kanban_test`: turns, rules, hooks (database side),
//! questions and the answer function (docs/hooks.md, docs/questions.md).

mod common;

use std::path::Path;
use std::time::Duration;

use kb_core::answer::{self, AnswerError};
use kb_core::events::{BoardEvent, EventListener, ListenEvent};
use kb_core::hook::{self, PreTool, Stop};
use kb_core::model::{Board, BoardStatus, NoteKind, Outcome, QuestionStatus};
use kb_core::read;
use kb_core::resolve::{self, SessionSource, SpaceContext, TerminalInfo, WorkspaceTarget};
use kb_core::rules::{self, Level, Levels, RuleId};
use kb_core::write::{self, BoardInit, BoardStartArgs, CardAdd, OptionSpec, StartStatus};
use kb_core::{Error, PgConnection, Role};
use serde_json::json;

const LANES: &str = "待辦:todo,實作:doing,完成:done";

fn manual_ws(name: &str) -> WorkspaceTarget {
    resolve::resolve_workspace(Some(name), None, Path::new("."))
}

async fn new_board(conn: &mut PgConnection) -> String {
    let session = common::unique("p2");
    let args = BoardInit {
        title: Some("提問看板".into()),
        goal: None,
        lanes: Some(write::parse_lanes(LANES).unwrap()),
        epics: None,
        cwd: None,
    };
    let ws = manual_ws(&common::unique("ws"));
    write::board_init(conn, &session, args, || ws)
        .await
        .unwrap();
    session
}

async fn board(conn: &mut PgConnection, session: &str) -> Board {
    read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap()
}

async fn add_card(conn: &mut PgConnection, session: &str, lane: &str, title: &str) -> i32 {
    write::card_add(conn, session, CardAdd::new(lane, title))
        .await
        .unwrap()
        .card
        .unwrap()
}

async fn start_turn(conn: &mut PgConnection, session: &str, key: &str) {
    hook::prompt(conn, session, Some(key), "請修正登入逾時")
        .await
        .unwrap()
        .expect("board exists");
}

fn only(rule: RuleId, level: Level) -> Levels {
    let mut levels = Levels::default();
    for r in RuleId::ALL {
        levels.set(r, Level::Off);
    }
    levels.set(rule, level);
    levels
}

fn opts(labels: &[&str]) -> Vec<OptionSpec> {
    labels
        .iter()
        .map(|l| write::parse_option(l).unwrap())
        .collect()
}

async fn ask(
    conn: &mut PgConnection,
    session: &str,
    seq: i32,
    labels: &[&str],
    multi: bool,
) -> i64 {
    write::question_ask(
        conn,
        session,
        seq,
        "token 的有效期要改成多久？",
        opts(labels),
        multi,
    )
    .await
    .unwrap()
    .question
    .unwrap()
}

async fn activity_verbs(conn: &mut PgConnection, board_id: i64) -> Vec<String> {
    read::recent_activity(conn, board_id, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.verb)
        .collect()
}

async fn option_ids(conn: &mut PgConnection, board_id: i64, question: i64) -> Vec<i64> {
    read::question(conn, board_id, question)
        .await
        .unwrap()
        .unwrap()
        .options
        .iter()
        .map(|o| o.id)
        .collect()
}

// --- turns (docs/hooks.md 2.4) -------------------------------------------------------------

#[tokio::test]
async fn prompt_starts_a_turn_and_every_activity_row_gets_the_turn_key() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    assert_eq!((b.turn_key.as_deref(), b.turn_started_at), (None, None));
    let before = read::recent_activity(&mut conn, b.id, 1).await.unwrap();
    assert_eq!(before[0].turn_key, None);

    start_turn(&mut conn, &session, "prompt-A").await;
    let b = board(&mut conn, &session).await;
    assert_eq!(b.turn_key.as_deref(), Some("prompt-A"));
    assert!(b.turn_started_at.is_some());
    let seq = add_card(&mut conn, &session, "待辦", "卡片").await;
    write::card_note(&mut conn, &session, seq, NoteKind::Progress, "紀錄")
        .await
        .unwrap();
    let rows = read::recent_activity(&mut conn, b.id, 3).await.unwrap();
    assert_eq!(rows[2].verb, "user.prompt");
    assert_eq!(rows[2].actor, "user");
    assert_eq!(rows[2].detail["excerpt"], "請修正登入逾時");
    assert!(
        rows.iter()
            .all(|a| a.turn_key.as_deref() == Some("prompt-A"))
    );
    assert!(rows.iter().all(|a| a.at >= b.turn_started_at.unwrap()));

    // No prompt_id: a key is generated.
    hook::prompt(&mut conn, &session, None, &"長".repeat(100))
        .await
        .unwrap();
    let b = board(&mut conn, &session).await;
    assert!(b.turn_key.as_deref().unwrap().starts_with("kb-"));
    let last = read::recent_activity(&mut conn, b.id, 1).await.unwrap();
    assert_eq!(
        last[0].detail["excerpt"].as_str().unwrap().chars().count(),
        80
    );
}

// --- questions (docs/questions.md 4, 5) ----------------------------------------------------

#[tokio::test]
async fn ask_validates_and_reads_show_open_questions() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let a = add_card(&mut conn, &session, "實作", "甲").await;
    let done = add_card(&mut conn, &session, "完成", "乙").await;
    let b = board(&mut conn, &session).await;

    for labels in [&["只有一個"][..], &["1", "2", "3", "4", "5", "6", "7"][..]] {
        let err = write::question_ask(&mut conn, &session, a, "問題", opts(labels), false)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("a question needs 2 to 6 options"),
            "{err}"
        );
        assert_eq!(err.exit_code(), 1);
    }
    let err = write::question_ask(&mut conn, &session, done, "問題", opts(&["a", "b"]), false)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "card is closed");
    assert!(write::parse_option("::說明").is_err());
    assert_eq!(
        write::parse_option("15 分鐘::與 refresh 的間隔一致").unwrap(),
        OptionSpec {
            label: "15 分鐘".into(),
            description: Some("與 refresh 的間隔一致".into())
        }
    );

    let out = write::question_ask(
        &mut conn,
        &session,
        a,
        "token 的有效期要改成多久？",
        opts(&[
            "15 分鐘::與 refresh 的間隔一致",
            "30 分鐘::減少更新次數",
            "60 分鐘",
        ]),
        false,
    )
    .await
    .unwrap();
    let q1 = out.question.unwrap();
    assert_eq!(
        out.message,
        format!("question {q1} asked on card {a} (3 options)")
    );
    let rows = read::recent_activity(&mut conn, b.id, 1).await.unwrap();
    assert_eq!(rows[0].verb, "question.ask");
    assert_eq!(rows[0].detail["question"], q1);
    assert_eq!(rows[0].detail["seq"], a);
    assert_eq!(rows[0].detail["excerpt"], "token 的有效期要改成多久？");

    // Second question, answered: ordering in card detail is open first.
    let q2 = ask(&mut conn, &session, a, &["是", "否"], false).await;
    let mut reader = common::reader().await;
    let ids = option_ids(&mut reader, b.id, q2).await;
    answer::answer_question(&mut reader, q2, &ids[..1], false, None)
        .await
        .unwrap();
    let q3 = ask(&mut conn, &session, a, &["甲", "乙"], true).await;

    let snap = read::board_snapshot(&mut reader, b.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.open_question_count, 2);
    assert_eq!(snap.card(a).unwrap().1.open_question_count, 2);
    assert_eq!(snap.card(done).unwrap().1.open_question_count, 0);

    let d = read::card_detail(&mut reader, b.id, a)
        .await
        .unwrap()
        .unwrap();
    let order: Vec<(i64, QuestionStatus)> = d
        .questions
        .iter()
        .map(|q| (q.question.id, q.question.status))
        .collect();
    assert_eq!(
        order,
        [
            (q1, QuestionStatus::Open),
            (q3, QuestionStatus::Open),
            (q2, QuestionStatus::Answered)
        ]
    );
    let first = &d.questions[0];
    assert_eq!(first.card_seq, a);
    let positions: Vec<i32> = first.options.iter().map(|o| o.position).collect();
    assert_eq!(positions, [1, 2, 3]);
    assert_eq!(
        first.options[0].description.as_deref(),
        Some("與 refresh 的間隔一致")
    );
    assert_eq!(first.options[2].description, None);
    assert_eq!(d.questions[2].selected_labels(), ["是"]);
    assert!(d.questions[2].question.answered_at.is_some());
    assert!(d.questions[1].question.multi);

    let list = read::board_questions(&mut reader, b.id, true)
        .await
        .unwrap();
    assert_eq!(
        list.iter().map(|q| q.question.id).collect::<Vec<_>>(),
        [q1, q3]
    );
    assert_eq!(
        read::board_questions(&mut reader, b.id, false)
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn answer_function_validates_every_case_and_changes_nothing_on_failure() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let a = add_card(&mut conn, &session, "實作", "甲").await;
    let b = board(&mut conn, &session).await;
    let single = ask(&mut conn, &session, a, &["15 分鐘", "30 分鐘"], false).await;
    let multi = ask(&mut conn, &session, a, &["甲", "乙", "丙"], true).await;
    let other = ask(&mut conn, &session, a, &["x", "y"], false).await;
    let mut reader = common::reader().await;
    let single_ids = option_ids(&mut reader, b.id, single).await;
    let multi_ids = option_ids(&mut reader, b.id, multi).await;
    let other_ids = option_ids(&mut reader, b.id, other).await;

    let rev = board(&mut conn, &session).await.rev;
    let fails: Vec<(i64, Vec<i64>, AnswerError)> = vec![
        (i64::MAX, vec![single_ids[0]], AnswerError::QuestionNotFound),
        (single, vec![], AnswerError::NoOptionSelected),
        (single, vec![other_ids[0]], AnswerError::OptionNotInQuestion),
        (
            single,
            vec![single_ids[0], other_ids[0]],
            AnswerError::OptionNotInQuestion,
        ),
        (single, single_ids.clone(), AnswerError::AcceptsOneOption),
    ];
    for (q, ids, expected) in fails {
        let err = answer::answer_question(&mut reader, q, &ids, false, None)
            .await
            .unwrap_err();
        match err {
            Error::Answer(e) => assert_eq!(e, expected, "{q} {ids:?}"),
            other => panic!("expected {expected:?}, got {other}"),
        }
        assert_eq!(err_text(expected), expected.to_string());
    }
    assert_eq!(
        board(&mut conn, &session).await.rev,
        rev,
        "failures change nothing"
    );
    let q = read::question(&mut reader, b.id, single)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.question.status, QuestionStatus::Open);
    assert!(q.options.iter().all(|o| !o.selected));

    // Duplicated ids count once: a single-choice question accepts [x, x].
    answer::answer_question(
        &mut reader,
        single,
        &[single_ids[1], single_ids[1]],
        false,
        None,
    )
    .await
    .unwrap();
    let q = read::question(&mut reader, b.id, single)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.question.status, QuestionStatus::Answered);
    assert_eq!(q.selected_labels(), ["30 分鐘"]);
    let err = answer::answer_question(&mut reader, single, &[single_ids[0]], false, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Answer(AnswerError::NotOpen)), "{err}");

    // Multi: several options.
    answer::answer_question(
        &mut reader,
        multi,
        &[multi_ids[2], multi_ids[0]],
        false,
        None,
    )
    .await
    .unwrap();
    let q = read::question(&mut reader, b.id, multi)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.selected_labels(), ["甲", "丙"]);
    let rows = read::recent_activity(&mut conn, b.id, 1).await.unwrap();
    assert_eq!(rows[0].verb, "question.answer");
    assert_eq!(rows[0].actor, "user");
    assert_eq!(rows[0].card_id, Some(q.question.card_id));
    assert_eq!(
        rows[0].detail,
        json!({"question": multi, "seq": a, "labels": ["甲", "丙"]})
    );

    // Chat ignores the option ids (even foreign ones).
    answer::answer_question(&mut reader, other, &[single_ids[0]], true, None)
        .await
        .unwrap();
    let q = read::question(&mut reader, b.id, other)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.question.status, QuestionStatus::Chat);
    assert!(q.options.iter().all(|o| !o.selected));
    let rows = read::recent_activity(&mut conn, b.id, 1).await.unwrap();
    assert_eq!(
        rows[0].detail,
        json!({"question": other, "seq": a, "chat": true})
    );

    // Cancelled question: not open.
    let cancelled = ask(&mut conn, &session, a, &["a", "b"], false).await;
    write::question_cancel(&mut conn, &session, cancelled, Some("不需要了".into()))
        .await
        .unwrap();
    let err = answer::answer_question(&mut reader, cancelled, &[], true, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Answer(AnswerError::NotOpen)));
    let err = write::question_cancel(&mut conn, &session, single, None)
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("question is not open"), "{err}");
    let err = write::question_cancel(&mut conn, &session, i64::MAX, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::QuestionNotFound(_)));

    // Archived board.
    let open = ask(&mut conn, &session, a, &["a", "b"], false).await;
    write::board_archive(&mut conn, &session, None, None)
        .await
        .unwrap();
    let err = answer::answer_question(&mut reader, open, &[], true, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Answer(AnswerError::BoardArchived)),
        "{err}"
    );
}

fn err_text(e: AnswerError) -> &'static str {
    e.as_str()
}

#[tokio::test]
async fn reader_can_answer_but_cannot_write_tables() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let a = add_card(&mut conn, &session, "實作", "甲").await;
    let q = ask(&mut conn, &session, a, &["a", "b"], false).await;
    let b = board(&mut conn, &session).await;
    let mut reader = common::reader().await;
    let ids = option_ids(&mut reader, b.id, q).await;

    for sql in [
        format!("UPDATE question SET status = 'answered' WHERE id = {q}"),
        format!("UPDATE question_option SET selected = true WHERE question_id = {q}"),
        format!("INSERT INTO question_option (question_id, position, label) VALUES ({q}, 9, 'x')"),
        format!("DELETE FROM question WHERE id = {q}"),
        format!("UPDATE board SET rev = rev + 1 WHERE id = {}", b.id),
        format!(
            "INSERT INTO activity (board_id, verb) VALUES ({}, 'question.answer')",
            b.id
        ),
    ] {
        let err = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .execute(&mut reader)
            .await
            .unwrap_err();
        let code = err
            .as_database_error()
            .and_then(|e| e.code())
            .map(|c| c.to_string());
        assert_eq!(code.as_deref(), Some("42501"), "{sql}: {err}");
    }
    answer::answer_question(&mut reader, q, &ids[..1], false, None)
        .await
        .unwrap();

    // Only the kb roles may execute the function; PUBLIC may not.
    let public: bool = sqlx::query_scalar(
        "SELECT bool_or(acl.grantee = 0)
         FROM pg_proc p, aclexplode(coalesce(p.proacl, acldefault('f', p.proowner))) acl
         WHERE p.proname = 'kb_answer_question'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(!public, "PUBLIC must not execute kb_answer_question");
    let config: (bool, Vec<String>) = sqlx::query_as(
        "SELECT p.prosecdef, p.proconfig FROM pg_proc p WHERE p.proname = 'kb_answer_question'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(config.0, "SECURITY DEFINER");
    assert!(
        config.1.iter().any(|c| c.starts_with("search_path=")),
        "{:?}",
        config.1
    );
}

#[tokio::test]
async fn inbox_delivers_answers_once() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let a = add_card(&mut conn, &session, "實作", "甲").await;
    let q1 = ask(&mut conn, &session, a, &["15 分鐘", "30 分鐘"], false).await;
    let q2 = ask(&mut conn, &session, a, &["是", "否"], false).await;
    let _q3 = ask(&mut conn, &session, a, &["x", "y"], false).await;
    let b = board(&mut conn, &session).await;

    let empty = write::inbox(&mut conn, &session, false).await.unwrap();
    assert!(empty.answers.is_empty());
    assert_eq!(empty.open_questions, 3);
    assert_eq!(
        board(&mut conn, &session).await.rev,
        b.rev,
        "empty inbox writes nothing"
    );

    let mut reader = common::reader().await;
    let ids = option_ids(&mut reader, b.id, q1).await;
    answer::answer_question(&mut reader, q1, &ids[..1], false, None)
        .await
        .unwrap();
    answer::answer_question(&mut reader, q2, &[], true, None)
        .await
        .unwrap();

    let peek = write::inbox(&mut conn, &session, true).await.unwrap();
    assert_eq!(peek.answers.len(), 2);
    assert!(!peek.delivered);
    let inbox = write::inbox(&mut conn, &session, false).await.unwrap();
    assert!(inbox.delivered);
    assert_eq!(
        inbox
            .answers
            .iter()
            .map(|q| q.question.id)
            .collect::<Vec<_>>(),
        [q1, q2]
    );
    assert_eq!(inbox.open_questions, 1);
    let rows = read::recent_activity(&mut conn, b.id, 2).await.unwrap();
    assert!(rows.iter().all(|r| r.verb == "question.deliver"));
    assert!(
        read::question(&mut conn, b.id, q1)
            .await
            .unwrap()
            .unwrap()
            .question
            .delivered_at
            .is_some()
    );
    assert!(
        write::inbox(&mut conn, &session, false)
            .await
            .unwrap()
            .answers
            .is_empty()
    );
}

// --- notifications ---------------------------------------------------------------------------

async fn board_events(listener: &mut EventListener, board_id: i64) -> Vec<BoardEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = tokio::time::timeout(Duration::from_millis(400), listener.next()).await {
        match ev.expect("listener") {
            ListenEvent::Board(e) if e.board == board_id => events.push(e),
            ListenEvent::Board(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    events
}

#[tokio::test]
async fn answer_notifies_once_and_tool_use_never() {
    let config = common::config();
    common::ensure_migrated().await;
    let mut listener = EventListener::connect(config.url(Role::Reader).unwrap())
        .await
        .unwrap();
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let a = add_card(&mut conn, &session, "實作", "甲").await;
    let q = ask(&mut conn, &session, a, &["a", "b", "c"], true).await;
    let b = board(&mut conn, &session).await;
    let _ = board_events(&mut listener, b.id).await;

    let mut reader = common::reader().await;
    let ids = option_ids(&mut reader, b.id, q).await;
    answer::answer_question(&mut reader, q, &ids, false, None)
        .await
        .unwrap();
    let events = board_events(&mut listener, b.id).await;
    assert_eq!(
        events,
        [BoardEvent {
            workspace: b.workspace_id,
            board: b.id,
            rev: b.rev + 1
        }]
    );

    let written = hook::tool_used(
        &mut conn,
        &session,
        "Edit",
        &json!({"file_path": "src/main.rs"}),
        Some(12),
        None,
    )
    .await
    .unwrap();
    assert!(written);
    assert!(board_events(&mut listener, b.id).await.is_empty());
    let after = board(&mut conn, &session).await;
    assert_eq!(after.rev, b.rev + 1, "tool.use does not bump rev");
    let rows = read::recent_activity(&mut conn, b.id, 1).await.unwrap();
    assert_eq!(rows[0].verb, "tool.use");
    assert_eq!(rows[0].actor, "agent");
    assert_eq!(
        rows[0].detail,
        // Not a scratch edit (a relative path without a hook cwd): no `scratch` field.
        json!({"tool": "Edit", "target": "src/main.rs", "duration_ms": 12})
    );
}

// --- rules (docs/hooks.md 4) ----------------------------------------------------------------

async fn last_rows(
    conn: &mut PgConnection,
    board_id: i64,
    n: i64,
) -> Vec<(String, serde_json::Value)> {
    read::recent_activity(conn, board_id, n)
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.verb, a.detail))
        .collect()
}

#[tokio::test]
async fn r1_and_r6_in_pre_tool_at_each_level() {
    let mut conn = common::writer().await;
    let session = common::unique("r1");
    // A board without lanes (as `kb board start` creates it).
    write::board_start(
        &mut conn,
        &session,
        write::BoardStartArgs::default(),
        None,
        || manual_ws("r1"),
    )
    .await
    .unwrap();
    let b = board(&mut conn, &session).await;

    assert_eq!(
        hook::pre_tool(&mut conn, &session, "Edit", &only(RuleId::R1, Level::Off))
            .await
            .unwrap(),
        PreTool::Pass
    );
    let rev = board(&mut conn, &session).await.rev;
    assert_eq!(rev, b.rev, "off writes nothing");

    match hook::pre_tool(&mut conn, &session, "Write", &only(RuleId::R1, Level::Warn))
        .await
        .unwrap()
    {
        PreTool::Warn(r) => assert_eq!(r.rule, RuleId::R1),
        other => panic!("{other:?}"),
    }
    let rows = last_rows(&mut conn, b.id, 1).await;
    assert_eq!(rows[0].0, "check.warn");
    assert_eq!(
        rows[0].1,
        json!({
            "rule": "R1",
            "message": "看板沒有泳道就修改檔案",
            "text": "R1: the board needs lanes and at least one card before files are edited.",
            "fix": "kb board init --title \"<title>\" --lanes \"<name>:todo,<name>:doing,<name>:done\" ; \
                    kb card add --lane <lane> --title \"<title>\"",
        })
    );

    match hook::pre_tool(
        &mut conn,
        &session,
        "MultiEdit",
        &only(RuleId::R1, Level::Block),
    )
    .await
    .unwrap()
    {
        PreTool::Block(_, text) => {
            assert!(text.starts_with("Kanban rules failed."), "{text}");
            assert!(text.contains("\nR1: "), "{text}");
            assert!(text.contains("\n    Fix: kb board init"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "check.block");

    // Lanes but no card: still failing; lanes and a card: passes.
    let args = BoardInit {
        title: Some("有泳道".into()),
        lanes: Some(write::parse_lanes(LANES).unwrap()),
        ..Default::default()
    };
    write::board_init(&mut conn, &session, args, || panic!("exists"))
        .await
        .unwrap();
    assert!(matches!(
        hook::pre_tool(
            &mut conn,
            &session,
            "NotebookEdit",
            &only(RuleId::R1, Level::Block)
        )
        .await
        .unwrap(),
        PreTool::Block(..)
    ));
    add_card(&mut conn, &session, "待辦", "第一張").await;
    assert_eq!(
        hook::pre_tool(&mut conn, &session, "Edit", &only(RuleId::R1, Level::Block))
            .await
            .unwrap(),
        PreTool::Pass
    );
    // Tools other than file edits and AskUserQuestion are not checked.
    assert_eq!(
        hook::pre_tool(&mut conn, &session, "Bash", &only(RuleId::R1, Level::Block))
            .await
            .unwrap(),
        PreTool::Pass
    );

    // R6: blocks by default (docs/board-v2.md 23.2).
    assert!(
        matches!(
            hook::pre_tool(&mut conn, &session, "AskUserQuestion", &Levels::default())
                .await
                .unwrap(),
            PreTool::Block(..)
        ),
        "R6 blocks by default"
    );
    assert_eq!(
        hook::pre_tool(
            &mut conn,
            &session,
            "AskUserQuestion",
            &only(RuleId::R6, Level::Off)
        )
        .await
        .unwrap(),
        PreTool::Pass
    );
    assert!(matches!(
        hook::pre_tool(
            &mut conn,
            &session,
            "AskUserQuestion",
            &only(RuleId::R6, Level::Warn)
        )
        .await
        .unwrap(),
        PreTool::Warn(_)
    ));
    match hook::pre_tool(
        &mut conn,
        &session,
        "AskUserQuestion",
        &only(RuleId::R6, Level::Block),
    )
    .await
    .unwrap()
    {
        PreTool::Block(_, text) => assert_eq!(
            text,
            "AskUserQuestion is disabled here. Ask through the board instead:\n  \
             kb ask --card <n> \"<question>\" --option \"<label>::<description>\" --option \"<label>\"\n\
             If no card covers the question, add one first: kb card add --lane <lane> --title \"<topic>\" --discuss\n\
             Then continue with work that does not depend on the answer."
        ),
        other => panic!("{other:?}"),
    }

    // No board: nothing.
    assert_eq!(
        hook::pre_tool(
            &mut conn,
            &common::unique("none"),
            "Edit",
            &only(RuleId::R1, Level::Block)
        )
        .await
        .unwrap(),
        PreTool::NoBoard
    );
}

fn failed(results: &[rules::RuleResult], rule: RuleId) -> Vec<Option<i32>> {
    results
        .iter()
        .find(|r| r.rule == rule)
        .unwrap()
        .violations
        .iter()
        .map(|v| v.card)
        .collect()
}

#[tokio::test]
async fn r2_wip_limit_at_each_level() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    write::lane_add(
        &mut conn,
        &session,
        "驗證",
        kb_core::model::LaneKind::Doing,
        None,
        Some(1),
    )
    .await
    .unwrap();
    let x = add_card(&mut conn, &session, "驗證", "一").await;
    let y = add_card(&mut conn, &session, "驗證", "二").await;
    for s in [x, y] {
        write::card_block(&mut conn, &session, s, "等待")
            .await
            .unwrap(); // R3-exempt
    }
    let b = board(&mut conn, &session).await;

    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R2, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "turn.summary");

    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R2, Level::Warn))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    let rows = last_rows(&mut conn, b.id, 2).await;
    assert_eq!(rows[0].0, "turn.summary");
    assert_eq!(
        rows[1],
        (
            "check.warn".into(),
            json!({
                "rule": "R2",
                "message": "泳道「驗證」超過上限（2/1）",
                "text": "R2: lane \"驗證\" has 2 cards, over its WIP limit of 1.",
                "fix": "kb card move <n> --to <lane>  (move cards out of \"驗證\")",
            })
        )
    );

    match hook::stop(&mut conn, &session, &only(RuleId::R2, Level::Block))
        .await
        .unwrap()
    {
        Stop::Block(results) => {
            let text = rules::block_text(&results, rules::STOP_HEADER);
            assert_eq!(
                text,
                "Kanban rules failed. Fix them, then stop again.\n\
                 R2: lane \"驗證\" has 2 cards, over its WIP limit of 1.\n    \
                 Fix: kb card move <n> --to <lane>  (move cards out of \"驗證\")"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "check.block");
}

#[tokio::test]
async fn r3_doing_cards_need_activity_this_turn() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let five = add_card(&mut conn, &session, "實作", "修正 token 更新時間").await;
    let blocked = add_card(&mut conn, &session, "實作", "等待中").await;
    write::card_block(&mut conn, &session, blocked, "等待確認")
        .await
        .unwrap();
    add_card(&mut conn, &session, "待辦", "還沒開始").await;
    // R3 is off by default (docs/board-v2.md 9); configured to block it still works.
    let mut levels = Levels::default();
    levels.set(RuleId::R3, Level::Block);

    // No user.prompt yet: R3 is not checked.
    let b = board(&mut conn, &session).await;
    let r = rules::check_turn(&mut conn, &b, &levels).await.unwrap();
    let r3 = r.iter().find(|r| r.rule == RuleId::R3).unwrap();
    assert!(!r3.checked && r3.passed);
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Pass { .. }
    ));

    // New turn: the doing card has no activity in it; the blocked card is exempt.
    start_turn(&mut conn, &session, "t1").await;
    // At the default level (off) the violation does not stop the agent.
    match hook::stop(&mut conn, &session, &Levels::default())
        .await
        .unwrap()
    {
        Stop::Pass { results, .. } => {
            let r3 = results.iter().find(|r| r.rule == RuleId::R3).unwrap();
            assert_eq!((r3.level, r3.passed), (Level::Off, false));
        }
        other => panic!("{other:?}"),
    }
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Block(results) => {
            assert_eq!(failed(&results, RuleId::R3), [Some(five)]);
            assert_eq!(
                rules::block_text(&results, rules::STOP_HEADER),
                format!(
                    "Kanban rules failed. Fix them, then stop again.\n\
                     R3: card {five} \"修正 token 更新時間\" is in a doing lane but has no activity this turn.\n    \
                     Fix: kb card note {five} \"<what happened>\"  (or move/block it)"
                )
            );
        }
        other => panic!("{other:?}"),
    }
    let b = board(&mut conn, &session).await;
    let rows = last_rows(&mut conn, b.id, 1).await;
    assert_eq!(
        rows[0],
        (
            "check.block".into(),
            json!({
                "rule": "R3",
                "message": format!("#{five} 這一輪沒有紀錄"),
                "text": format!(
                    "R3: card {five} \"修正 token 更新時間\" is in a doing lane but has no activity this turn."
                ),
                "fix": format!("kb card note {five} \"<what happened>\"  (or move/block it)"),
            })
        )
    );
    // The check.block row has no card id, so it does not satisfy R3.
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Block(_)
    ));

    // Warn level: passes, records check.warn and the summary.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R3, Level::Warn))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].0, "check.warn");

    // Any activity on the card this turn fixes it.
    write::card_note(&mut conn, &session, five, NoteKind::Progress, "找到原因")
        .await
        .unwrap();
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Pass { summary, .. } => {
            assert_eq!(summary["cards_changed"], 1);
            assert_eq!(summary["notes"], 1);
            assert_eq!(summary["tools"], 0);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn r4_cards_done_this_turn_need_a_result() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let old = add_card(&mut conn, &session, "待辦", "上一輪完成").await;
    write::card_move(&mut conn, &session, old, "完成", None)
        .await
        .unwrap();
    let levels = only(RuleId::R4, Level::Block);

    // Without a turn, every done card is checked.
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Block(results) => assert_eq!(failed(&results, RuleId::R4), [Some(old)]),
        other => panic!("{other:?}"),
    }

    // The block wrote no turn.summary, so the turn has not ended: a new prompt does not cut
    // it and the card is still checked (docs/hooks.md 2.4).
    start_turn(&mut conn, &session, "t1").await;
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Block(_)
    ));
    // The turn ends (a stop that passes writes the summary).
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R4, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    start_turn(&mut conn, &session, "t2").await;
    // Entered done before this turn: not checked.
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Pass { .. }
    ));

    let three = add_card(&mut conn, &session, "實作", "重現問題").await;
    write::card_move(&mut conn, &session, three, "完成", None)
        .await
        .unwrap();
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Block(results) => {
            assert_eq!(failed(&results, RuleId::R4), [Some(three)]);
            assert!(
                rules::block_text(&results, rules::STOP_HEADER).ends_with(&format!(
                    "R4: card {three} \"重現問題\" is done but has no result.\n    \
                 Fix: kb card done {three} --result \"<result>\""
                ))
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R4, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    write::card_done(&mut conn, &session, three, "可以穩定重現")
        .await
        .unwrap();
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Pass { .. }
    ));
}

#[tokio::test]
async fn r5_file_edits_need_card_activity() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片").await;
    let levels = only(RuleId::R5, Level::Block);
    let edit = json!({"file_path": "a.rs"});

    // No user.prompt: not checked even with edits.
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Pass { .. }
    ));

    start_turn(&mut conn, &session, "t1").await;
    // Shell commands are not file edits.
    hook::tool_used(
        &mut conn,
        &session,
        "Bash",
        &json!({"command": "cargo test"}),
        Some(5),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        hook::stop(&mut conn, &session, &levels).await.unwrap(),
        Stop::Pass { .. }
    ));

    hook::tool_used(&mut conn, &session, "Write", &edit, None, None)
        .await
        .unwrap();
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Block(results) => assert_eq!(failed(&results, RuleId::R5), [None]),
        other => panic!("{other:?}"),
    }
    // A note fixes it; the summary counts this turn only (since the previous stop that
    // passed): the edit, not the shell command of the turn before.
    write::card_note(&mut conn, &session, seq, NoteKind::Progress, "改了 a.rs")
        .await
        .unwrap();
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Pass { summary, .. } => {
            assert_eq!(summary, json!({"cards_changed": 1, "notes": 1, "tools": 1}))
        }
        other => panic!("{other:?}"),
    }

    // Warn level: passes and records check.warn.
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R5, Level::Warn))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    let b = board(&mut conn, &session).await;
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].1["rule"], "R5");
}

/// A board whose stored `cwd` (the project folder) is `project`.
async fn board_in(conn: &mut PgConnection, project: &str) -> String {
    let session = common::unique("p2-proj");
    let args = BoardInit {
        title: Some("專案看板".into()),
        goal: None,
        lanes: Some(write::parse_lanes(LANES).unwrap()),
        epics: None,
        cwd: Some(project.to_string()),
    };
    let ws = manual_ws(&common::unique("ws"));
    write::board_init(conn, &session, args, || ws)
        .await
        .unwrap();
    session
}

/// The R5 sentence when the stop blocks only on R5; `None` when it passes.
async fn r5_stop(conn: &mut PgConnection, session: &str) -> Option<String> {
    match hook::stop(conn, session, &only(RuleId::R5, Level::Block))
        .await
        .unwrap()
    {
        Stop::Pass { .. } => None,
        Stop::Block(results) => Some(
            results
                .iter()
                .find(|r| r.rule == RuleId::R5)
                .unwrap()
                .violations[0]
                .text
                .clone(),
        ),
        other => panic!("{other:?}"),
    }
}

/// R5 of the current turn (no write), then the turn ends (a stop that passes writes the
/// summary). The R5 sentence, or `None` when R5 passes.
async fn r5_then_end(conn: &mut PgConnection, session: &str) -> Option<String> {
    let b = board(conn, session).await;
    let r5 = rules::check_turn(conn, &b, &only(RuleId::R5, Level::Warn))
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.rule == RuleId::R5)
        .unwrap();
    assert!(r5.checked);
    assert!(matches!(
        hook::stop(conn, session, &only(RuleId::R5, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    r5.violations.first().map(|v| v.text.clone())
}

/// One edit through the hook; returns the row's `detail.scratch` (`Null` when absent).
async fn edit_file(
    conn: &mut PgConnection,
    session: &str,
    path: &Path,
    cwd: &str,
) -> serde_json::Value {
    let input = json!({ "file_path": path.to_string_lossy() });
    assert!(
        hook::tool_used(conn, session, "Edit", &input, None, Some(cwd))
            .await
            .unwrap()
    );
    let b = board(conn, session).await;
    let row = &last_rows(conn, b.id, 1).await[0];
    assert_eq!(row.0, "tool.use");
    assert!(row.1.get("in_project").is_none(), "{:?}", row.1);
    row.1.get("scratch").cloned().unwrap_or_default()
}

/// A project folder that is in no scratch place (it does not need to exist).
fn plain_project() -> std::path::PathBuf {
    let root = if cfg!(windows) { r"Z:\kb-test" } else { "/kb-test" };
    Path::new(root).join(common::unique("proj"))
}

/// The home directory as kb sees it (`USERPROFILE`, else `HOME`).
fn home() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
        .map(std::path::PathBuf::from)
        .expect("a home directory")
}

const R5_ONE: &str = "R5: files were edited this turn (1 edits) but no card has activity this turn.";

#[tokio::test]
async fn r5_does_not_count_scratch_edits() {
    // docs/hooks.md sections 3.4 and 4: every edit counts except scratch ones.
    let mut conn = common::writer().await;
    let project = plain_project();
    let p = project.to_string_lossy().to_string();
    let session = board_in(&mut conn, &p).await;
    start_turn(&mut conn, &session, "t1").await;
    let tmp_file = std::env::temp_dir()
        .join(common::unique("scratch"))
        .join("notes.md");
    let memory = home().join(".claude").join("projects").join("MEMORY.md");

    // The temporary folder and a hidden top-level folder of home: scratch, R5 passes.
    assert_eq!(edit_file(&mut conn, &session, &tmp_file, &p).await, true);
    assert_eq!(edit_file(&mut conn, &session, &memory, &p).await, true);
    assert_eq!(r5_then_end(&mut conn, &session).await, None);

    // A file of the project (relative to the hook's cwd): counts.
    let inside = Path::new("src").join("lib.rs");
    assert_eq!(edit_file(&mut conn, &session, &inside, &p).await, json!(null));
    assert_eq!(r5_then_end(&mut conn, &session).await.as_deref(), Some(R5_ONE));

    // A file outside the project, on another drive or path: counts.
    let elsewhere = if cfg!(windows) {
        Path::new(r"Y:\other").join("main.rs")
    } else {
        Path::new("/opt/other").join("main.rs")
    };
    assert_eq!(edit_file(&mut conn, &session, &elsewhere, &p).await, json!(null));
    assert_eq!(r5_then_end(&mut conn, &session).await.as_deref(), Some(R5_ONE));

    // Mixed: only the edits that are not scratch are counted; a shell command is no edit.
    for path in [&tmp_file, &project.join("a.rs"), &memory, &elsewhere] {
        edit_file(&mut conn, &session, path, &p).await;
    }
    hook::tool_used(
        &mut conn,
        &session,
        "Bash",
        &json!({"command": "cargo test"}),
        None,
        Some(&p),
    )
    .await
    .unwrap();
    let b = board(&mut conn, &session).await;
    assert!(last_rows(&mut conn, b.id, 1).await[0].1.get("scratch").is_none());
    assert_eq!(
        r5_then_end(&mut conn, &session).await.as_deref(),
        Some("R5: files were edited this turn (2 edits) but no card has activity this turn.")
    );

    // Rows written before `scratch` existed: `in_project: false` does not count, `true`
    // and rows with neither field count.
    let legacy = |detail: serde_json::Value| {
        sqlx::query(
            "INSERT INTO activity (board_id, actor, verb, detail, turn_key)
             VALUES ($1, 'agent', 'tool.use', $2, 'legacy')",
        )
        .bind(b.id)
        .bind(sqlx::types::Json(detail))
    };
    let target = memory.to_string_lossy().to_string();
    legacy(json!({"tool": "Write", "target": target, "in_project": false}))
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(r5_then_end(&mut conn, &session).await, None);
    legacy(json!({"tool": "Write", "target": target, "in_project": true}))
        .execute(&mut conn)
        .await
        .unwrap();
    legacy(json!({"tool": "Write", "target": target, "duration_ms": null}))
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        r5_then_end(&mut conn, &session).await.as_deref(),
        Some("R5: files were edited this turn (2 edits) but no card has activity this turn.")
    );
}

#[tokio::test]
async fn a_project_in_a_scratch_place_counts_its_own_files() {
    let mut conn = common::writer().await;
    // The project lives in the temporary folder: its files count, the rest of the
    // temporary folder and ~/.claude stay scratch.
    let tmp = std::env::temp_dir();
    let project = tmp.join(common::unique("專案"));
    let p = project.to_string_lossy().to_string();
    let session = board_in(&mut conn, &p).await;
    start_turn(&mut conn, &session, "t1").await;
    let own = project.join("src").join("main.rs");
    assert_eq!(edit_file(&mut conn, &session, &own, &p).await, json!(null));
    assert_eq!(
        edit_file(&mut conn, &session, Path::new("src/lib.rs"), &p).await,
        json!(null)
    );
    let other = tmp.join(common::unique("scratch")).join("x.md");
    assert_eq!(edit_file(&mut conn, &session, &other, &p).await, true);
    let memory = home().join(".claude").join("x.md");
    assert_eq!(edit_file(&mut conn, &session, &memory, &p).await, true);
    assert_eq!(
        r5_then_end(&mut conn, &session).await.as_deref(),
        Some("R5: files were edited this turn (2 edits) but no card has activity this turn.")
    );

    // The home directory as the project does not rescue its hidden folders.
    let h = home();
    let hp = h.to_string_lossy().to_string();
    let session = board_in(&mut conn, &hp).await;
    start_turn(&mut conn, &session, "t1").await;
    let settings = h.join(".claude").join("settings.json");
    assert_eq!(edit_file(&mut conn, &session, &settings, &hp).await, true);
    assert_eq!(r5_then_end(&mut conn, &session).await, None);
    let src = h.join("src").join("x").join("main.rs");
    assert_eq!(edit_file(&mut conn, &session, &src, &hp).await, json!(null));
    assert_eq!(r5_then_end(&mut conn, &session).await.as_deref(), Some(R5_ONE));

    // A board without a stored cwd: the places still apply, with no project to rescue a
    // file.
    let plain = new_board(&mut conn).await;
    start_turn(&mut conn, &plain, "t1").await;
    assert_eq!(edit_file(&mut conn, &plain, &other, &p).await, true);
    assert_eq!(edit_file(&mut conn, &plain, &own, &p).await, true);
    let elsewhere = plain_project().join("a.rs");
    assert_eq!(edit_file(&mut conn, &plain, &elsewhere, &p).await, json!(null));
    assert_eq!(r5_then_end(&mut conn, &plain).await.as_deref(), Some(R5_ONE));
}

async fn note_on(conn: &mut PgConnection, session: &str, seq: i32) {
    write::card_note(conn, session, seq, NoteKind::Progress, "紀錄")
        .await
        .unwrap();
}

fn summary_of(stop: Stop) -> serde_json::Value {
    match stop {
        Stop::Pass { summary, .. } | Stop::AfterBlock { summary, .. } => summary,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_rules_turn_runs_from_the_previous_end_of_a_turn() {
    // docs/hooks.md 2.4: the turn the rules check starts after the latest turn.summary of
    // the stop hook; user.prompt does not move it.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片").await;
    let levels = only(RuleId::R5, Level::Block);
    let edit = json!({"file_path": "a.rs"});

    // A normal turn: a note and an edit.
    start_turn(&mut conn, &session, "t1").await;
    note_on(&mut conn, &session, seq).await;
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    let s = summary_of(hook::stop(&mut conn, &session, &levels).await.unwrap());
    assert_eq!(s, json!({"cards_changed": 1, "notes": 1, "tools": 1}));

    // A wake turn (no new turn from the prompt hook) is checked on its own rows: the note
    // of the turn before does not cover its edit.
    hook::notification(&mut conn, &session).await.unwrap();
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    assert_eq!(r5_stop(&mut conn, &session).await.as_deref(), Some(R5_ONE));
    // After the block the turn continues: a note, and the stop passes. The summary counts
    // the whole wake turn.
    note_on(&mut conn, &session, seq).await;
    let s = summary_of(hook::stop(&mut conn, &session, &levels).await.unwrap());
    assert_eq!(s, json!({"cards_changed": 1, "notes": 1, "tools": 1}));

    // A wake turn with only a card note: no warning, and the summary counts only it.
    hook::notification(&mut conn, &session).await.unwrap();
    note_on(&mut conn, &session, seq).await;
    let s = summary_of(hook::stop(&mut conn, &session, &levels).await.unwrap());
    assert_eq!(s, json!({"cards_changed": 1, "notes": 1, "tools": 0}));

    // A message sent while the agent is busy does not cut the turn: the note before it
    // covers the edit after it ...
    start_turn(&mut conn, &session, "t2").await;
    note_on(&mut conn, &session, seq).await;
    start_turn(&mut conn, &session, "t2-busy").await;
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    assert_eq!(r5_stop(&mut conn, &session).await, None);
    // ... and an edit before it still counts.
    start_turn(&mut conn, &session, "t3").await;
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    start_turn(&mut conn, &session, "t3-busy").await;
    assert_eq!(r5_stop(&mut conn, &session).await.as_deref(), Some(R5_ONE));
    summary_of(
        hook::stop_after_block(&mut conn, &session, &levels)
            .await
            .unwrap(),
    );

    // A turn interrupted with Esc (no Stop, no summary) is checked with the next turn.
    start_turn(&mut conn, &session, "t4").await;
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    start_turn(&mut conn, &session, "t5").await;
    assert_eq!(r5_stop(&mut conn, &session).await.as_deref(), Some(R5_ONE));

    // The turn.summary of `kb board tidy` (the agent, mid-turn) is not an end of a turn.
    write::board_tidy(&mut conn, &session).await.unwrap();
    assert_eq!(r5_stop(&mut conn, &session).await.as_deref(), Some(R5_ONE));
}

/// The cards R4 reports for the board's current turn (no write).
async fn r4_cards(conn: &mut PgConnection, session: &str) -> Vec<Option<i32>> {
    let b = board(conn, session).await;
    let results = rules::check_turn(conn, &b, &only(RuleId::R4, Level::Block))
        .await
        .unwrap();
    failed(&results, RuleId::R4)
}

/// A card moved into the done lane without a result, closed three days ago.
async fn done_days_ago(conn: &mut PgConnection, session: &str, title: &str) -> i32 {
    let seq = add_card(conn, session, "待辦", title).await;
    write::card_move(conn, session, seq, "完成", None)
        .await
        .unwrap();
    let b = board(conn, session).await;
    sqlx::query(
        "UPDATE card SET closed_at = now() - interval '3 days',
                         updated_at = now() - interval '3 days'
         WHERE board_id = $1 AND seq = $2",
    )
    .bind(b.id)
    .bind(seq)
    .execute(&mut *conn)
    .await
    .unwrap();
    seq
}

async fn session_starts(conn: &mut PgConnection, session: &str, source: &str) {
    hook::session_start(conn, session, source, || None)
        .await
        .unwrap()
        .expect("board exists");
}

#[tokio::test]
async fn without_an_end_of_a_turn_the_rules_turn_starts_at_the_session_start() {
    // docs/hooks.md 2.4: a board without a turn.summary of the stop hook (an old board, or
    // a session restarted before any turn ended cleanly).
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let old = done_days_ago(&mut conn, &session, "幾天前完成").await;
    // No start marker at all: the turn covers the board's whole history.
    assert_eq!(r4_cards(&mut conn, &session).await, [Some(old)]);

    // The session starts: the card done days earlier is not part of the turn.
    session_starts(&mut conn, &session, "resume").await;
    assert!(r4_cards(&mut conn, &session).await.is_empty());
    start_turn(&mut conn, &session, "t1").await;
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R4, Level::Block))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));

    // A card done in the next turn is still checked.
    start_turn(&mut conn, &session, "t2").await;
    let new = add_card(&mut conn, &session, "待辦", "這一輪完成").await;
    write::card_move(&mut conn, &session, new, "完成", None)
        .await
        .unwrap();
    assert_eq!(r4_cards(&mut conn, &session).await, [Some(new)]);
}

#[tokio::test]
async fn a_later_session_start_wins_over_an_older_end_of_a_turn() {
    // docs/hooks.md 2.4: the turn starts after the latest start marker. Each source of a
    // session start except `compact` starts between turns.
    for source in ["startup", "resume", "clear", "fork"] {
        let mut conn = common::writer().await;
        let session = new_board(&mut conn).await;
        start_turn(&mut conn, &session, "t1").await;
        assert!(matches!(
            hook::stop(&mut conn, &session, &only(RuleId::R4, Level::Off))
                .await
                .unwrap(),
            Stop::Pass { .. }
        ));
        // A turn interrupted with Esc (no stop, no summary), then the session ends.
        start_turn(&mut conn, &session, "t2").await;
        let left = add_card(&mut conn, &session, "待辦", "中斷的那一輪").await;
        write::card_move(&mut conn, &session, left, "完成", None)
            .await
            .unwrap();
        assert_eq!(
            r4_cards(&mut conn, &session).await,
            [Some(left)],
            "{source}"
        );

        session_starts(&mut conn, &session, source).await;
        assert!(r4_cards(&mut conn, &session).await.is_empty(), "{source}");

        // The next end of a turn is again the latest marker.
        start_turn(&mut conn, &session, "t3").await;
        let late = add_card(&mut conn, &session, "待辦", "下一輪").await;
        write::card_move(&mut conn, &session, late, "完成", None)
            .await
            .unwrap();
        assert!(matches!(
            hook::stop(&mut conn, &session, &only(RuleId::R4, Level::Off))
                .await
                .unwrap(),
            Stop::Pass { .. }
        ));
        assert!(r4_cards(&mut conn, &session).await.is_empty(), "{source}");
    }
}

#[tokio::test]
async fn a_compaction_does_not_start_the_rules_turn() {
    // docs/hooks.md 2.4: a compaction can come in the middle of a turn; the work done
    // before it stays in the turn.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let seq = add_card(&mut conn, &session, "待辦", "卡片").await;
    session_starts(&mut conn, &session, "startup").await;
    start_turn(&mut conn, &session, "t1").await;
    let edit = json!({"file_path": "a.rs"});
    hook::tool_used(&mut conn, &session, "Edit", &edit, None, None)
        .await
        .unwrap();
    session_starts(&mut conn, &session, "compact").await;
    // R5 still sees the edit made before the compaction.
    assert_eq!(r5_stop(&mut conn, &session).await.as_deref(), Some(R5_ONE));
    note_on(&mut conn, &session, seq).await;
    let s = summary_of(
        hook::stop(&mut conn, &session, &only(RuleId::R5, Level::Block))
            .await
            .unwrap(),
    );
    assert_eq!(s, json!({"cards_changed": 1, "notes": 1, "tools": 1}));

    // R4 still sees a card done before the compaction.
    let session = new_board(&mut conn).await;
    session_starts(&mut conn, &session, "startup").await;
    start_turn(&mut conn, &session, "t1").await;
    let done = add_card(&mut conn, &session, "待辦", "壓縮前完成").await;
    write::card_move(&mut conn, &session, done, "完成", None)
        .await
        .unwrap();
    session_starts(&mut conn, &session, "compact").await;
    assert_eq!(r4_cards(&mut conn, &session).await, [Some(done)]);

    // A compaction alone is no start: without another marker the whole history counts.
    let session = new_board(&mut conn).await;
    let old = done_days_ago(&mut conn, &session, "幾天前完成").await;
    session_starts(&mut conn, &session, "compact").await;
    assert_eq!(r4_cards(&mut conn, &session).await, [Some(old)]);
}

#[tokio::test]
async fn the_turn_board_start_begins_is_a_start_of_the_rules_turn() {
    // docs/board-v2.md 12.4 and docs/hooks.md 2.4: `kb board start` reopening a board
    // starts the turn; what was done before the board was archived is not part of it.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let old = done_days_ago(&mut conn, &session, "封存前完成").await;
    assert_eq!(r4_cards(&mut conn, &session).await, [Some(old)]);
    write::board_archive(&mut conn, &session, None, None)
        .await
        .unwrap();
    let start = write::board_start(&mut conn, &session, start_args(None, false), None, || {
        panic!("no resolve")
    })
    .await
    .unwrap();
    assert_eq!(start.status, StartStatus::Reopened);
    assert!(r4_cards(&mut conn, &session).await.is_empty());
}

#[tokio::test]
async fn card_activity_is_what_the_agent_wrote() {
    // docs/hooks.md 4: rows of hooks and of the user, and the deliveries of `kb inbox`, are
    // not card activity for R3, R5 and `cards_changed`.
    let mut conn = common::writer().await;
    let mut reader = common::reader().await;
    let session = new_board(&mut conn).await;
    let five = add_card(&mut conn, &session, "實作", "卡片").await;
    let q = ask(&mut conn, &session, five, &["a", "b"], false).await;
    let b = board(&mut conn, &session).await;
    let card_id: i64 = sqlx::query_scalar("SELECT id FROM card WHERE board_id = $1 AND seq = $2")
        .bind(b.id)
        .bind(five)
        .fetch_one(&mut conn)
        .await
        .unwrap();
    summary_of(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
    );

    start_turn(&mut conn, &session, "t1").await;
    // A finding of the model judge (hook), the user's answer and comment, and their
    // delivery through the agent's `kb inbox`.
    sqlx::query(
        "INSERT INTO activity (board_id, card_id, actor, verb, detail, turn_key)
         VALUES ($1, $2, 'hook', 'judge.finding', '{\"question\": \"advice\"}', 't1')",
    )
    .bind(b.id)
    .bind(card_id)
    .execute(&mut conn)
    .await
    .unwrap();
    let ids = option_ids(&mut reader, b.id, q).await;
    answer::answer_question(&mut reader, q, &ids[..1], false, None)
        .await
        .unwrap();
    kb_core::comment::add_comment(&mut reader, card_id, "先查 log")
        .await
        .unwrap();
    let inbox = write::inbox(&mut conn, &session, false).await.unwrap();
    assert_eq!((inbox.answers.len(), inbox.comments.len()), (1, 1));
    hook::tool_used(
        &mut conn,
        &session,
        "Edit",
        &json!({"file_path": "a.rs"}),
        None,
        None,
    )
    .await
    .unwrap();

    let mut levels = only(RuleId::R5, Level::Block);
    levels.set(RuleId::R3, Level::Block);
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Block(results) => {
            assert_eq!(failed(&results, RuleId::R3), [Some(five)]);
            assert_eq!(failed(&results, RuleId::R5), [None]);
        }
        other => panic!("{other:?}"),
    }
    // The agent's reply note is card activity.
    write::card_note(&mut conn, &session, five, NoteKind::Reply, "好，先查 log")
        .await
        .unwrap();
    let s = summary_of(hook::stop(&mut conn, &session, &levels).await.unwrap());
    assert_eq!((&s["cards_changed"], &s["tools"]), (&json!(1), &json!(1)));

    // A turn with only rows of others on the card: no card changed.
    start_turn(&mut conn, &session, "t2").await;
    kb_core::comment::add_comment(&mut reader, card_id, "還有一件事")
        .await
        .unwrap();
    let s = summary_of(
        hook::stop_after_block(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
    );
    assert_eq!(s["cards_changed"], 0);
}

#[tokio::test]
async fn the_board_start_turn_is_checked() {
    // docs/board-v2.md section 12.4: `kb board start` starts the turn of the /kb prompt.
    let mut conn = common::writer().await;
    let project = std::env::temp_dir().join(common::unique("kb-turn"));
    let p = project.to_string_lossy().to_string();
    let s = common::unique("kb-turn");
    let start = write::board_start(&mut conn, &s, start_args(Some(&p), true), None, || {
        manual_ws(&common::unique("ws"))
    })
    .await
    .unwrap();
    assert_eq!(start.status, StartStatus::Created);
    let b = start.board.unwrap();
    let key = b.turn_key.clone().unwrap();
    let rows = last_rows(&mut conn, b.id, 1).await;
    assert_eq!(rows[0].0, "user.prompt");
    assert_eq!(
        rows[0].1,
        json!({"excerpt": "kb board start", "source": "board.start"})
    );

    // The prompt hook of the same /kb prompt (if it runs after the start) keeps the turn.
    let p1 = hook::prompt(&mut conn, &s, Some("prompt-kb"), "/kb 修正登入逾時")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p1.turn_key, key);
    let after = board(&mut conn, &s).await;
    assert_eq!(
        (after.turn_key.as_deref(), after.turn_started_at, after.rev),
        (Some(key.as_str()), b.turn_started_at, b.rev)
    );
    assert_eq!(
        activity_verbs(&mut conn, b.id).await,
        ["user.prompt", "board.create"]
    );

    // The agent designs the board and edits a file of the project without card activity:
    // R5 is checked in this turn.
    write::board_init(
        &mut conn,
        &s,
        BoardInit {
            title: Some("看板".into()),
            lanes: Some(write::parse_lanes(LANES).unwrap()),
            ..Default::default()
        },
        || panic!("the board exists"),
    )
    .await
    .unwrap();
    let mut levels = Levels::default();
    levels.set(RuleId::R5, Level::Block);
    let b2 = board(&mut conn, &s).await;
    let r5 = rules::check_turn(&mut conn, &b2, &levels)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.rule == RuleId::R5)
        .unwrap();
    assert!(r5.checked && r5.passed, "{r5:?}");
    hook::tool_used(
        &mut conn,
        &s,
        "Edit",
        &json!({"file_path": project.join("a.rs").to_string_lossy()}),
        None,
        Some(&p),
    )
    .await
    .unwrap();
    assert!(r5_stop(&mut conn, &s).await.is_some());
    // The turn's one block; after it, the stop records and ends the turn.
    assert!(matches!(
        hook::stop_after_block(&mut conn, &s, &levels)
            .await
            .unwrap(),
        Stop::AfterBlock { .. }
    ));

    // The turn is over and no prompt hook has run on this board: not checked any more.
    let b3 = board(&mut conn, &s).await;
    let r5 = rules::check_turn(&mut conn, &b3, &levels)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.rule == RuleId::R5)
        .unwrap();
    assert!(!r5.checked);

    // The next human prompt starts a new turn, and shows the /kb turn's warning (the
    // start's user.prompt is not a boundary for the reminders).
    let next = hook::prompt(&mut conn, &s, Some("prompt-2"), "繼續")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.turn_key, "prompt-2");
    assert_eq!(
        next.reminders
            .iter()
            .map(|r| r.rule.as_str())
            .collect::<Vec<_>>(),
        ["R5"]
    );
    // A later /kb on the active board: the start changes nothing, and the prompt hook
    // starts a turn as usual (the latest row is not the start's user.prompt).
    let again = write::board_start(&mut conn, &s, start_args(Some(&p), false), None, || {
        panic!("no resolve")
    })
    .await
    .unwrap();
    assert_eq!(again.status, StartStatus::Active);
    let p3 = hook::prompt(&mut conn, &s, Some("prompt-3"), "/kb")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p3.turn_key, "prompt-3");
}

#[tokio::test]
async fn stop_delivers_answers_before_checking_rules() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let five = add_card(&mut conn, &session, "實作", "卡片").await;
    let six = add_card(&mut conn, &session, "實作", "另一張").await;
    let q = ask(&mut conn, &session, five, &["a", "b"], false).await;
    // The turn that added the cards ends; in the next one both doing cards violate R3.
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    start_turn(&mut conn, &session, "t1").await;
    let b = board(&mut conn, &session).await;
    let mut reader = common::reader().await;
    answer::answer_question(&mut reader, q, &[], true, None)
        .await
        .unwrap();

    let mut levels = Levels::default();
    levels.set(RuleId::R3, Level::Block);
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Deliver(d) => {
            assert_eq!(d.answers.len(), 1);
            assert_eq!(d.answers[0].question.id, q);
            assert_eq!(d.open_questions, 0);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "question.deliver");
    // Delivered; the rules come next. The delivery (and the user's answer) on card 5 are not
    // the agent's card activity, so both cards fail.
    match hook::stop(&mut conn, &session, &levels).await.unwrap() {
        Stop::Block(results) => {
            assert_eq!(failed(&results, RuleId::R3), [Some(five), Some(six)])
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn rule_levels_from_file_text() {
    let (l, w) = Levels::parse("");
    assert_eq!(l, Levels::default());
    assert!(w.is_empty());
    assert_eq!(
        RuleId::ALL.map(|r| Levels::default().get(r)),
        [
            Level::Warn,
            Level::Warn,
            Level::Off,
            Level::Block,
            Level::Warn,
            Level::Block,
            // R7-R10 (docs/board-v2.md section 17.1).
            Level::Warn,
            Level::Warn,
            Level::Warn,
            Level::Warn,
            // R11 (docs/board-v2.md section 19.4).
            Level::Warn,
        ]
    );
    let (l, w) = Levels::parse(
        "[rules]\nR1 = \"block\"\nR6 = \"warn\"\nR9 = \"off\"\nR10 = \"block\"\nR99 = \"off\"\n",
    );
    assert_eq!(
        (l.get(RuleId::R1), l.get(RuleId::R6), l.get(RuleId::R3)),
        (Level::Block, Level::Warn, Level::Off)
    );
    assert_eq!(
        (l.get(RuleId::R7), l.get(RuleId::R9), l.get(RuleId::R10)),
        (Level::Warn, Level::Off, Level::Block)
    );
    assert!(w.is_empty());
    let (l, w) = Levels::parse("[rules]\nR3 = \"loud\"\nR2 = 1\n");
    assert_eq!(
        (l.get(RuleId::R3), l.get(RuleId::R2)),
        (Level::Off, Level::Warn)
    );
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("R2") || w[1].contains("R2"));
    let (l, w) = Levels::parse("not toml [");
    assert_eq!(l, Levels::default());
    assert_eq!(w.len(), 1);
}

#[test]
fn tool_targets_and_kb_commands() {
    assert_eq!(
        hook::tool_target("Edit", &json!({"file_path": "src/a.rs"})),
        "src/a.rs"
    );
    assert_eq!(
        hook::tool_target("NotebookEdit", &json!({"notebook_path": "n.ipynb"})),
        "n.ipynb"
    );
    let long = "x".repeat(100);
    assert_eq!(
        hook::tool_target("Bash", &json!({"command": long})).len(),
        80
    );
    assert_eq!(hook::tool_target("Grep", &json!({"pattern": "x"})), "");
    for cmd in [
        "kb card note 1 x",
        "  kb board show",
        "kb.exe inbox",
        "& kb ask --card 1 q",
        "D:/bin/kb.exe card add",
        r"'C:\tools\kb.exe' inbox",
        "KB",
    ] {
        assert!(hook::is_kb_command(cmd), "{cmd}");
    }
    for cmd in ["cargo test", "kbx", "echo kb", "git status"] {
        assert!(!hook::is_kb_command(cmd), "{cmd}");
    }
    let input = hook::HookInput::parse(
        r#"{"session_id":" s1 ","stop_hook_active":true,"duration_ms":12.4,"extra":[1]}"#,
    )
    .unwrap();
    assert_eq!(input.session_id, "s1");
    assert!(input.stop_hook_active);
    assert_eq!(input.duration_ms, Some(12));
    assert_eq!(hook::HookInput::parse("{}").unwrap().session_id, "");
    assert!(hook::HookInput::parse("[1]").is_none());
    assert!(hook::HookInput::parse("nope").is_none());
    assert_eq!(hook::normalize_source(Some("weird")), "startup");
    assert_eq!(hook::normalize_source(Some("compact")), "compact");
}

// --- session-start / session-end -------------------------------------------------------------

fn term(terminal: &str, space: Option<&str>) -> TerminalInfo {
    TerminalInfo {
        terminal: terminal.to_string(),
        space: space.map(str::to_string),
        tab: None,
    }
}

fn start_args(cwd: Option<&str>, new: bool) -> BoardStartArgs {
    BoardStartArgs {
        cwd: cwd.map(str::to_string),
        new,
        ..Default::default()
    }
}

#[tokio::test]
async fn session_start_needs_a_board_that_board_start_creates() {
    let mut conn = common::writer().await;
    let terminal = common::unique("term");
    let space = common::unique("space");
    let ws = || {
        resolve::resolve_workspace(
            None,
            Some(SpaceContext {
                space: space.clone(),
                name: "space".into(),
            }),
            Path::new("."),
        )
    };

    // No board: session-start writes nothing and does not look at the terminal.
    let s1 = common::unique("s1");
    let none = hook::session_start(&mut conn, &s1, "startup", || panic!("no terminal"))
        .await
        .unwrap();
    assert!(none.is_none());
    assert!(
        read::board_by_session(&mut conn, &s1)
            .await
            .unwrap()
            .is_none()
    );

    // kb board start creates the board (docs/board-v2.md section 12.4).
    let t = term(&terminal, Some(&space));
    let start = write::board_start(
        &mut conn,
        &s1,
        start_args(Some("D:/work"), false),
        Some(&t),
        ws,
    )
    .await
    .unwrap();
    assert_eq!(start.status, StartStatus::Created);
    let b1 = start.board.clone().unwrap();
    assert_eq!(b1.title, "");
    assert_eq!(b1.cwd.as_deref(), Some("D:/work"));
    assert_eq!(b1.terminal_id.as_deref(), Some(terminal.as_str()));
    assert_eq!(b1.terminal_space.as_deref(), Some(space.as_str()));
    assert_eq!(b1.prev_board_id, None);
    // The start also starts the turn: a turn key, its start time, a user.prompt.
    assert_eq!(
        activity_verbs(&mut conn, b1.id).await,
        ["user.prompt", "board.create"]
    );
    assert!(b1.turn_key.is_some() && b1.turn_started_at.is_some());

    // Already active: no change, no activity, the same turn.
    let again = write::board_start(&mut conn, &s1, start_args(None, true), Some(&t), || {
        panic!("no resolve")
    })
    .await
    .unwrap();
    assert_eq!(again.status, StartStatus::Active);
    assert_eq!(
        activity_verbs(&mut conn, b1.id).await,
        ["user.prompt", "board.create"]
    );
    let b1_again = again.board.clone().unwrap();
    assert_eq!(
        (b1_again.turn_key, b1_again.turn_started_at, b1_again.rev),
        (b1.turn_key.clone(), b1.turn_started_at, b1.rev)
    );

    // Source 4 of the session key: the terminal's latest active board.
    let key = resolve::resolve_session(&mut conn, SessionSource::Terminal(terminal.clone()))
        .await
        .unwrap();
    assert_eq!(key, s1);
    let err = resolve::resolve_session(&mut conn, SessionSource::Terminal(common::unique("t")))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NoSession));
    let env = |k: &str| (k == "KB_TERMINAL_ID").then(|| "t9".to_string());
    assert_eq!(
        resolve::session_source(None, env).unwrap(),
        SessionSource::Terminal("t9".into())
    );
    assert_eq!(
        resolve::session_source(Some("flag"), env).unwrap(),
        SessionSource::Key("flag".into())
    );

    // Undelivered answers are delivered by session-start; the terminal fields are refreshed
    // to the current terminal, even without a terminal space (section 12.7).
    let args = BoardInit {
        title: Some("有泳道".into()),
        lanes: Some(write::parse_lanes(LANES).unwrap()),
        ..Default::default()
    };
    write::board_init(&mut conn, &s1, args, || panic!("exists"))
        .await
        .unwrap();
    let seq = add_card(&mut conn, &s1, "實作", "卡片").await;
    let q = ask(&mut conn, &s1, seq, &["a", "b"], false).await;
    let mut reader = common::reader().await;
    answer::answer_question(&mut reader, q, &[], true, None)
        .await
        .unwrap();
    let moved = common::unique("moved");
    let started = hook::session_start(&mut conn, &s1, "compact", || Some(term(&moved, None)))
        .await
        .unwrap()
        .unwrap();
    assert!(!started.reopened);
    assert_eq!(started.board.id, b1.id);
    assert_eq!(started.delivery.answers.len(), 1);
    assert_eq!(started.delivery.answers[0].question.id, q);
    assert_eq!(started.board.terminal_id.as_deref(), Some(moved.as_str()));
    assert_eq!(started.board.terminal_space, None);
    assert_eq!(
        activity_verbs(&mut conn, b1.id).await[..2],
        ["question.deliver", "session.start"]
    );
    // An unknown terminal leaves the fields alone.
    let kept = hook::session_start(&mut conn, &s1, "resume", || None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.board.terminal_id.as_deref(), Some(moved.as_str()));

    // A second session in the same terminal sees the first board as a candidate and gets
    // no board, unless --new; then prev_board_id links them.
    let tm = term(&moved, None);
    let s2 = common::unique("s2");
    let listed = write::board_start(&mut conn, &s2, start_args(None, false), Some(&tm), || {
        panic!("no board is created")
    })
    .await
    .unwrap();
    assert_eq!(listed.status, StartStatus::Candidates);
    assert!(listed.board.is_none());
    assert_eq!(
        listed.candidates.iter().map(|b| b.id).collect::<Vec<_>>(),
        [b1.id]
    );
    assert!(
        read::board_by_session(&mut conn, &s2)
            .await
            .unwrap()
            .is_none()
    );
    let fresh = write::board_start(&mut conn, &s2, start_args(None, true), Some(&tm), || {
        manual_ws("fresh")
    })
    .await
    .unwrap();
    assert_eq!(fresh.status, StartStatus::Created);
    let b2 = fresh.board.unwrap();
    assert_eq!(b2.prev_board_id, Some(b1.id));
    // The terminal is stored even with a manual workspace (section 12.7).
    assert_eq!(b2.terminal_id.as_deref(), Some(moved.as_str()));

    // session-end archives (open cards: incomplete); session-start reopens and clears the
    // archive summary (section 12.6).
    let outcome = hook::session_end(&mut conn, &s1, "clear").await.unwrap();
    assert_eq!(outcome, Some(Outcome::Incomplete));
    let b = board(&mut conn, &s1).await;
    assert_eq!(b.status, BoardStatus::Archived);
    assert_eq!(b.summary.as_deref(), Some("session ended: clear"));
    let resumed = hook::session_start(&mut conn, &s1, "resume", || None)
        .await
        .unwrap()
        .unwrap();
    assert!(resumed.reopened);
    assert_eq!(resumed.board.status, BoardStatus::Active);
    assert_eq!(
        (
            resumed.board.outcome,
            resumed.board.summary.as_deref(),
            resumed.board.archived_at
        ),
        (None, None, None)
    );
    assert_eq!(
        activity_verbs(&mut conn, b.id).await[..3],
        ["session.start", "board.reopen", "board.archive"]
    );

    // kb board start reopens an archived board too.
    hook::session_end(&mut conn, &s1, "other").await.unwrap();
    let reopened = write::board_start(&mut conn, &s1, start_args(None, false), None, || {
        panic!("no resolve")
    })
    .await
    .unwrap();
    assert_eq!(reopened.status, StartStatus::Reopened);
    let rb = reopened.board.unwrap();
    assert_eq!(rb.summary, None);
    // Reopening starts a new turn as well.
    assert_eq!(
        activity_verbs(&mut conn, rb.id).await[..2],
        ["user.prompt", "board.reopen"]
    );
    assert_ne!(rb.turn_key, b1.turn_key);
    // kb board reopen clears the summary as well.
    write::board_archive(&mut conn, &s1, None, Some("總結".into()))
        .await
        .unwrap();
    write::board_reopen(&mut conn, &s1).await.unwrap();
    assert_eq!(board(&mut conn, &s1).await.summary, None);
}

#[tokio::test]
async fn start_candidates_match_terminal_else_folder_and_skip_old_archives() {
    let mut conn = common::writer().await;
    let cwd = format!("D:/{}", common::unique("dir"));
    let mk = |name: &str| (common::unique(name), manual_ws(&common::unique("ws")));
    let (old, ws_old) = mk("old");
    let (recent, ws_recent) = mk("recent");
    for (s, ws) in [(&old, ws_old), (&recent, ws_recent)] {
        write::board_start(&mut conn, s, start_args(Some(&cwd), true), None, || ws)
            .await
            .unwrap();
        hook::session_end(&mut conn, s, "other").await.unwrap();
    }
    sqlx::query("UPDATE board SET archived_at = now() - interval '8 days' WHERE session_key = $1")
        .bind(&old)
        .execute(&mut conn)
        .await
        .unwrap();
    let ids = |list: &[Board]| {
        list.iter()
            .map(|b| b.session_key.clone())
            .collect::<Vec<_>>()
    };

    let by_dir = write::start_candidates(&mut conn, "me", None, Some(&cwd))
        .await
        .unwrap();
    assert_eq!(ids(&by_dir), [recent]);
    // A known terminal is matched instead of the folder.
    let by_term = write::start_candidates(&mut conn, "me", Some(&common::unique("t")), Some(&cwd))
        .await
        .unwrap();
    assert!(by_term.is_empty());
    // At most 3, most recently changed first; the session's own board is excluded.
    let mut keys = Vec::new();
    for _ in 0..4 {
        let s = common::unique("many");
        write::board_start(&mut conn, &s, start_args(Some(&cwd), true), None, || {
            manual_ws("many")
        })
        .await
        .unwrap();
        keys.push(s);
    }
    let list = write::start_candidates(&mut conn, &keys[3], None, Some(&cwd))
        .await
        .unwrap();
    assert_eq!(
        ids(&list),
        [keys[2].clone(), keys[1].clone(), keys[0].clone()]
    );
}

#[tokio::test]
async fn board_adopt_moves_a_board_to_the_current_session() {
    let mut conn = common::writer().await;
    let old = new_board(&mut conn).await;
    add_card(&mut conn, &old, "待辦", "卡片").await;
    hook::session_end(&mut conn, &old, "other").await.unwrap();
    let id = board(&mut conn, &old).await.id;

    // Errors: unknown target; the current session already has a board.
    let new = common::unique("new");
    let err = write::board_adopt(&mut conn, &new, "no-such-session", None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "board not found: no-such-session");
    let busy = new_board(&mut conn).await;
    let busy_id = board(&mut conn, &busy).await.id;
    let err = write::board_adopt(&mut conn, &busy, &id.to_string(), None)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("session {busy} already has board {busy_id}")
    );

    // By board id: the archived board is reopened and its terminal refreshed.
    let t = term(&common::unique("t"), None);
    let adoption = write::board_adopt(&mut conn, &new, &id.to_string(), Some(&t))
        .await
        .unwrap();
    assert_eq!(adoption.from, old);
    assert!(adoption.reopened);
    assert_eq!(adoption.board.session_key, new);
    assert_eq!(adoption.board.status, BoardStatus::Active);
    assert_eq!(adoption.board.summary, None);
    assert_eq!(
        adoption.board.terminal_id.as_deref(),
        Some(t.terminal.as_str())
    );
    assert!(
        read::board_by_session(&mut conn, &old)
            .await
            .unwrap()
            .is_none()
    );
    let rows = last_rows(&mut conn, id, 2).await;
    assert_eq!(rows[0].0, "board.reopen");
    assert_eq!(
        rows[1],
        (
            "board.adopt".to_string(),
            json!({"from": old, "auto": false})
        )
    );
    let actors: Vec<String> = read::recent_activity(&mut conn, id, 2)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.actor)
        .collect();
    assert_eq!(actors, ["agent", "agent"]);

    // By session key; an active board is not reopened.
    let newer = common::unique("newer");
    let again = write::board_adopt(&mut conn, &newer, &new, None)
        .await
        .unwrap();
    assert!(!again.reopened);
    assert_eq!(again.board.id, id);

    // Automatic adoption follows the predecessors to the first one with a board.
    let latest = common::unique("latest");
    let auto = hook::auto_adopt(
        &mut conn,
        &latest,
        || vec![common::unique("no-board"), newer.clone()],
        || None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!((auto.board.id, auto.from.as_str()), (id, newer.as_str()));
    let rows = read::recent_activity(&mut conn, id, 1).await.unwrap();
    assert_eq!(
        (rows[0].actor.as_str(), &rows[0].detail),
        ("hook", &json!({"from": newer, "auto": true}))
    );
    // Nothing to adopt, or the session already has a board: no change, the predecessors
    // are not even read.
    assert!(
        hook::auto_adopt(&mut conn, &common::unique("x"), Vec::new, || None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        hook::auto_adopt(&mut conn, &latest, || panic!("not read"), || None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn session_end_outcomes() {
    let mut conn = common::writer().await;
    // All cards closed: completed.
    let s = new_board(&mut conn).await;
    let c = add_card(&mut conn, &s, "待辦", "x").await;
    write::card_done(&mut conn, &s, c, "好了").await.unwrap();
    assert_eq!(
        hook::session_end(&mut conn, &s, "logout").await.unwrap(),
        Some(Outcome::Completed)
    );
    // Open cards: incomplete.
    let s = new_board(&mut conn).await;
    add_card(&mut conn, &s, "實作", "x").await;
    assert_eq!(
        hook::session_end(&mut conn, &s, "prompt_input_exit")
            .await
            .unwrap(),
        Some(Outcome::Incomplete)
    );
    let b = board(&mut conn, &s).await;
    assert_eq!(
        activity_verbs(&mut conn, b.id).await[..2],
        ["board.archive", "session.end"]
    );
    // Already archived: only session.end is recorded.
    assert_eq!(
        hook::session_end(&mut conn, &s, "other").await.unwrap(),
        None
    );
    assert_eq!(activity_verbs(&mut conn, b.id).await[0], "session.end");
    assert_eq!(
        board(&mut conn, &s).await.outcome,
        Some(Outcome::Incomplete)
    );
    // No board.
    assert_eq!(
        hook::session_end(&mut conn, &common::unique("none"), "other")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn hooks_ignore_sessions_without_an_active_board() {
    let mut conn = common::writer().await;
    let none = common::unique("none");
    assert!(
        hook::prompt(&mut conn, &none, None, "x")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !hook::tool_used(&mut conn, &none, "Edit", &json!({}), None, None)
            .await
            .unwrap()
    );
    assert!(matches!(
        hook::stop(&mut conn, &none, &Levels::default())
            .await
            .unwrap(),
        Stop::NoBoard
    ));
    let s = new_board(&mut conn).await;
    write::board_archive(&mut conn, &s, None, None)
        .await
        .unwrap();
    assert!(
        hook::prompt(&mut conn, &s, None, "x")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !hook::tool_used(&mut conn, &s, "Edit", &json!({}), None, None)
            .await
            .unwrap()
    );
    // A kb command is not recorded.
    let s = new_board(&mut conn).await;
    assert!(
        !hook::tool_used(
            &mut conn,
            &s,
            "Bash",
            &json!({"command": "kb card note 1 x"}),
            None,
            None
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn board_tidy_renumbers_positions() {
    let mut conn = common::writer().await;
    let s = new_board(&mut conn).await;
    let a = add_card(&mut conn, &s, "待辦", "a").await;
    let b_ = add_card(&mut conn, &s, "待辦", "b").await;
    let c = add_card(&mut conn, &s, "待辦", "c").await;
    write::card_move(&mut conn, &s, a, "實作", None)
        .await
        .unwrap();
    write::card_move(&mut conn, &s, a, "待辦", None)
        .await
        .unwrap(); // a: position 4
    let out = write::board_tidy(&mut conn, &s).await.unwrap();
    assert_eq!(out.message, "board tidied: 3 card positions renumbered");
    let b = board(&mut conn, &s).await;
    let snap = read::board_snapshot(&mut conn, b.id)
        .await
        .unwrap()
        .unwrap();
    let todo: Vec<(i32, i32)> = snap.lanes[0]
        .cards
        .iter()
        .map(|c| (c.card.seq, c.card.position))
        .collect();
    assert_eq!(todo, [(b_, 1), (c, 2), (a, 3)]);
    assert_eq!(activity_verbs(&mut conn, b.id).await[0], "turn.summary");
}
