//! Comments from the human and the background waiter (docs/board-v2.md section 13) against
//! `kanban_test`: `kb_add_comment` and its rules, unreplied counts, delivery through
//! `kb inbox` and the hooks, task notifications, and `kb hook wait`'s waiter.

mod common;

use std::path::Path;
use std::time::Duration;

use kb_core::comment::{self, CommentError};
use kb_core::hook;
use kb_core::model::NoteKind;
use kb_core::read;
use kb_core::resolve;
use kb_core::wait::{self, Announcement, WaitEnd, WaitOptions};
use kb_core::write::{self, BoardInit, CardAdd};
use kb_core::{Error, PgConnection, Role};

/// A board with lanes and two cards; returns the session key, the board id and the ids of
/// cards 1 and 2.
async fn setup(conn: &mut PgConnection) -> (String, i64, i64, i64) {
    let session = common::unique("comment");
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    write::board_init(
        conn,
        &session,
        BoardInit {
            title: Some("留言".into()),
            goal: None,
            lanes: Some(write::parse_lanes("待辦:todo,實作:doing,完成:done").unwrap()),
            epics: None,
            cwd: None,
        },
        || ws,
    )
    .await
    .unwrap();
    write::card_add(conn, &session, CardAdd::new("實作", "登入逾時\n第二行"))
        .await
        .unwrap();
    write::card_add(conn, &session, CardAdd::new("待辦", "另一張"))
        .await
        .unwrap();
    let board = read::board_by_session(conn, &session)
        .await
        .unwrap()
        .unwrap();
    let snap = read::board_snapshot(conn, board.id).await.unwrap().unwrap();
    let card_id = |seq: i32| snap.card(seq).unwrap().1.card.id;
    (session.clone(), board.id, card_id(1), card_id(2))
}

fn refused(r: kb_core::Result<i64>) -> CommentError {
    match r {
        Err(Error::Comment(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn writer_url() -> String {
    common::config().url(Role::Writer).unwrap().to_string()
}

/// Short timings so the tests stay fast.
fn opts(session: &str, claude_pid: Option<u32>) -> WaitOptions {
    let mut o = WaitOptions::new(session, claude_pid);
    o.alive_every = Duration::from_millis(200);
    o.poll_every = Duration::from_secs(5);
    o.debounce = Duration::from_millis(300);
    o.takeover_limit = Duration::from_secs(5);
    o.max_wait = Duration::from_secs(20);
    o
}

/// The session's `kb_waiter` rows (0 or 1) and the token of the row, if any.
async fn waiter_row(conn: &mut PgConnection, session: &str) -> Option<String> {
    sqlx::query_scalar("SELECT token FROM kb_waiter WHERE session_key = $1")
        .bind(session)
        .fetch_optional(&mut *conn)
        .await
        .unwrap()
}

/// A row as a waiter leaves it when it is killed (docs/board-v2.md section 20.4).
async fn stale_row(conn: &mut PgConnection, session: &str, claude_pid: Option<i64>) {
    sqlx::query(
        "INSERT INTO kb_waiter (session_key, token, waiter_pid, claude_pid, started_at)
         VALUES ($1, 'stale', 999999, $2, now() - interval '2 days')",
    )
    .bind(session)
    .bind(claude_pid)
    .execute(&mut *conn)
    .await
    .unwrap();
}

#[tokio::test]
async fn reader_adds_comments_by_the_rules() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board_id, card1, _) = setup(&mut writer).await;
    let rev = read::board_by_id(&mut writer, board_id)
        .await
        .unwrap()
        .unwrap()
        .rev;

    // Line breaks become spaces; white space (also the ideographic one) is trimmed.
    let id = comment::add_comment(&mut reader, card1, "\u{3000} 這張先不要做\r\n先查 log ")
        .await
        .unwrap();
    let detail = read::card_detail(&mut writer, board_id, 1)
        .await
        .unwrap()
        .unwrap();
    let note = detail.notes.iter().find(|n| n.id == id).unwrap();
    assert_eq!(note.kind, NoteKind::Comment);
    assert_eq!(note.author, "user");
    assert_eq!(note.body, "這張先不要做 先查 log");
    assert_eq!(note.delivered_at, None);
    assert_eq!(detail.unreplied_comments, vec![id]);

    let a = &read::recent_activity(&mut writer, board_id, 1)
        .await
        .unwrap()[0];
    assert_eq!((a.verb.as_str(), a.actor.as_str()), ("comment.add", "user"));
    assert_eq!(a.detail["note"], id);
    assert_eq!(a.detail["seq"], 1);
    assert_eq!(a.detail["excerpt"], "這張先不要做 先查 log");
    let board = read::board_by_id(&mut writer, board_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(board.rev, rev + 1);

    // 500 characters are fine, 501 are not; empty text and unknown cards are refused.
    comment::add_comment(&mut reader, card1, &"字".repeat(500))
        .await
        .unwrap();
    assert_eq!(
        refused(comment::add_comment(&mut reader, card1, &"字".repeat(501)).await),
        CommentError::TooLong
    );
    assert_eq!(
        refused(comment::add_comment(&mut reader, card1, " \n\u{3000}").await),
        CommentError::Empty
    );
    assert_eq!(
        refused(comment::add_comment(&mut reader, -1, "x").await),
        CommentError::CardNotFound
    );

    // The reader still cannot write the table directly.
    let direct =
        sqlx::query("INSERT INTO card_note (card_id, kind, body) VALUES ($1, 'comment', 'x')")
            .bind(card1)
            .execute(&mut reader)
            .await;
    assert!(direct.is_err());

    // The agent cannot write a comment.
    let err = write::card_note(&mut writer, &session, 1, NoteKind::Comment, "x")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "comment notes come from the user; reply with --kind reply"
    );

    // Archived boards refuse comments.
    write::board_archive(&mut writer, &session, None, None)
        .await
        .unwrap();
    assert_eq!(
        refused(comment::add_comment(&mut reader, card1, "還有一件事").await),
        CommentError::BoardArchived
    );
}

#[tokio::test]
async fn replies_clear_the_unreplied_counts() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board_id, card1, card2) = setup(&mut writer).await;
    let c1 = comment::add_comment(&mut reader, card1, "第一則")
        .await
        .unwrap();
    comment::add_comment(&mut reader, card2, "第二則")
        .await
        .unwrap();

    let snap = read::board_snapshot(&mut reader, board_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.unreplied_comment_count, 2);
    assert_eq!(snap.card(1).unwrap().1.unreplied_comment_count, 1);
    assert_eq!(snap.card(2).unwrap().1.unreplied_comment_count, 1);

    // A reply answers every older comment on the same card, not the other card's.
    write::card_note(&mut writer, &session, 1, NoteKind::Reply, "好，先查 log")
        .await
        .unwrap();
    let snap = read::board_snapshot(&mut reader, board_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.card(1).unwrap().1.unreplied_comment_count, 0);
    assert_eq!(snap.unreplied_comment_count, 1);
    let unreplied = read::unreplied_comments(&mut reader, board_id)
        .await
        .unwrap();
    assert_eq!(
        unreplied.iter().map(|c| c.card_seq).collect::<Vec<_>>(),
        [2]
    );

    // A newer comment needs a newer reply.
    let c3 = comment::add_comment(&mut reader, card1, "再補一句")
        .await
        .unwrap();
    let detail = read::card_detail(&mut reader, board_id, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.unreplied_comments, vec![c3]);
    assert!(detail.notes.iter().any(|n| n.id == c1));
}

#[tokio::test]
async fn inbox_and_hooks_deliver_comments() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board_id, card1, _) = setup(&mut writer).await;
    let c1 = comment::add_comment(&mut reader, card1, "先查 log")
        .await
        .unwrap();

    // --peek reads only.
    let peek = write::inbox(&mut writer, &session, true).await.unwrap();
    assert_eq!(peek.comments.len(), 1);
    assert_eq!(peek.comments[0].note.id, c1);
    assert_eq!(peek.comments[0].card_seq, 1);
    assert_eq!(peek.comments[0].card_title, "登入逾時\n第二行");
    assert!(!peek.delivered);
    assert_eq!(peek.unreplied_comments.len(), 1);

    let inbox = write::inbox(&mut writer, &session, false).await.unwrap();
    assert!(inbox.delivered);
    assert_eq!(inbox.comments.len(), 1);
    let a = &read::recent_activity(&mut writer, board_id, 1)
        .await
        .unwrap()[0];
    assert_eq!(
        (a.verb.as_str(), a.actor.as_str()),
        ("comment.deliver", "agent")
    );
    assert_eq!(a.detail["note"], c1);
    let again = write::inbox(&mut writer, &session, false).await.unwrap();
    assert!(again.comments.is_empty());
    // Delivered, but still without a reply.
    assert_eq!(again.unreplied_comments.len(), 1);

    // The stop hook blocks with an undelivered comment, like with an answer.
    let c2 = comment::add_comment(&mut reader, card1, "再一則")
        .await
        .unwrap();
    let levels = kb_core::rules::Levels::default();
    match hook::stop(&mut writer, &session, &levels).await.unwrap() {
        hook::Stop::Deliver(d) => {
            assert!(d.answers.is_empty());
            assert_eq!(d.comments[0].note.id, c2);
            assert_eq!(d.unreplied_comments.len(), 2);
        }
        other => panic!("expected a delivery, got {other:?}"),
    }
    let a = &read::recent_activity(&mut writer, board_id, 1)
        .await
        .unwrap()[0];
    assert_eq!(
        (a.verb.as_str(), a.actor.as_str()),
        ("comment.deliver", "hook")
    );
}

#[tokio::test]
async fn task_notifications_deliver_without_a_new_turn() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board_id, card1, _) = setup(&mut writer).await;
    hook::prompt(&mut writer, &session, Some("turn-1"), "開始")
        .await
        .unwrap()
        .unwrap();
    assert!(hook::is_task_notification(
        "\n<task-notification>\n<summary>Stop hook feedback</summary>"
    ));
    assert!(!hook::is_task_notification("請看 <task-notification>"));

    // Nothing to deliver: nothing written.
    let rev = read::board_by_id(&mut writer, board_id)
        .await
        .unwrap()
        .unwrap()
        .rev;
    let d = hook::notification(&mut writer, &session)
        .await
        .unwrap()
        .unwrap();
    assert!(d.is_empty());
    assert_eq!(
        read::board_by_id(&mut writer, board_id)
            .await
            .unwrap()
            .unwrap()
            .rev,
        rev
    );

    let c = comment::add_comment(&mut reader, card1, "留言")
        .await
        .unwrap();
    let d = hook::notification(&mut writer, &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.comments[0].note.id, c);
    let board = read::board_by_id(&mut writer, board_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(board.turn_key.as_deref(), Some("turn-1"));
    let verbs: Vec<String> = read::recent_activity(&mut writer, board_id, 3)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.verb)
        .collect();
    assert_eq!(verbs, ["comment.deliver", "comment.add", "user.prompt"]);
}

// --- the waiter ------------------------------------------------------------------------------

#[tokio::test]
async fn waiter_without_an_active_board_ends_at_once() {
    let mut writer = common::writer().await;
    let url = writer_url();
    let end = wait::wait(&url, &opts(&common::unique("none"), None))
        .await
        .unwrap();
    assert_eq!(end, WaitEnd::NoBoard);
    let (session, _, _, _) = setup(&mut writer).await;
    write::board_archive(&mut writer, &session, None, None)
        .await
        .unwrap();
    assert_eq!(
        wait::wait(&url, &opts(&session, None)).await.unwrap(),
        WaitEnd::NoBoard
    );
}

#[tokio::test]
async fn waiter_wakes_once_per_item_and_never_delivers() {
    let mut writer = common::writer().await;
    let mut reader = common::reader().await;
    let (session, board_id, card1, card2) = setup(&mut writer).await;
    let url = writer_url();
    let me = Some(std::process::id());

    // A question answered with "reply in the session" does not wake.
    let options = vec![
        write::parse_option("要").unwrap(),
        write::parse_option("不要").unwrap(),
    ];
    let q_chat = write::question_ask(&mut writer, &session, 1, "聊聊？", options.clone(), false)
        .await
        .unwrap()
        .question
        .unwrap();
    kb_core::answer::answer_question(&mut reader, q_chat, &[], true, None)
        .await
        .unwrap();
    let q = write::question_ask(&mut writer, &session, 2, "保留嗎？", options, false)
        .await
        .unwrap()
        .question
        .unwrap();

    let waiter = tokio::spawn({
        let (url, o) = (url.clone(), opts(&session, me));
        async move { wait::wait(&url, &o).await }
    });
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!waiter.is_finished(), "the chat answer must not wake");

    // An answer and a comment made together: one wake-up for both.
    let opt = read::question(&mut reader, board_id, q)
        .await
        .unwrap()
        .unwrap()
        .options[0]
        .id;
    kb_core::answer::answer_question(&mut reader, q, &[opt], false, None)
        .await
        .unwrap();
    let c = comment::add_comment(&mut reader, card2, "這張先不要做")
        .await
        .unwrap();
    let end = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("woke in time")
        .unwrap()
        .unwrap();
    let expected = Announcement {
        answers: vec![(q, 2)],
        comments: vec![(c, 2)],
    };
    assert_eq!(end, WaitEnd::Wake(expected.clone()));
    assert_eq!(
        waiter_row(&mut writer, &session).await,
        None,
        "row left after a wake"
    );
    assert_eq!(
        expected.message(),
        format!(
            "[kb] question {q} answered; 1 new comment on card 2. The content is delivered in \
             this turn's context; run \"kb inbox\" only if you do not see it."
        )
    );

    // Nothing was delivered: kb inbox still has both.
    let peek = write::inbox(&mut writer, &session, true).await.unwrap();
    assert_eq!(peek.comments.len(), 1);
    assert!(peek.answers.iter().any(|a| a.question.id == q));

    // A new waiter does not wake again for the same items, but does for a new one.
    let waiter = tokio::spawn({
        let (url, o) = (url.clone(), opts(&session, me));
        async move { wait::wait(&url, &o).await }
    });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        !waiter.is_finished(),
        "already announced items must not wake"
    );
    let c2 = comment::add_comment(&mut reader, card1, "還有這張")
        .await
        .unwrap();
    let end = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("woke in time")
        .unwrap()
        .unwrap();
    assert_eq!(
        end,
        WaitEnd::Wake(Announcement {
            answers: vec![],
            comments: vec![(c2, 1)],
        })
    );
    assert_eq!(waiter_row(&mut writer, &session).await, None);
}

#[tokio::test]
async fn one_waiter_per_session() {
    let mut writer = common::writer().await;
    let (session, _, _, _) = setup(&mut writer).await;
    let url = writer_url();
    let me = Some(std::process::id());

    let first = tokio::spawn({
        let (url, o) = (url.clone(), opts(&session, me));
        async move { wait::wait(&url, &o).await }
    });
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Same Claude Code process: the second one leaves at once.
    let second = wait::wait(&url, &opts(&session, me)).await.unwrap();
    assert_eq!(second, WaitEnd::Duplicate);
    assert!(!first.is_finished());

    // Another Claude Code process takes the session over; the first steps down.
    let other = tokio::spawn({
        let (url, o) = (url.clone(), opts(&session, None));
        async move { wait::wait(&url, &o).await }
    });
    let end = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .expect("first waiter stepped down")
        .unwrap()
        .unwrap();
    assert_eq!(end, WaitEnd::TakenOver);
    // The superseded waiter leaves the new waiter's row alone.
    assert!(waiter_row(&mut writer, &session).await.is_some());

    // The new waiter holds the session: archiving the board ends it.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!other.is_finished());
    write::board_archive(&mut writer, &session, None, None)
        .await
        .unwrap();
    let end = tokio::time::timeout(Duration::from_secs(10), other)
        .await
        .expect("waiter saw the archive")
        .unwrap()
        .unwrap();
    assert_eq!(end, WaitEnd::BoardClosed);
    assert_eq!(waiter_row(&mut writer, &session).await, None);
}

#[tokio::test]
async fn waiter_ends_when_claude_is_gone_or_time_is_up() {
    let mut writer = common::writer().await;
    let (session, _, _, _) = setup(&mut writer).await;
    let url = writer_url();

    let mut child = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
    } else {
        std::process::Command::new("true").spawn()
    }
    .unwrap();
    let gone = child.id();
    child.wait().unwrap();
    drop(child);
    let end = tokio::time::timeout(
        Duration::from_secs(10),
        wait::wait(&url, &opts(&session, Some(gone))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(end, WaitEnd::ClaudeGone);
    assert_eq!(waiter_row(&mut writer, &session).await, None);

    let mut o = opts(&session, Some(std::process::id()));
    o.max_wait = Duration::from_millis(600);
    let end = tokio::time::timeout(Duration::from_secs(10), wait::wait(&url, &o))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, WaitEnd::Deadline);
    assert_eq!(waiter_row(&mut writer, &session).await, None);
}

/// docs/board-v2.md section 20.4: a row left by a killed waiter never blocks a new one,
/// whatever its `claude_pid`; the new waiter overwrites it.
#[tokio::test]
async fn a_stale_waiter_row_does_not_block() {
    let mut writer = common::writer().await;
    let (session, _, _, _) = setup(&mut writer).await;
    let url = writer_url();
    let me = Some(std::process::id());

    // Another Claude Code process: no take-over wait, the waiter just starts.
    stale_row(&mut writer, &session, Some(1)).await;
    let mut o = opts(&session, me);
    o.max_wait = Duration::from_millis(1500);
    let end = tokio::time::timeout(Duration::from_secs(10), wait::wait(&url, &o))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, WaitEnd::Deadline);
    assert_eq!(waiter_row(&mut writer, &session).await, None);

    // The same Claude Code process: not taken for a live duplicate.
    stale_row(&mut writer, &session, me.map(i64::from)).await;
    let waiter = tokio::spawn({
        let (url, o) = (url.clone(), opts(&session, me));
        async move { wait::wait(&url, &o).await }
    });
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!waiter.is_finished(), "the stale row stopped the waiter");
    let token = waiter_row(&mut writer, &session).await.expect("waiter row");
    assert_ne!(token, "stale");

    // kb hook session-end deletes the row and archives the board; the waiter ends.
    let outcome = hook::session_end(&mut writer, &session, "other")
        .await
        .unwrap();
    assert!(outcome.is_some());
    let end = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("waiter ended after session-end")
        .unwrap()
        .unwrap();
    assert!(
        matches!(end, WaitEnd::TakenOver | WaitEnd::BoardClosed),
        "{end:?}"
    );
    assert_eq!(waiter_row(&mut writer, &session).await, None);
}

/// docs/board-v2.md section 20.4: session-end deletes the row also without an active board,
/// and a waiter whose take-over fails deletes the row it wrote.
#[tokio::test]
async fn waiter_rows_are_deleted_at_session_end_and_after_a_failed_take_over() {
    let mut writer = common::writer().await;

    // No board at all.
    let lonely = common::unique("lonely");
    stale_row(&mut writer, &lonely, None).await;
    assert_eq!(
        hook::session_end(&mut writer, &lonely, "other")
            .await
            .unwrap(),
        None
    );
    assert_eq!(waiter_row(&mut writer, &lonely).await, None);

    // An already archived board.
    let (session, _, _, _) = setup(&mut writer).await;
    write::board_archive(&mut writer, &session, None, None)
        .await
        .unwrap();
    stale_row(&mut writer, &session, None).await;
    assert_eq!(
        hook::session_end(&mut writer, &session, "other")
            .await
            .unwrap(),
        None
    );
    assert_eq!(waiter_row(&mut writer, &session).await, None);

    // A holder that never steps down: the take-over fails, and the row it wrote is gone.
    let (session, _, _, _) = setup(&mut writer).await;
    let mut holder = common::writer().await;
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('kb_waiter:' || $1, 0))")
        .bind(&session)
        .execute(&mut holder)
        .await
        .unwrap();
    stale_row(&mut writer, &session, Some(1)).await;
    let mut o = opts(&session, Some(std::process::id()));
    o.takeover_limit = Duration::from_millis(1500);
    let end = tokio::time::timeout(Duration::from_secs(10), wait::wait(&writer_url(), &o))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, WaitEnd::TakeoverFailed);
    assert_eq!(waiter_row(&mut writer, &session).await, None);
}
