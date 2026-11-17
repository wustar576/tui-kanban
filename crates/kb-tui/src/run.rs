//! The IO side: startup resolution, the loader task that owns the query connection, the
//! event loop that merges keys, notifications, load results and a tick, and the
//! `--snapshot` renderer. Everything else is in `app` (state) and `view` (drawing).

use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use kb_core::events::{self, ListenEvent};
use kb_core::read;
use kb_core::{Config, Error, PgConnection, Result, Role, db};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use crate::app::{
    AnswerRequest, App, BoardData, BoardTarget, CommentRequest, Effect, EpicKey, Event, Key, Scope,
};
use crate::{notify, view};

/// Options of `kb tui` (docs/tui.md section 2).
#[derive(Debug, Clone)]
pub struct Options {
    /// Session key of the board to show.
    pub board: Option<String>,
    /// Workspace name (or key) whose latest active board to show.
    pub workspace: Option<String>,
    /// Size of the `--snapshot` frame.
    pub width: u16,
    pub height: u16,
    /// `--snapshot --card <n>`: render that card's screen instead of the board.
    pub card: Option<i32>,
    /// `--snapshot --epic <name>`: render that epic's screen (「未歸類」 for the cards
    /// without an epic).
    pub epic: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            board: None,
            workspace: None,
            width: 120,
            height: 30,
            card: None,
            epic: None,
        }
    }
}

/// Activity entries loaded with a board (the bottom strip shows the newest three).
const ACTIVITY_LIMIT: i64 = 10;
/// A query that takes longer than this is treated as a lost connection.
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Tick interval: debounce resolution and highlight expiry.
const TICK: Duration = Duration::from_millis(50);

fn terminal_error(e: std::io::Error) -> Error {
    Error::invalid(format!("terminal error: {e}"))
}

fn reader_url() -> Result<String> {
    Ok(Config::load()?.url(Role::Reader)?.to_string())
}

/// The most recently changed active board in `scope`.
async fn latest_in_scope(conn: &mut PgConnection, scope: &Scope) -> Result<Option<i64>> {
    match scope {
        Scope::All => Ok(read::latest_active_board(conn, None).await?.map(|b| b.id)),
        Scope::Workspaces(ids) => {
            let mut best: Option<read::BoardSummary> = None;
            for id in ids {
                if let Some(b) = read::latest_active_board(conn, Some(*id)).await?
                    && best.as_ref().is_none_or(|x| b.updated_at > x.updated_at)
                {
                    best = Some(b);
                }
            }
            Ok(best.map(|b| b.id))
        }
    }
}

/// Which board to show first (docs/tui.md section 2.1), and which boards an empty screen
/// waits for.
pub async fn resolve_start(
    conn: &mut PgConnection,
    opts: &Options,
) -> Result<(Scope, Option<i64>)> {
    if let Some(key) = opts.board.as_deref() {
        let board = read::board_by_session(conn, key)
            .await?
            .ok_or_else(|| Error::NoBoard(key.to_string()))?;
        return Ok((Scope::Workspaces(vec![board.workspace_id]), Some(board.id)));
    }
    if let Some(name) = opts.workspace.as_deref() {
        let found = read::workspaces_by_name(conn, name).await?;
        if found.is_empty() {
            return Err(Error::WorkspaceNotFound(name.to_string()));
        }
        let scope = Scope::Workspaces(found.iter().map(|w| w.id).collect());
        let id = latest_in_scope(conn, &scope).await?;
        return Ok((scope, id));
    }
    // HC_TERMINAL_ID: the workspace of this herdr space, if it has an active board.
    if let Some((_, Some(ws))) = read::workspace_for_current_terminal(conn).await? {
        let scope = Scope::Workspaces(vec![ws.id]);
        if let Some(id) = latest_in_scope(conn, &scope).await? {
            return Ok((scope, Some(id)));
        }
    }
    let id = latest_in_scope(conn, &Scope::All).await?;
    Ok((Scope::All, id))
}

async fn load_board(conn: &mut PgConnection, target: &BoardTarget) -> Result<Option<BoardData>> {
    let id = match target {
        BoardTarget::Id(id) => *id,
        BoardTarget::Latest(scope) => match latest_in_scope(conn, scope).await? {
            Some(id) => id,
            None => return Ok(None),
        },
    };
    let Some(snapshot) = read::board_snapshot(conn, id).await? else {
        return Ok(None);
    };
    let activity = read::recent_feed(conn, id, ACTIVITY_LIMIT).await?;
    let findings = read::active_judge_findings(conn, id).await?;
    Ok(Some(BoardData {
        snapshot,
        activity,
        findings,
    }))
}

/// Runs one effect and returns the event that carries its result.
async fn execute(conn: &mut PgConnection, effect: &Effect) -> Result<Event> {
    Ok(match effect {
        Effect::LoadBoard(target) => Event::BoardLoaded {
            target: target.clone(),
            data: load_board(conn, target).await?,
            at: Instant::now(),
        },
        Effect::LoadCard { board_id, seq } => Event::CardLoaded {
            board_id: *board_id,
            seq: *seq,
            detail: read::card_detail(conn, *board_id, *seq).await?,
        },
        Effect::LoadWorkspaces => Event::WorkspacesLoaded(read::list_workspaces(conn).await?),
        Effect::LoadBoards { workspace_id } => Event::BoardsLoaded {
            workspace_id: *workspace_id,
            boards: read::list_boards(
                conn,
                read::BoardQuery {
                    workspace_id: *workspace_id,
                    include_archived: true,
                },
            )
            .await?,
        },
        Effect::Answer(_) => unreachable!("answers are sent by send_answer"),
        Effect::Comment(_) => unreachable!("comments are sent by send_comment"),
    })
}

/// Sends an answer the way the TUI does: `kb_answer_question` through
/// [`kb_core::answer::answer_question`], on the `kb_reader` connection
/// (docs/questions.md section 3). With [`send_comment`], the TUI's only writes
/// (docs/board-v2.md section 13.20).
pub async fn send_answer(conn: &mut PgConnection, req: &AnswerRequest) -> Result<()> {
    kb_core::answer::answer_question(
        conn,
        req.question_id,
        &req.option_ids,
        req.chat,
        req.text.as_deref(),
    )
    .await
}

/// Adds a comment the way the TUI does: `kb_add_comment` through
/// [`kb_core::comment::add_comment`], on the `kb_reader` connection (docs/board-v2.md
/// section 13.15). Returns the new note's id.
pub async fn send_comment(conn: &mut PgConnection, req: &CommentRequest) -> Result<i64> {
    kb_core::comment::add_comment(conn, req.card_id, &req.text).await
}

/// The bottom-line text for a refused comment.
pub fn comment_error_text(e: &Error) -> String {
    match e {
        Error::Comment(c) => c.zh_tw().to_string(),
        _ => "無法連線到資料庫".to_string(),
    }
}

/// The bottom-line text for a failed answer.
pub fn answer_error_text(e: &Error) -> String {
    match e {
        Error::Answer(a) => a.zh_tw().to_string(),
        _ => "無法連線到資料庫".to_string(),
    }
}

/// Runs an [`Effect::Answer`] on the loader's connection and reports
/// [`Event::Answered`]. After 「在 session 裡說明」 the pane focus runs in its own task and
/// reports [`Event::Notified`] only when it worked (docs/board-v2.md section 13.19).
async fn answer_effect(
    url: &str,
    conn: &mut Option<PgConnection>,
    req: &AnswerRequest,
    events: &mpsc::UnboundedSender<Event>,
) {
    if conn.is_none() {
        *conn = db::connect(url).await.ok();
    }
    let error = match conn.as_mut() {
        None => Some("無法連線到資料庫".to_string()),
        Some(c) => match tokio::time::timeout(QUERY_TIMEOUT, send_answer(c, req)).await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                if !matches!(e, Error::Answer(_)) {
                    *conn = None;
                }
                Some(answer_error_text(&e))
            }
            Err(_) => {
                *conn = None;
                Some("無法連線到資料庫".to_string())
            }
        },
    };
    if error.is_none() && req.chat {
        let events = events.clone();
        let terminal = req.terminal.clone();
        tokio::spawn(async move {
            if let Some(line) = notify::after_answer(terminal.as_deref(), true).await {
                let _ = events.send(Event::Notified(line));
            }
        });
    }
    let _ = events.send(Event::Answered {
        question_id: req.question_id,
        error,
    });
}

/// Runs an [`Effect::Comment`] on the loader's connection and reports
/// [`Event::Commented`].
async fn comment_effect(
    url: &str,
    conn: &mut Option<PgConnection>,
    req: &CommentRequest,
    events: &mpsc::UnboundedSender<Event>,
) {
    if conn.is_none() {
        *conn = db::connect(url).await.ok();
    }
    let error = match conn.as_mut() {
        None => Some("無法連線到資料庫".to_string()),
        Some(c) => match tokio::time::timeout(QUERY_TIMEOUT, send_comment(c, req)).await {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => {
                if !matches!(e, Error::Comment(_)) {
                    *conn = None;
                }
                Some(comment_error_text(&e))
            }
            Err(_) => {
                *conn = None;
                Some("無法連線到資料庫".to_string())
            }
        },
    };
    let _ = events.send(Event::Commented {
        card_id: req.card_id,
        error,
    });
}

/// The loader task: owns the query connection (separate from the listener's), runs
/// effects in order, and reconnects on the next effect after a failure.
fn spawn_loader(
    url: String,
    conn: PgConnection,
    events: mpsc::UnboundedSender<Event>,
) -> mpsc::UnboundedSender<Effect> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Effect>();
    tokio::spawn(async move {
        let mut conn = Some(conn);
        while let Some(first) = rx.recv().await {
            // Merge what queued up meanwhile; identical loads run once.
            let mut batch = vec![first];
            while let Ok(e) = rx.try_recv() {
                if !batch.contains(&e) {
                    batch.push(e);
                }
            }
            'batch: for effect in batch {
                match &effect {
                    Effect::Answer(req) => {
                        answer_effect(&url, &mut conn, req, &events).await;
                        continue;
                    }
                    Effect::Comment(req) => {
                        comment_effect(&url, &mut conn, req, &events).await;
                        continue;
                    }
                    _ => {}
                }
                // An old connection may have died unnoticed: on failure, try once more
                // on a fresh connection before reporting the failure.
                for attempt in 0..2 {
                    let fresh = conn.is_none();
                    if fresh {
                        match db::connect(&url).await {
                            Ok(c) => conn = Some(c),
                            Err(_) => {
                                let _ = events.send(Event::LoadFailed { at: Instant::now() });
                                break 'batch;
                            }
                        }
                    }
                    let c = conn.as_mut().expect("connected above");
                    match tokio::time::timeout(QUERY_TIMEOUT, execute(c, &effect)).await {
                        Ok(Ok(ev)) => {
                            if events.send(ev).is_err() {
                                return;
                            }
                            break;
                        }
                        Ok(Err(_)) | Err(_) => {
                            conn = None;
                            if fresh || attempt == 1 {
                                let _ = events.send(Event::LoadFailed { at: Instant::now() });
                                break 'batch;
                            }
                        }
                    }
                }
            }
        }
    });
    tx
}

/// Maps a terminal event to an [`Event`]. Only key presses (and repeats) count: on
/// Windows crossterm also reports key releases.
pub fn translate(ev: TermEvent) -> Option<Event> {
    match ev {
        TermEvent::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            let alt = k.modifiers.contains(KeyModifiers::ALT);
            let key = match k.code {
                KeyCode::Char('c') | KeyCode::Char('C') if ctrl => Key::CtrlC,
                KeyCode::Char('u') | KeyCode::Char('U') if ctrl && !alt => Key::CtrlU,
                // AltGr arrives as Ctrl+Alt: the character it produces is typed text.
                KeyCode::Char(c) if ctrl && alt => Key::Char(c),
                _ if ctrl || alt => Key::Other,
                KeyCode::Left => Key::Left,
                KeyCode::Right => Key::Right,
                KeyCode::Up => Key::Up,
                KeyCode::Down => Key::Down,
                KeyCode::Home => Key::Home,
                KeyCode::End => Key::End,
                KeyCode::PageUp => Key::PageUp,
                KeyCode::PageDown => Key::PageDown,
                KeyCode::Enter => Key::Enter,
                KeyCode::Esc => Key::Esc,
                KeyCode::Backspace => Key::Backspace,
                KeyCode::Delete => Key::Delete,
                KeyCode::Tab if k.modifiers.contains(KeyModifiers::SHIFT) => Key::BackTab,
                KeyCode::Tab => Key::Tab,
                KeyCode::BackTab => Key::BackTab,
                KeyCode::Char(c) => Key::Char(c),
                _ => Key::Other,
            };
            Some(Event::Key(key))
        }
        TermEvent::Resize(w, h) => Some(Event::Resize(w, h)),
        TermEvent::Paste(text) => Some(Event::Paste(text)),
        _ => None,
    }
}

/// `kb tui`: the interactive screen. Returns when the user quits.
pub async fn run(opts: Options) -> Result<()> {
    let url = reader_url()?;
    let mut conn = db::connect(&url).await?;
    let (scope, want) = resolve_start(&mut conn, &opts).await?;
    let listener = events::spawn_listener(&url).await?;
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let effects = spawn_loader(url, conn, ev_tx);

    let mut terminal = ratatui::try_init().map_err(terminal_error)?;
    // Pasted text arrives as one event (typed into a question's input row). Terminals or
    // platforms without bracketed paste deliver it as key presses instead, which also
    // type. Turned off again on exit and on panic (ratatui's hook restores the rest).
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
        previous_hook(info);
    }));
    let result = event_loop(&mut terminal, scope, want, listener, ev_rx, effects).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    scope: Scope,
    want: Option<i64>,
    mut listener: mpsc::UnboundedReceiver<ListenEvent>,
    mut loaded: mpsc::UnboundedReceiver<Event>,
    effects: mpsc::UnboundedSender<Effect>,
) -> Result<()> {
    let size = terminal.size().map_err(terminal_error)?;
    let mut app = App::new(
        (size.width, size.height),
        Local::now().date_naive(),
        scope,
        want,
    );
    let send = |fx: Vec<Effect>| {
        for e in fx {
            let _ = effects.send(e);
        }
    };
    send(app.start());

    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;

    loop {
        if dirty {
            terminal
                .draw(|f| view::draw(f, &app))
                .map_err(terminal_error)?;
            dirty = false;
        }
        let event = tokio::select! {
            maybe = keys.next() => match maybe {
                Some(Ok(ev)) => translate(ev),
                Some(Err(e)) => return Err(terminal_error(e)),
                None => return Ok(()),
            },
            Some(le) = listener.recv() => Some(match le {
                ListenEvent::Board(b) => Event::Notify {
                    workspace: b.workspace,
                    board: b.board,
                    at: Instant::now(),
                },
                ListenEvent::Disconnected => Event::ListenerDown,
                ListenEvent::Reconnected => Event::ListenerUp,
            }),
            Some(ev) = loaded.recv() => Some(ev),
            _ = tick.tick() => {
                let today = Local::now().date_naive();
                if today != app.today {
                    app.today = today;
                    dirty = true;
                }
                Some(Event::Tick(Instant::now()))
            }
        };
        let Some(event) = event else { continue };
        let is_tick = matches!(event, Event::Tick(_));
        let before = (app.highlights.len(), app.connected());
        send(app.update(event));
        if !is_tick || before != (app.highlights.len(), app.connected()) {
            dirty = true;
        }
        if app.quit {
            return Ok(());
        }
    }
}

/// `kb tui --snapshot`: one frame of the board screen (or with `--card`, of a card
/// screen) as plain text (UTF-8), drawn through ratatui's `TestBackend`. Never touches
/// the terminal.
pub async fn snapshot(opts: &Options) -> Result<String> {
    let url = reader_url()?;
    let mut conn = db::connect(&url).await?;
    let (scope, want) = resolve_start(&mut conn, opts).await?;
    let mut app = App::new(
        (opts.width, opts.height),
        Local::now().date_naive(),
        scope,
        want,
    );
    let target = app.board_target();
    let data = load_board(&mut conn, &target).await?;
    app.update(Event::BoardLoaded {
        target,
        data,
        at: Instant::now(),
    });
    // A snapshot shows the board as it is, without "just changed" highlights.
    app.highlights.clear();
    if let Some(seq) = opts.card {
        for effect in app.open_card(seq) {
            if let Effect::LoadCard { board_id, seq } = effect {
                let detail = read::card_detail(&mut conn, board_id, seq)
                    .await?
                    .ok_or(Error::CardNotFound(seq))?;
                app.update(Event::CardLoaded {
                    board_id,
                    seq,
                    detail: Some(detail),
                });
            }
        }
        if app.card.is_none() {
            return Err(Error::CardNotFound(seq));
        }
    } else if let Some(name) = opts.epic.as_deref() {
        let snap = app
            .data
            .as_ref()
            .map(|d| &d.snapshot)
            .ok_or_else(|| Error::NoBoard(opts.board.clone().unwrap_or_default()))?;
        let key = if name == "未歸類" {
            EpicKey::NoEpic
        } else {
            match snap.epics.iter().find(|e| e.epic.name == name) {
                Some(e) => EpicKey::Epic(e.epic.id),
                None => {
                    return Err(Error::EpicNotFound {
                        name: name.to_string(),
                        available: snap.epics.iter().map(|e| e.epic.name.clone()).collect(),
                    });
                }
            }
        };
        app.open_epic(key);
    }
    Ok(render_text(&app, opts.width, opts.height))
}

/// Renders `app` at `width` × `height` through a `TestBackend` and returns the text.
pub fn render_text(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend never fails");
    terminal
        .draw(|f| view::draw(f, app))
        .expect("TestBackend never fails");
    view::buffer_to_text(terminal.backend().buffer())
}
