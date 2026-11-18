//! `kb`: the tui-kanban command line (docs/cli.md).

mod cli;
mod guide;
mod hook;
mod output;

use std::io::{Read, Write};
use std::process::ExitCode;

use clap::Parser;
use kb_core::model::{self, LaneKind, LinkKind, NoteKind, Origin, Outcome};
use kb_core::read::{self, BoardQuery};
use kb_core::resolve::{self, SessionSource};
use kb_core::rules::{self, Levels};
use kb_core::write::{self, BoardInit, CardAdd, CardEdit, LinkTarget};
use kb_core::{Config, Error, PgConnection, Result, Role, db};

use cli::{
    BoardCmd, CardCmd, Cli, Cmd, DbCmd, EpicCmd, LaneCmd, LinkCmd, QuestionCmd, TuiArgs,
    WorkspaceCmd,
};

fn main() -> ExitCode {
    let mut cli = Cli::parse(); // syntax errors exit with code 2 here
    if cli.skill {
        if cli.cmd.is_some() {
            eprintln!("error: --skill cannot be used with a subcommand");
            return ExitCode::from(2);
        }
        print_stdout(&guide::skill_text());
        return ExitCode::SUCCESS;
    }
    let Some(cmd) = cli.cmd.take() else {
        // Only global options: show the help, as for a missing subcommand.
        let _ = <Cli as clap::CommandFactory>::command().print_help();
        return ExitCode::from(2);
    };
    let cli = Parsed {
        session: cli.session,
        json: cli.json,
        cmd,
    };

    // A hook never fails the session (docs/hooks.md section 2.3), and the hook bindings read
    // any other non-zero exit of `kb hook <event>` as "kb cannot run" (docs/board-v2.md
    // section 12.12). So a panic or a runtime failure in a hook exits 0.
    let is_hook = matches!(cli.cmd, Cmd::Hook { .. });
    if is_hook {
        std::panic::set_hook(Box::new(|info| {
            eprintln!("kb hook: internal error: {info}");
            std::process::exit(0);
        }));
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start runtime: {e}");
            return ExitCode::from(if is_hook { 0 } else { 1 });
        }
    };
    let result = match cli.cmd {
        // Hooks never fail the session: they handle every error themselves (exit 0; only
        // `wait` and `judge` exit 2, to wake the session).
        Cmd::Hook { event } => return ExitCode::from(runtime.block_on(hook::run(event))),
        Cmd::Tui(args) => runtime.block_on(tui(args)).map(|text| (text, 0)),
        Cmd::Board {
            cmd: BoardCmd::Check { history: false, .. },
        } => runtime.block_on(board_check(cli.session.as_deref(), cli.json)),
        Cmd::Board {
            cmd: BoardCmd::Check { limit, .. },
        } => runtime
            .block_on(check_history(
                cli.session.as_deref(),
                cli.json,
                limit.unwrap_or(HISTORY_LIMIT),
            ))
            .map(|text| (text, 0)),
        _ => runtime.block_on(run(cli)).map(|text| (text, 0)),
    };
    match result {
        Ok((text, code)) => {
            print_stdout(&text);
            ExitCode::from(code)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

/// The command line after `--skill` and the missing-subcommand case are handled.
struct Parsed {
    session: Option<String>,
    json: bool,
    cmd: Cmd,
}

/// Writes to stdout, ignoring a closed pipe (e.g. `kb board show | head`).
fn print_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    if !text.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

/// `kb tui`: the interactive screen, or one frame as text with `--snapshot`. Connects as
/// the reader role only.
async fn tui(args: TuiArgs) -> Result<String> {
    let defaults = kb_tui::Options::default();
    let opts = kb_tui::Options {
        board: args.board,
        workspace: args.workspace,
        width: args.width.unwrap_or(defaults.width),
        height: args.height.unwrap_or(defaults.height),
        card: args.card,
        epic: args.epic,
    };
    if args.snapshot {
        kb_tui::snapshot(&opts).await
    } else {
        kb_tui::run(opts).await?;
        Ok(String::new())
    }
}

/// Where the session key comes from (sources 1-4 of docs/cli.md section 2.3). Source 4
/// needs the database, so it is resolved after connecting ([`session_key`]).
fn session_source(flag: Option<&str>) -> Result<SessionSource> {
    resolve::session_source(flag, |k| std::env::var(k).ok())
}

async fn session_key(conn: &mut PgConnection, source: SessionSource) -> Result<String> {
    resolve::resolve_session(conn, source).await
}

/// Loads the rule levels; problems with the rules file are warnings on stderr.
fn rule_levels(prefix: &str) -> Levels {
    let (levels, warnings) = Levels::load();
    for w in warnings {
        eprintln!("{prefix}warning: {w}");
    }
    levels
}

/// `kb board check [--json]`: rules R1-R5 and R7-R11; exit 1 when a rule at level `block` fails.
/// Reads only.
async fn board_check(flag: Option<&str>, json: bool) -> Result<(String, u8)> {
    let source = session_source(flag)?;
    let config = Config::load()?;
    let mut conn = connect(&config).await?;
    let session = session_key(&mut conn, source).await?;
    let board = session_board(&mut conn, &session).await?;
    let levels = rule_levels("");
    // The [judge] table lives in the same file; the judge itself prints nothing.
    for w in kb_core::judge::JudgeConfig::load().1 {
        eprintln!("warning: {w}");
    }
    let results = rules::check_board(&mut conn, &board, &levels).await?;
    // Informational only: never changes the exit code (docs/board-v2.md section 13.9).
    let unreplied = read::unreplied_comments(&mut conn, board.id).await?;
    let blocked = results.iter().any(|r| r.blocks());
    let text = if json {
        output::json(&serde_json::json!({
            "session_key": board.session_key,
            "blocked": blocked,
            "rules": results,
            "unreplied_comments": unreplied,
        }))
    } else {
        output::check(&results, &unreplied)
    };
    Ok((text, if blocked { 1 } else { 0 }))
}

/// Default number of rows of `kb board check --history`.
const HISTORY_LIMIT: i64 = 20;

/// `kb board check --history [--limit N]`: the board's past rule failures, newest first
/// (docs/board-v2.md section 16.2). Reads only; exit 0.
async fn check_history(flag: Option<&str>, json: bool, limit: i64) -> Result<String> {
    let source = session_source(flag)?;
    let config = Config::load()?;
    let mut conn = connect(&config).await?;
    let session = session_key(&mut conn, source).await?;
    let board = session_board(&mut conn, &session).await?;
    let records = read::check_history(&mut conn, board.id, limit).await?;
    Ok(if json {
        output::json(&records)
    } else {
        output::check_history(&records)
    })
}

/// Runs one command and returns what to print on stdout.
async fn run(cli: Parsed) -> Result<String> {
    let json = cli.json;
    let session_flag = cli.session.clone();
    let source = || session_source(session_flag.as_deref());
    let (config, warnings) = Config::load_with_warnings()?;
    // Only `kb db` prints the config warning (admin_url): agents run the other commands
    // and cannot fix the file (docs/board-v2.md section 15.4).
    if matches!(cli.cmd, Cmd::Db { .. }) {
        for w in warnings {
            eprintln!("warning: {w}");
        }
    }

    match cli.cmd {
        Cmd::Tui(_) | Cmd::Hook { .. } => unreachable!("handled in main"),

        Cmd::Db {
            cmd: DbCmd::Migrate { database, yes },
        } => {
            // Confirmation on a database with boards (docs/board-v2.md section 15.5).
            let report = db::migrate_checked(&config, database.as_deref(), yes).await?;
            Ok(if json {
                output::json(&report)
            } else {
                output::migrate(&report)
            })
        }
        Cmd::Db { cmd: DbCmd::Status } => {
            let status = db::status(config.url(Role::Writer)?).await?;
            Ok(if json {
                output::json(&status)
            } else {
                output::db_status(&status)
            })
        }

        Cmd::Workspace {
            cmd: WorkspaceCmd::List,
        } => {
            let mut conn = connect(&config).await?;
            let list = read::list_workspaces(&mut conn).await?;
            Ok(if json {
                output::json(&list)
            } else {
                output::workspaces(&list)
            })
        }

        Cmd::Board { cmd } => {
            let mut conn = connect(&config).await?;
            match cmd {
                BoardCmd::Check { .. } => unreachable!("handled in main"),
                BoardCmd::Start { title, goal, new } => {
                    let session = start_session_key(session_flag.as_deref())?;
                    let cwd = current_dir()?;
                    let hc = resolve::current_hc();
                    let terminal = hc.as_ref().map(|h| h.terminal.clone());
                    let args = write::BoardStartArgs {
                        title,
                        goal: goal.map(|g| read_text_arg(&g)).transpose()?,
                        new,
                        cwd: Some(cwd.to_string_lossy().to_string()),
                    };
                    let resolve_ws = || resolve::workspace_here(None, hc, &cwd);
                    let start = write::board_start(
                        &mut conn,
                        &session,
                        args,
                        terminal.as_ref(),
                        resolve_ws,
                    )
                    .await?;
                    board_start_output(&mut conn, &start, json).await
                }
                BoardCmd::Adopt { target } => {
                    let session = start_session_key(session_flag.as_deref())?;
                    let terminal = resolve::current_hc().map(|h| h.terminal);
                    let adoption =
                        write::board_adopt(&mut conn, &session, &target, terminal.as_ref()).await?;
                    if json {
                        return Ok(output::json(&adoption));
                    }
                    let b = &adoption.board;
                    let first = format!(
                        "board {} adopted by session {} (was {})",
                        b.id, b.session_key, adoption.from
                    );
                    let snap = read::board_snapshot(&mut conn, b.id)
                        .await?
                        .ok_or_else(|| Error::NoBoard(b.session_key.clone()))?;
                    forget_next(&mut conn, b.id).await;
                    Ok(format!("{first}\n\n{}", guide::board_context(&snap)))
                }
                BoardCmd::Init {
                    title,
                    goal,
                    lanes,
                    epics,
                    workspace,
                } => {
                    let session = session_key(&mut conn, source()?).await?;
                    let lanes = lanes.as_deref().map(write::parse_lanes).transpose()?;
                    let epics = epics.as_deref().map(write::parse_epics).transpose()?;
                    let cwd = current_dir()?;
                    let args = BoardInit {
                        title,
                        goal: goal.map(|g| read_text_arg(&g)).transpose()?,
                        lanes,
                        epics,
                        cwd: Some(cwd.to_string_lossy().to_string()),
                    };
                    // The terminal is stored whatever the workspace source
                    // (docs/board-v2.md section 12.7).
                    let resolve_ws = || {
                        resolve::workspace_here(workspace.as_deref(), resolve::current_hc(), &cwd)
                    };
                    let out = write::board_init(&mut conn, &session, args, resolve_ws).await?;
                    write_result(&mut conn, &out, json, false).await
                }
                BoardCmd::Show { all } => {
                    let session = session_key(&mut conn, source()?).await?;
                    let board = session_board(&mut conn, &session).await?;
                    let snap = read::board_snapshot(&mut conn, board.id)
                        .await?
                        .ok_or_else(|| Error::NoBoard(board.session_key.clone()))?;
                    // --json is always complete (docs/board-v2.md section 15.1).
                    Ok(if json {
                        output::json(&snap)
                    } else {
                        output::board(&snap, all)
                    })
                }
                BoardCmd::List {
                    workspace,
                    all,
                    archived,
                } => {
                    let workspace_ids: Vec<Option<i64>> = if all {
                        vec![None]
                    } else if let Some(name) = workspace {
                        let found = read::workspaces_by_name(&mut conn, &name).await?;
                        if found.is_empty() {
                            return Err(Error::WorkspaceNotFound(name));
                        }
                        found.iter().map(|w| Some(w.id)).collect()
                    } else {
                        // Default: the workspace of the session's board; all workspaces when
                        // the session has no board (or no session key is set).
                        let key = match source() {
                            Ok(src) => session_key(&mut conn, src).await.ok(),
                            Err(_) => None,
                        };
                        let board = match key {
                            Some(key) => read::board_by_session(&mut conn, &key).await?,
                            None => None,
                        };
                        vec![board.map(|b| b.workspace_id)]
                    };
                    let mut boards = Vec::new();
                    for workspace_id in workspace_ids {
                        let q = BoardQuery {
                            workspace_id,
                            include_archived: archived,
                        };
                        boards.extend(read::list_boards(&mut conn, q).await?);
                    }
                    Ok(if json {
                        output::json(&boards)
                    } else {
                        output::boards(&boards)
                    })
                }
                BoardCmd::Archive { outcome, summary } => {
                    let outcome = outcome.as_deref().map(str::parse::<Outcome>).transpose()?;
                    let summary = summary.map(|s| read_text_arg(&s)).transpose()?;
                    let session = session_key(&mut conn, source()?).await?;
                    let out = write::board_archive(&mut conn, &session, outcome, summary).await?;
                    write_result(&mut conn, &out, json, false).await
                }
                BoardCmd::Reopen => {
                    let session = session_key(&mut conn, source()?).await?;
                    let out = write::board_reopen(&mut conn, &session).await?;
                    write_result(&mut conn, &out, json, false).await
                }
                BoardCmd::Tidy => {
                    let session = session_key(&mut conn, source()?).await?;
                    let out = write::board_tidy(&mut conn, &session).await?;
                    write_result(&mut conn, &out, json, false).await
                }
            }
        }

        Cmd::Lane { cmd } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            let out = match cmd {
                LaneCmd::Add {
                    name,
                    kind,
                    after,
                    wip,
                } => {
                    let kind: LaneKind = kind.parse()?;
                    write::lane_add(&mut conn, &session, &name, kind, after.as_deref(), wip).await?
                }
                LaneCmd::Rename { name, new_name } => {
                    write::lane_rename(&mut conn, &session, &name, &new_name).await?
                }
                LaneCmd::Move { name, position } => {
                    write::lane_move(&mut conn, &session, &name, position).await?
                }
                LaneCmd::Remove { name } => write::lane_remove(&mut conn, &session, &name).await?,
            };
            write_result(&mut conn, &out, json, false).await
        }

        Cmd::Epic { cmd } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            let out = match cmd {
                EpicCmd::Add { name, after } => {
                    write::epic_add(&mut conn, &session, &name, after.as_deref()).await?
                }
                EpicCmd::Rename { name, new_name } => {
                    write::epic_rename(&mut conn, &session, &name, &new_name).await?
                }
                EpicCmd::Move { name, position } => {
                    write::epic_move(&mut conn, &session, &name, position).await?
                }
                EpicCmd::Remove { name } => write::epic_remove(&mut conn, &session, &name).await?,
                EpicCmd::List => {
                    let board = session_board(&mut conn, &session).await?;
                    let snap = read::board_snapshot(&mut conn, board.id)
                        .await?
                        .ok_or_else(|| Error::NoBoard(board.session_key.clone()))?;
                    return Ok(if json {
                        output::json(&snap.epics)
                    } else {
                        output::epics(&snap.epics)
                    });
                }
            };
            write_result(&mut conn, &out, json, false).await
        }

        Cmd::Card { cmd } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            // `kb card next` names the next card: it always echoes it (section 15.2).
            let always_next = matches!(cmd, CardCmd::Next { .. });
            let out = match cmd {
                CardCmd::Add {
                    lane,
                    title,
                    priority,
                    body,
                    owner,
                    from,
                    epic,
                    discuss,
                    agreed,
                    next,
                } => {
                    let mut args = CardAdd::new(lane, title);
                    args.priority = model::parse_priority(&priority)?;
                    args.body = body_input(body.body, body.body_file)?.unwrap_or_default();
                    args.owner = owner;
                    args.from = from.as_deref().map(model::parse_card_seq).transpose()?;
                    args.epic = epic;
                    args.origin = if discuss {
                        Origin::Discussing
                    } else if agreed {
                        Origin::Agreed
                    } else {
                        Origin::Derived
                    };
                    args.next = next;
                    write::card_add(&mut conn, &session, args).await?
                }
                CardCmd::Next { card } => {
                    let seq = model::parse_card_seq(&card)?;
                    write::card_next(&mut conn, &session, seq).await?
                }
                CardCmd::Order { cards } => {
                    let seqs = parse_card_seqs(&cards)?;
                    write::card_order(&mut conn, &session, &seqs).await?
                }
                CardCmd::Confirm { cards } => {
                    let seqs = parse_card_seqs(&cards)?;
                    write::card_set_origin(&mut conn, &session, &seqs, Origin::Agreed).await?
                }
                CardCmd::Discuss { cards } => {
                    let seqs = parse_card_seqs(&cards)?;
                    write::card_set_origin(&mut conn, &session, &seqs, Origin::Discussing).await?
                }
                CardCmd::Move {
                    card,
                    to,
                    note,
                    next,
                } => {
                    let seq = model::parse_card_seq(&card)?;
                    let note = note.map(|n| read_text_arg(&n)).transpose()?;
                    write::card_move_to(&mut conn, &session, seq, &to, note, next).await?
                }
                CardCmd::Edit {
                    card,
                    title,
                    body,
                    priority,
                    owner,
                    epic,
                    no_epic,
                } => {
                    let seq = model::parse_card_seq(&card)?;
                    let edit = CardEdit {
                        title,
                        body: body_input(body.body, body.body_file)?,
                        priority: priority.as_deref().map(model::parse_priority).transpose()?,
                        owner,
                        epic: if no_epic { Some(None) } else { epic.map(Some) },
                    };
                    write::card_edit(&mut conn, &session, seq, edit).await?
                }
                CardCmd::Note {
                    card,
                    kind,
                    text,
                    because,
                } => {
                    let seq = model::parse_card_seq(&card)?;
                    let kind: NoteKind = kind.parse()?;
                    let because = parse_because(&because.because)?;
                    let text = read_text_arg(&text)?;
                    write::card_note_because(&mut conn, &session, seq, kind, &text, &because)
                        .await?
                }
                CardCmd::Block { card, reason } => {
                    let seq = model::parse_card_seq(&card)?;
                    write::card_block(&mut conn, &session, seq, &read_text_arg(&reason)?).await?
                }
                CardCmd::Unblock { card } => {
                    write::card_unblock(&mut conn, &session, model::parse_card_seq(&card)?).await?
                }
                CardCmd::Done {
                    card,
                    result,
                    because,
                } => {
                    let seq = model::parse_card_seq(&card)?;
                    let because = parse_because(&because.because)?;
                    let result = read_text_arg(&result)?;
                    write::card_done_because(&mut conn, &session, seq, &result, &because).await?
                }
                CardCmd::Drop {
                    card,
                    reason,
                    because,
                } => {
                    let seq = model::parse_card_seq(&card)?;
                    let because = parse_because(&because.because)?;
                    let reason = read_text_arg(&reason)?;
                    write::card_drop_because(&mut conn, &session, seq, &reason, &because).await?
                }
                CardCmd::Show { card } => {
                    let seq = model::parse_card_seq(&card)?;
                    let board = session_board(&mut conn, &session).await?;
                    let detail = read::card_detail(&mut conn, board.id, seq)
                        .await?
                        .ok_or(Error::CardNotFound(seq))?;
                    return Ok(if json {
                        output::json(&detail)
                    } else {
                        output::card(&detail)
                    });
                }
            };
            write_result(&mut conn, &out, json, always_next).await
        }

        Cmd::Link { cmd } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            let out = match cmd {
                LinkCmd::Add {
                    card,
                    blocks,
                    relates,
                    continues,
                    r#ref,
                    label,
                } => {
                    let seq = model::parse_card_seq(&card)?;
                    let (kind, target) = if let Some(to) = blocks {
                        (
                            LinkKind::Blocks,
                            LinkTarget::Card(model::parse_card_seq(&to)?),
                        )
                    } else if let Some(to) = relates {
                        (
                            LinkKind::Relates,
                            LinkTarget::Card(model::parse_card_seq(&to)?),
                        )
                    } else if let Some(to) = continues {
                        (
                            LinkKind::Continues,
                            LinkTarget::Card(model::parse_card_seq(&to)?),
                        )
                    } else if let Some(target) = r#ref {
                        (LinkKind::Ref, LinkTarget::External(target))
                    } else {
                        unreachable!("clap requires one link kind")
                    };
                    write::link_add(&mut conn, &session, seq, kind, target, label).await?
                }
                LinkCmd::Remove { id } => write::link_remove(&mut conn, &session, id).await?,
            };
            write_result(&mut conn, &out, json, false).await
        }

        Cmd::Activity { limit } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            let board = session_board(&mut conn, &session).await?;
            let items = read::recent_activity(&mut conn, board.id, limit).await?;
            Ok(if json {
                output::json(&items)
            } else {
                output::activity(&items)
            })
        }

        Cmd::Ask {
            card,
            question,
            options,
            multi,
        } => {
            let seq = model::parse_card_seq(&card)?;
            let options = options
                .iter()
                .map(|o| write::parse_option(o))
                .collect::<Result<Vec<_>>>()?;
            let text = read_text_arg(&question)?;
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            let out = write::question_ask(&mut conn, &session, seq, &text, options, multi).await?;
            write_result(&mut conn, &out, json, false).await
        }

        Cmd::Inbox { peek } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            let inbox = write::inbox(&mut conn, &session, peek).await?;
            Ok(if json {
                output::json(&inbox)
            } else {
                output::inbox(&inbox)
            })
        }

        Cmd::Question { cmd } => {
            let source = source()?;
            let mut conn = connect(&config).await?;
            let session = session_key(&mut conn, source).await?;
            match cmd {
                QuestionCmd::List { open } => {
                    let board = session_board(&mut conn, &session).await?;
                    let list = read::board_questions(&mut conn, board.id, open).await?;
                    Ok(if json {
                        output::json(&list)
                    } else {
                        output::questions(&list)
                    })
                }
                QuestionCmd::Show { id } => {
                    let board = session_board(&mut conn, &session).await?;
                    let q = read::question(&mut conn, board.id, id)
                        .await?
                        .ok_or(Error::QuestionNotFound(id))?;
                    Ok(if json {
                        output::json(&q)
                    } else {
                        output::question(&q)
                    })
                }
                QuestionCmd::Cancel { id, reason } => {
                    let reason = reason.map(|r| read_text_arg(&r)).transpose()?;
                    let out = write::question_cancel(&mut conn, &session, id, reason).await?;
                    write_result(&mut conn, &out, json, false).await
                }
            }
        }
    }
}

/// The session key of `kb board start` and `kb board adopt`: sources 1-3 only. Source 4
/// would name a board that already exists (docs/board-v2.md sections 12.4 and 12.5).
fn start_session_key(flag: Option<&str>) -> Result<String> {
    resolve::resolve_session_key(flag, |k| std::env::var(k).ok())
}

fn current_dir() -> Result<std::path::PathBuf> {
    std::env::current_dir()
        .map_err(|e| Error::invalid(format!("cannot read current directory: {e}")))
}

/// Output of `kb board start` (docs/board-v2.md section 12.4).
async fn board_start_output(
    conn: &mut PgConnection,
    start: &write::BoardStart,
    json: bool,
) -> Result<String> {
    if json {
        return Ok(output::json(start));
    }
    let key = &start.session_key;
    let Some(board) = &start.board else {
        return Ok(output::start_candidates(key, &start.candidates));
    };
    let first = match start.status {
        write::StartStatus::Active => {
            format!("board {} is already active for session {key}", board.id)
        }
        write::StartStatus::Reopened => format!("board {} reopened for session {key}", board.id),
        _ => {
            let ws = read::workspace_by_id(conn, board.workspace_id)
                .await?
                .map(|w| w.name)
                .unwrap_or_default();
            format!(
                "board {} created for session {key} in workspace {ws}",
                board.id
            )
        }
    };
    let snap = read::board_snapshot(conn, board.id)
        .await?
        .ok_or_else(|| Error::NoBoard(key.clone()))?;
    forget_next(conn, board.id).await;
    Ok(format!("{first}\n\n{}", guide::board_context(&snap)))
}

/// The agent's context receives the board without a `next:` line: forget the last one so
/// the next write prints it (docs/board-v2.md section 15.2). A failure (e.g. schema version
/// 5 without the column) only means the next write prints the line anyway.
async fn forget_next(conn: &mut PgConnection, board_id: i64) {
    let _ = write::forget_next(conn, board_id).await;
}

async fn connect(config: &Config) -> Result<PgConnection> {
    db::connect(config.url(Role::Writer)?).await
}

async fn session_board(conn: &mut PgConnection, session: &str) -> Result<model::Board> {
    read::board_by_session(conn, session)
        .await?
        .ok_or_else(|| Error::NoBoard(session.to_string()))
}

/// Formats the result of a successful write command; warnings go to stderr. A write ends
/// with the card that will be done next (docs/board-v2.md sections 10.2 and 15.2): with
/// `--json` always a `next` field; as text a last line `next: ...` only when it differs
/// from the board's last one, or when `always` (`kb card next`). Read commands never come
/// here.
async fn write_result(
    conn: &mut PgConnection,
    out: &write::WriteOutcome,
    json: bool,
    always: bool,
) -> Result<String> {
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    let snap = read::board_snapshot(conn, out.board_id).await?;
    let next = snap.as_ref().and_then(|s| s.next_card());
    let todo_cards = snap.as_ref().map_or(0, |s| s.todo_card_count());
    let line = output::next_line(next, todo_cards);
    // The write itself is committed: failing to remember the line (e.g. schema version 5
    // without the column) must not fail the command, so the line is printed instead.
    let changed = write::record_next(conn, out.board_id, &line)
        .await
        .unwrap_or(true);
    if json {
        let mut value = serde_json::to_value(out)
            .map_err(|e| Error::invalid(format!("cannot encode the result: {e}")))?;
        value["next"] = match next {
            Some((lane, c)) => serde_json::json!({
                "seq": c.card.seq,
                "title": c.card.title,
                "lane": lane.name,
                "blocked": c.is_blocked(),
            }),
            None => serde_json::Value::Null,
        };
        Ok(output::json(&value))
    } else {
        let mut text = out.message.clone();
        if changed || always {
            text = format!("{text}\n{line}");
        }
        // After the normal echo, never on stderr (docs/board-v2.md section 18.5).
        if let Some(hint) = &out.hint {
            text = format!("{text}\nhint: {hint}");
        }
        Ok(text)
    }
}

/// The `--because` values, checked for syntax only (docs/board-v2.md section 18.2); the
/// write checks them against the board.
fn parse_because(refs: &[String]) -> Result<Vec<model::CauseRef>> {
    refs.iter().map(|r| model::CauseRef::parse(r)).collect()
}

fn parse_card_seqs(cards: &[String]) -> Result<Vec<i32>> {
    cards.iter().map(|c| model::parse_card_seq(c)).collect()
}

/// A text argument; `-` means "read standard input".
fn read_text_arg(value: &str) -> Result<String> {
    if value != "-" {
        return Ok(value.to_string());
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid(format!("cannot read standard input: {e}")))?;
    decode_text(bytes, "standard input")
}

/// `--body <text|->` or `--body-file <path>` (docs/cli.md section 2.6).
fn body_input(body: Option<String>, body_file: Option<String>) -> Result<Option<String>> {
    match (body, body_file) {
        (Some(b), _) => Ok(Some(read_text_arg(&b)?)),
        (None, Some(path)) => {
            let bytes = std::fs::read(&path)
                .map_err(|e| Error::invalid(format!("cannot read {path}: {e}")))?;
            Ok(Some(decode_text(bytes, &path)?))
        }
        (None, None) => Ok(None),
    }
}

/// UTF-8 text without a BOM and without trailing line breaks.
fn decode_text(mut bytes: Vec<u8>, source: &str) -> Result<String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::invalid(format!("{source} is not valid UTF-8")))?;
    Ok(text.trim_end_matches(['\r', '\n']).replace("\r\n", "\n"))
}
