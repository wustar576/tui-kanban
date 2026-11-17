//! Adding a comment the way the TUI does, against database `kanban_test` (never
//! `kanban`): `i` on the board opens the comment row, Enter produces the request,
//! `kb_tui::send_comment` sends it on a `kb_reader` connection, and the reloaded board and
//! card show the comment (docs/board-v2.md sections 13.15 and 13.16).
//!
//! The board and card are set up with `kb_core::write` as the writer role; that is test
//! setup only. The TUI library itself never uses `kb_core::write`.

use std::path::Path;
use std::time::Instant;

use chrono::Local;
use kb_core::comment::CommentError;
use kb_core::model::NoteKind;
use kb_core::resolve;
use kb_core::write::{self, BoardInit, CardAdd};
use kb_core::{Config, Error, PgConnection, Role, db};
use kb_tui::app::{App, BoardData, BoardTarget, Effect, Event, Key, MSG_COMMENT_SENT, Scope};
use kb_tui::run::comment_error_text;

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
async fn the_tui_adds_a_comment_on_the_reader_connection() {
    let config = config();
    db::migrate(&config, Some(TEST_DATABASE)).await.unwrap();
    let mut writer = db::connect_role(&config, Role::Writer).await.unwrap();
    let mut reader = db::connect_role(&config, Role::Reader).await.unwrap();

    // Setup (writer): a board with one card.
    let session = unique("tui-comment");
    let ws = resolve::resolve_workspace(Some(&unique("ws")), None, Path::new("."));
    write::board_init(
        &mut writer,
        &session,
        BoardInit {
            title: Some("留言看板".into()),
            goal: None,
            lanes: Some(write::parse_lanes("待辦:todo,實作:doing,完成:done").unwrap()),
            epics: None,
            cwd: None,
        },
        || ws,
    )
    .await
    .unwrap();
    let seq = write::card_add(&mut writer, &session, CardAdd::new("待辦", "登入逾時"))
        .await
        .unwrap()
        .card
        .unwrap();
    let board_id = kb_core::read::board_by_session(&mut reader, &session)
        .await
        .unwrap()
        .unwrap()
        .id;

    // The screen: `i` on the selected card, typing (command letters included), Enter.
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
    app.update(Event::Key(Key::Char('i')));
    for c in "這張先不要做，quick 查 log".chars() {
        app.update(Event::Key(Key::Char(c)));
    }
    assert!(!app.quit);
    let fx = app.update(Event::Key(Key::Enter));
    let [Effect::Comment(request)] = &fx[..] else {
        panic!("expected one comment, got {fx:?}");
    };
    assert_eq!(request.seq, seq);

    // Sent through the TUI's function on the reader connection.
    let note_id = kb_tui::send_comment(&mut reader, request).await.unwrap();
    let detail = kb_core::read::card_detail(&mut reader, board_id, seq)
        .await
        .unwrap()
        .unwrap();
    let note = detail.notes.iter().find(|n| n.id == note_id).unwrap();
    assert_eq!(note.kind, NoteKind::Comment);
    assert_eq!(note.author, "user");
    assert_eq!(note.body, "這張先不要做，quick 查 log");
    assert_eq!(detail.unreplied_comments, vec![note_id]);
    let feed = kb_core::read::recent_feed(&mut reader, board_id, 5)
        .await
        .unwrap();
    assert_eq!(feed[0].verb, "comment.add");
    assert_eq!(feed[0].actor, "user");

    // The result reaches the screen: the row closes, the reload shows the header count,
    // the tag and the feed line; the card shows the comment as 未回覆.
    let fx = app.update(Event::Commented {
        card_id: request.card_id,
        error: None,
    });
    assert!(fx.contains(&Effect::LoadBoard(BoardTarget::Id(board_id))));
    assert!(app.comment.is_none());
    assert_eq!(app.flash.as_deref(), Some(MSG_COMMENT_SENT));
    app.update(Event::BoardLoaded {
        target: BoardTarget::Id(board_id),
        data: Some(load(&mut reader, board_id).await),
        at: Instant::now(),
    });
    assert_eq!(
        app.data.as_ref().unwrap().snapshot.unreplied_comment_count,
        1
    );
    let text = kb_tui::render_text(&app, 100, 30);
    assert!(text.lines().next().unwrap().contains("留言 1"), "{text}");
    assert!(text.contains(" 留言 "), "the tag: {text}");
    assert!(
        text.contains(&format!("#{seq} 留言：這張先不要做")),
        "{text}"
    );
    app.update(Event::Key(Key::Enter));
    app.update(Event::CardLoaded {
        board_id,
        seq,
        detail: Some(detail),
    });
    let text = kb_tui::render_text(&app, 100, 30);
    assert!(text.contains("留言   未回覆"), "{text}");

    // A reply from the agent clears 未回覆 and the tag.
    write::card_note(&mut writer, &session, seq, NoteKind::Reply, "好")
        .await
        .unwrap();
    let snap = kb_core::read::board_snapshot(&mut reader, board_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.unreplied_comment_count, 0);

    // Refusals come back with a readable reason: empty text, then an archived board.
    let mut empty = request.clone();
    empty.text = " \u{3000} ".into();
    let err = kb_tui::send_comment(&mut reader, &empty).await.unwrap_err();
    assert!(
        matches!(err, Error::Comment(CommentError::Empty)),
        "{err:?}"
    );
    write::board_archive(&mut writer, &session, None, None)
        .await
        .unwrap();
    let err = kb_tui::send_comment(&mut reader, request)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Comment(CommentError::BoardArchived)),
        "{err:?}"
    );
    assert_eq!(
        comment_error_text(&err),
        "看板已封存：請 agent 重新開啟看板"
    );
}
