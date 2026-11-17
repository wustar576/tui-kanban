//! Answering a question the way the TUI does, against database `kanban_test` (never
//! `kanban`): the screen state produces the answer, `kb_tui::send_answer` sends it on a
//! `kb_reader` connection, and the reloaded card shows the result.
//!
//! The board, card and question are set up with `kb_core::write` as the writer role; that
//! is test setup only. The TUI library itself never uses `kb_core::write`.

use std::path::Path;
use std::time::Instant;

use chrono::Local;
use kb_core::model::QuestionStatus;
use kb_core::resolve;
use kb_core::write::{self, BoardInit, CardAdd};
use kb_core::{Config, Error, PgConnection, Role, db};
use kb_tui::app::{App, BoardData, BoardTarget, Effect, Event, Key, Scope};
use kb_tui::run::answer_error_text;

const TEST_DATABASE: &str = "kanban_test";

fn config() -> Config {
    let base = Config::load().expect("load config");
    let swap = |role: Role| {
        let mut url = url::Url::parse(base.url(role).expect("database url")).unwrap();
        url.set_path(TEST_DATABASE);
        Some(url.to_string())
    };
    let config = Config {
        writer_url: swap(Role::Writer),
        reader_url: swap(Role::Reader),
        admin_url: swap(Role::Admin),
    };
    for role in [Role::Writer, Role::Reader, Role::Admin] {
        assert!(config.url(role).unwrap().ends_with("/kanban_test"));
    }
    config
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("test-{prefix}-{}-{nanos}", std::process::id())
}

async fn load(reader: &mut PgConnection, board_id: i64) -> BoardData {
    BoardData {
        snapshot: kb_core::read::board_snapshot(reader, board_id)
            .await
            .unwrap()
            .unwrap(),
        activity: kb_core::read::recent_feed(reader, board_id, 60)
            .await
            .unwrap(),
        findings: kb_core::read::active_judge_findings(reader, board_id)
            .await
            .unwrap(),
    }
}

#[tokio::test]
async fn the_tui_answers_a_question_on_the_reader_connection() {
    let config = config();
    db::migrate(&config, Some(TEST_DATABASE)).await.unwrap();
    let mut writer = db::connect_role(&config, Role::Writer).await.unwrap();
    let mut reader = db::connect_role(&config, Role::Reader).await.unwrap();

    // Setup (writer): a board, a card, a single-choice question.
    let session = unique("tui-answer");
    let ws = resolve::resolve_workspace(Some(&unique("ws")), None, Path::new("."));
    write::board_init(
        &mut writer,
        &session,
        BoardInit {
            title: Some("提問看板".into()),
            goal: None,
            lanes: Some(write::parse_lanes("待辦:todo,實作:doing,完成:done").unwrap()),
            epics: None,
            cwd: None,
        },
        || ws,
    )
    .await
    .unwrap();
    let seq = write::card_add(&mut writer, &session, CardAdd::new("實作", "修正 token"))
        .await
        .unwrap()
        .card
        .unwrap();
    let options = ["15 分鐘::與 refresh 一致", "30 分鐘"]
        .iter()
        .map(|o| write::parse_option(o).unwrap())
        .collect();
    let qid = write::question_ask(
        &mut writer,
        &session,
        seq,
        "token 的有效期？",
        options,
        false,
    )
    .await
    .unwrap()
    .question
    .unwrap();
    let board_id = kb_core::read::board_by_session(&mut reader, &session)
        .await
        .unwrap()
        .unwrap()
        .id;

    // The screen: board loaded from the reader, `a` opens the card, Down + Space marks
    // 「30 分鐘」, Enter produces the answer.
    let mut app = App::new(
        (100, 30),
        Local::now().date_naive(),
        Scope::All,
        Some(board_id),
    );
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(board_id),
        data: Some(load(&mut reader, board_id).await),
        at: Instant::now(),
    });
    assert_eq!(app.data.as_ref().unwrap().snapshot.open_question_count, 1);
    let fx = app.update(Event::Key(Key::Char('a')));
    assert_eq!(fx, vec![Effect::LoadCard { board_id, seq }]);
    let detail = kb_core::read::card_detail(&mut reader, board_id, seq)
        .await
        .unwrap();
    app.update(Event::CardLoaded {
        board_id,
        seq,
        detail,
    });
    app.update(Event::Key(Key::Down));
    app.update(Event::Key(Key::Char(' ')));
    let fx = app.update(Event::Key(Key::Enter));
    let [Effect::Answer(request)] = &fx[..] else {
        panic!("expected one answer, got {fx:?}");
    };
    assert_eq!((request.question_id, request.chat), (qid, false));
    assert_eq!(request.option_ids.len(), 1);

    // Sent through the TUI's function on the reader connection.
    kb_tui::send_answer(&mut reader, request).await.unwrap();
    let q = kb_core::read::question(&mut reader, board_id, qid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.question.status, QuestionStatus::Answered);
    assert_eq!(q.selected_labels(), vec!["30 分鐘"]);
    let feed = kb_core::read::recent_feed(&mut reader, board_id, 5)
        .await
        .unwrap();
    assert_eq!(feed[0].verb, "question.answer");
    assert_eq!(feed[0].actor, "user");

    // The result reaches the screen: marks cleared, reload requested; after the reload the
    // question is one answered line and the header count is gone.
    let fx = app.update(Event::Answered {
        question_id: qid,
        error: None,
    });
    assert!(fx.contains(&Effect::LoadBoard(BoardTarget::Id(board_id))));
    assert!(app.marks.is_empty());
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(board_id),
        data: Some(load(&mut reader, board_id).await),
        at: Instant::now(),
    });
    let detail = kb_core::read::card_detail(&mut reader, board_id, seq)
        .await
        .unwrap();
    app.update(Event::CardLoaded {
        board_id,
        seq,
        detail,
    });
    let text = kb_tui::render_text(&app, 100, 30);
    assert!(
        text.contains(&format!("提問 {qid}：token 的有效期？  已回答：30 分鐘")),
        "{text}"
    );
    assert!(!text.contains("待回答"), "{text}");
    assert_eq!(app.data.as_ref().unwrap().snapshot.open_question_count, 0);

    // Answering again is refused with a readable reason.
    let err = kb_tui::send_answer(&mut reader, request).await.unwrap_err();
    assert!(matches!(err, Error::Answer(_)), "{err:?}");
    assert_eq!(
        answer_error_text(&err),
        "提問已經不是待回答（可能已被回答或取消）"
    );
    // No `hc send` any more (docs/board-v2.md section 13.19): nothing runs after a plain
    // answer, and nothing is shown about it.
    assert_eq!(kb_tui::notify::after_answer(None, false).await, None);
    assert_eq!(kb_tui::notify::command(Some("t9"), false), None);
}

#[tokio::test]
async fn the_tui_sends_typed_text_on_the_reader_connection() {
    let config = config();
    db::migrate(&config, Some(TEST_DATABASE)).await.unwrap();
    let mut writer = db::connect_role(&config, Role::Writer).await.unwrap();
    let mut reader = db::connect_role(&config, Role::Reader).await.unwrap();
    let session = unique("tui-text");
    let ws = resolve::resolve_workspace(Some(&unique("ws")), None, Path::new("."));
    write::board_init(
        &mut writer,
        &session,
        BoardInit {
            title: Some("文字".into()),
            goal: None,
            lanes: Some(write::parse_lanes("待辦:todo,實作:doing,完成:done").unwrap()),
            epics: None,
            cwd: None,
        },
        || ws,
    )
    .await
    .unwrap();
    let seq = write::card_add(&mut writer, &session, CardAdd::new("實作", "排程器"))
        .await
        .unwrap()
        .card
        .unwrap();
    let options = ["要", "不要"]
        .iter()
        .map(|o| write::parse_option(o).unwrap())
        .collect();
    let qid = write::question_ask(
        &mut writer,
        &session,
        seq,
        "要不要一併重構？",
        options,
        false,
    )
    .await
    .unwrap()
    .question
    .unwrap();
    let board_id = kb_core::read::board_by_session(&mut reader, &session)
        .await
        .unwrap()
        .unwrap()
        .id;

    let mut app = App::new(
        (100, 30),
        Local::now().date_naive(),
        Scope::All,
        Some(board_id),
    );
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(board_id),
        data: Some(load(&mut reader, board_id).await),
        at: Instant::now(),
    });
    app.update(Event::Key(Key::Char('a')));
    let detail = kb_core::read::card_detail(&mut reader, board_id, seq)
        .await
        .unwrap();
    app.update(Event::CardLoaded {
        board_id,
        seq,
        detail,
    });
    // Down to the input row (要, 不要, 在 session 裡說明, input) and type, `q` included.
    for k in [Key::Down, Key::Down, Key::Down] {
        app.update(Event::Key(k));
    }
    for c in "先不要，quick 一點".chars() {
        app.update(Event::Key(Key::Char(c)));
    }
    assert!(!app.quit);
    let fx = app.update(Event::Key(Key::Enter));
    let [Effect::Answer(request)] = &fx[..] else {
        panic!("expected one answer, got {fx:?}");
    };
    assert_eq!(request.text.as_deref(), Some("先不要，quick 一點"));
    assert!(request.option_ids.is_empty());
    kb_tui::send_answer(&mut reader, request).await.unwrap();
    let q = kb_core::read::question(&mut reader, board_id, qid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(q.question.status, QuestionStatus::Answered);
    assert_eq!(
        q.question.answer_text.as_deref(),
        Some("先不要，quick 一點")
    );
    // After the reload, the answered line carries the text.
    app.update(Event::Answered {
        question_id: qid,
        error: None,
    });
    let detail = kb_core::read::card_detail(&mut reader, board_id, seq)
        .await
        .unwrap();
    app.update(Event::CardLoaded {
        board_id,
        seq,
        detail,
    });
    let text = kb_tui::render_text(&app, 100, 30);
    assert!(
        text.contains(&format!(
            "提問 {qid}：要不要一併重構？  已回答：先不要，quick 一點"
        )),
        "{text}"
    );
}
