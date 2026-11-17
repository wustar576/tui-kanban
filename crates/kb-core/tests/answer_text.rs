//! Typed answers (docs/board-v2.md section 7.1) against `kanban_test`: the fourth parameter
//! of `kb_answer_question`, its rules, the stored text, the activity detail, the old
//! three-argument call, the reader's privileges and one notification per answer.

mod common;

use std::path::Path;
use std::time::Duration;

use kb_core::answer::{self, AnswerError};
use kb_core::events::{EventListener, ListenEvent};
use kb_core::model::QuestionStatus;
use kb_core::read;
use kb_core::resolve;
use kb_core::write::{self, BoardInit, CardAdd};
use kb_core::{Error, PgConnection};

async fn setup(conn: &mut PgConnection) -> (String, i64, i32) {
    let session = common::unique("text");
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    write::board_init(
        conn,
        &session,
        BoardInit {
            title: Some("文字回答".into()),
            goal: None,
            lanes: Some(write::parse_lanes("待辦:todo,實作:doing,完成:done").unwrap()),
            epics: None,
            cwd: None,
        },
        || ws,
    )
    .await
    .unwrap();
    let seq = write::card_add(conn, &session, CardAdd::new("實作", "修正 token"))
        .await
        .unwrap()
        .card
        .unwrap();
    let board = read::board_by_session(conn, &session)
        .await
        .unwrap()
        .unwrap();
    (session, board.id, seq)
}

async fn ask(conn: &mut PgConnection, session: &str, seq: i32, multi: bool) -> i64 {
    let options = ["15 分鐘", "30 分鐘", "60 分鐘"]
        .iter()
        .map(|o| write::parse_option(o).unwrap())
        .collect();
    write::question_ask(conn, session, seq, "token 的有效期？", options, multi)
        .await
        .unwrap()
        .question
        .unwrap()
}

async fn option_ids(conn: &mut PgConnection, board: i64, q: i64) -> Vec<i64> {
    read::question(conn, board, q)
        .await
        .unwrap()
        .unwrap()
        .options
        .iter()
        .map(|o| o.id)
        .collect()
}

fn refused(r: kb_core::Result<()>) -> AnswerError {
    match r {
        Err(Error::Answer(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn text_alone_or_with_options_is_stored_and_recorded() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board, seq) = setup(&mut writer).await;

    // Text only: the text is the answer.
    let q1 = ask(&mut writer, &session, seq, false).await;
    answer::answer_question(
        &mut reader,
        q1,
        &[],
        false,
        Some("  先不要，等這一輪結束再說 \n"),
    )
    .await
    .unwrap();
    let v = read::question(&mut reader, board, q1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.question.status, QuestionStatus::Answered);
    assert_eq!(
        v.question.answer_text.as_deref(),
        Some("先不要，等這一輪結束再說"),
        "trimmed"
    );
    assert!(v.selected_labels().is_empty());
    let a = &read::recent_activity(&mut reader, board, 1).await.unwrap()[0];
    assert_eq!(a.verb, "question.answer");
    assert_eq!(a.detail["text"], "先不要，等這一輪結束再說");
    assert_eq!(a.detail["labels"], serde_json::json!([]));

    // Options and text, multi-choice; the activity keeps the first 80 characters.
    let q2 = ask(&mut writer, &session, seq, true).await;
    let ids = option_ids(&mut reader, board, q2).await;
    let long = "字".repeat(120);
    answer::answer_question(&mut reader, q2, &[ids[0], ids[2]], false, Some(&long))
        .await
        .unwrap();
    let v = read::question(&mut reader, board, q2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.selected_labels(), ["15 分鐘", "60 分鐘"]);
    assert_eq!(v.question.answer_text.as_deref(), Some(long.as_str()));
    let a = &read::recent_activity(&mut reader, board, 1).await.unwrap()[0];
    assert_eq!(a.detail["text"].as_str().unwrap().chars().count(), 80);
    assert_eq!(
        a.detail["labels"],
        serde_json::json!(["15 分鐘", "60 分鐘"])
    );

    // Text with 「在 session 裡說明」: ignored.
    let q3 = ask(&mut writer, &session, seq, false).await;
    answer::answer_question(&mut reader, q3, &[], true, Some("不會被存下"))
        .await
        .unwrap();
    let v = read::question(&mut reader, board, q3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.question.status, QuestionStatus::Chat);
    assert_eq!(v.question.answer_text, None);
    let a = &read::recent_activity(&mut reader, board, 1).await.unwrap()[0];
    assert!(a.detail.get("text").is_none());
}

#[tokio::test]
async fn empty_or_too_long_text_is_refused() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board, seq) = setup(&mut writer).await;
    let q = ask(&mut writer, &session, seq, false).await;
    let ids = option_ids(&mut reader, board, q).await;

    // No option and no real text: as before.
    for text in [None, Some(""), Some("   "), Some("\n\t　")] {
        let e = refused(answer::answer_question(&mut reader, q, &[], false, text).await);
        assert_eq!(e, AnswerError::NoOptionSelected, "{text:?}");
    }
    // 501 characters: refused, with or without options; 500 is fine.
    let e = refused(
        answer::answer_question(&mut reader, q, &ids[..1], false, Some(&"字".repeat(501))).await,
    );
    assert_eq!(e, AnswerError::TextTooLong);
    assert_eq!(e.as_str(), "answer text is too long");
    assert_eq!(e.zh_tw(), "文字超過 500 個字");
    // Still a single-choice question.
    let e = refused(answer::answer_question(&mut reader, q, &ids[..2], false, Some("x")).await);
    assert_eq!(e, AnswerError::AcceptsOneOption);
    // Nothing was written by the refusals.
    let v = read::question(&mut reader, board, q)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.question.status, QuestionStatus::Open);
    answer::answer_question(&mut reader, q, &[], false, Some(&"字".repeat(500)))
        .await
        .unwrap();
    let v = read::question(&mut reader, board, q)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.question.answer_text.unwrap().chars().count(), 500);
}

#[tokio::test]
async fn the_three_argument_call_still_works_and_the_reader_stays_read_only() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board, seq) = setup(&mut writer).await;
    let q = ask(&mut writer, &session, seq, false).await;
    let ids = option_ids(&mut reader, board, q).await;

    // What binaries built before version 4 send.
    sqlx::query("SELECT kb_answer_question($1, $2, $3)")
        .bind(q)
        .bind(&ids[1..2])
        .bind(false)
        .execute(&mut reader)
        .await
        .unwrap();
    let v = read::question(&mut reader, board, q)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v.selected_labels(), ["30 分鐘"]);
    assert_eq!(v.question.answer_text, None);

    // One function only (no ambiguous overload), SECURITY DEFINER, pinned search_path,
    // not executable by PUBLIC.
    let (n, definer, config): (i64, bool, Option<Vec<String>>) = sqlx::query_as(
        "SELECT count(*) OVER (), p.prosecdef, p.proconfig FROM pg_proc p
         WHERE p.proname = 'kb_answer_question'",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert_eq!(n, 1);
    assert!(definer);
    assert!(
        config
            .unwrap()
            .iter()
            .any(|c| c.starts_with("search_path="))
    );
    let public: bool = sqlx::query_scalar(
        "SELECT has_function_privilege('public',
             'kb_answer_question(bigint, bigint[], boolean, text)', 'EXECUTE')",
    )
    .fetch_one(&mut writer)
    .await
    .unwrap();
    assert!(!public, "PUBLIC must not execute kb_answer_question");

    // The reader can answer (above) but still cannot write the column directly.
    let err = sqlx::query("UPDATE question SET answer_text = 'x' WHERE id = $1")
        .bind(q)
        .execute(&mut reader)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"), "{err}");
}

#[tokio::test]
async fn one_notification_per_answer_with_text() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board, seq) = setup(&mut writer).await;
    let q = ask(&mut writer, &session, seq, false).await;

    let url = common::config()
        .url(kb_core::Role::Reader)
        .unwrap()
        .to_string();
    let mut listener = EventListener::connect(&url).await.unwrap();
    answer::answer_question(&mut reader, q, &[], false, Some("只有文字"))
        .await
        .unwrap();
    let mut seen = 0;
    while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(800), listener.next()).await {
        if let ListenEvent::Board(b) = ev
            && b.board == board
        {
            seen += 1;
        }
    }
    assert_eq!(seen, 1);
}
