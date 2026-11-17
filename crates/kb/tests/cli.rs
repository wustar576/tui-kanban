//! End-to-end tests of the `kb` binary: exit codes, messages, JSON, UTF-8 text and stdin.
//! Database commands run against `kanban_test` only.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use kb_core::{Config, Role};

fn test_url(role: Role) -> String {
    let config = Config::load().expect("load config");
    let mut url = url::Url::parse(
        config
            .url(role)
            .expect("database url configured (the tests also need KB_ADMIN_URL)"),
    )
    .unwrap();
    url.set_path("kanban_test");
    url.to_string()
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("test-cli-{prefix}-{}-{nanos}", std::process::id())
}

/// `kb` with database URLs pointing at kanban_test and no inherited session variables.
fn kb() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_WRITER_URL", test_url(Role::Writer))
        .env("KB_READER_URL", test_url(Role::Reader))
        .env("KB_ADMIN_URL", test_url(Role::Admin))
        .env_remove("KB_SESSION")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("HC_TERMINAL_ID");
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("run kb")
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).expect("stderr is UTF-8")
}

fn ok(cmd: &mut Command) -> String {
    let o = run(cmd);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    stdout(&o)
}

fn migrate() {
    ok(kb().args(["db", "migrate", "--database", "kanban_test", "--yes"]));
}

#[test]
fn exit_code_2_on_syntax_error() {
    let o = run(kb().args(["card", "frobnicate"]));
    assert_eq!(o.status.code(), Some(2));
    let o = run(kb().args(["card", "add", "--lane", "待辦"])); // missing --title
    assert_eq!(o.status.code(), Some(2));
    let o = run(kb().args(["card", "edit", "1"])); // no field
    assert_eq!(o.status.code(), Some(2));
    let o = run(kb().args(["link", "add", "1", "--blocks", "2", "--relates", "3"]));
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn tui_snapshot_renders_a_board_as_text() {
    migrate();
    let session = unique("tui");
    let workspace = unique("畫面");
    ok(kb().args([
        "--session",
        &session,
        "board",
        "init",
        "--title",
        "畫面快照",
        "--goal",
        "檢查 kb tui --snapshot",
        "--workspace",
        &workspace,
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb().args([
        "--session",
        &session,
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "一張很長很長很長很長很長很長的中文卡片標題",
        "--priority",
        "P0",
    ]));
    ok(kb().args([
        "--session",
        &session,
        "card",
        "block",
        "1",
        "--reason",
        "等待",
    ]));

    let out = ok(kb().args([
        "tui",
        "--snapshot",
        "--board",
        &session,
        "--width",
        "80",
        "--height",
        "20",
    ]));
    // Board screen (docs/board-v2.md section 5): header without a completion ratio, the
    // side panel (EPIC and the live feed), boxed cards with tags.
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 20, "{out}");
    assert!(lines[0].contains("› 畫面快照"), "{out}");
    assert!(lines[0].ends_with("● 已連線"), "{out}");
    assert!(!lines[0].contains("完成"), "{out}");
    assert!(
        lines[1].starts_with("┌ EPIC ─") && lines[1].contains("待辦 (1)"),
        "{out}"
    );
    assert!(lines[2].starts_with("│ （沒有 epic）"), "{out}");
    assert!(lines[3].contains("▶#1 一張很長"), "{out}");
    assert!(out.contains("阻塞"), "{out}");
    assert!(out.contains("#1 阻擋：等待"), "{out}");

    // --card renders that card's screen.
    let out = ok(kb().args([
        "tui",
        "--snapshot",
        "--board",
        &session,
        "--card",
        "1",
        "--width",
        "80",
        "--height",
        "20",
    ]));
    // The open layout (docs/board-v2.md section 6.0); 20 rows is a short screen, so no
    // top padding.
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("  #1 一張很長"), "{out}");
    assert_eq!(lines[1], "", "{out}");
    assert!(lines[2].starts_with("  待辦 · 衍生 · P0 · agent"), "{out}");
    assert!(lines[3].starts_with("  阻塞：等待"), "{out}");
    assert!(
        out.contains("\n  內容 ─")
            && out.contains("\n  連結 (0) ─")
            && out.contains("\n  紀錄 (0) ─"),
        "{out}"
    );
    assert!(!out.contains('┌') && !out.contains('│'), "no frame: {out}");
    let err = kb()
        .args(["tui", "--snapshot", "--board", &session, "--card", "99"])
        .output()
        .unwrap();
    assert!(!err.status.success());

    // --epic renders an epic screen; 「未歸類」 lists the cards without an epic.
    let out = ok(kb().args([
        "tui",
        "--snapshot",
        "--board",
        &session,
        "--epic",
        "未歸類",
        "--width",
        "80",
        "--height",
        "30",
    ]));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].starts_with("  未歸類"), "{out}");
    assert!(out.contains("\n  卡片 (1) ─"), "{out}");
    assert!(out.contains("\n  ▶ #1  待辦  一張很長"), "{out}");
    let err = kb()
        .args([
            "tui",
            "--snapshot",
            "--board",
            &session,
            "--epic",
            "沒有這個",
        ])
        .output()
        .unwrap();
    assert!(!err.status.success());

    // --workspace picks the same board; too small a window shows only the message.
    let out = ok(kb().args(["tui", "--snapshot", "--workspace", &workspace]));
    assert_eq!(out.lines().count(), 30);
    assert!(out.contains("畫面快照"));
    let out = ok(kb().args([
        "tui",
        "--snapshot",
        "--board",
        &session,
        "--width",
        "50",
        "--height",
        "10",
    ]));
    assert_eq!(out.trim(), "視窗太小");
}

#[test]
fn tui_errors_and_exit_codes() {
    migrate();
    // --width without --snapshot is a syntax error.
    let o = run(kb().args(["tui", "--width", "80"]));
    assert_eq!(o.status.code(), Some(2));
    let o = run(kb().args(["tui", "--snapshot", "--board", &unique("missing")]));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).starts_with("no board for session"),
        "{}",
        stderr(&o)
    );
    // The TUI connects only as the reader.
    let o = run(kb()
        .env(
            "KB_READER_URL",
            "postgres://kb_reader:x@127.0.0.1:1/kanban_test",
        )
        .args(["tui", "--snapshot"]));
    assert_eq!(o.status.code(), Some(3));
    assert!(
        stderr(&o).starts_with("database unreachable:"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn no_session_is_exit_1() {
    migrate();
    let o = run(kb().args(["card", "add", "--lane", "待辦", "--title", "x"]));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        "no session: set KB_SESSION or pass --session"
    );
}

/// docs/board-v2.md section 20.1: `kb board init --goal` without `--title`.
#[test]
fn board_init_without_title() {
    migrate();
    let s = unique("notitle");
    let ws = unique("ws");
    let kb_s = || {
        let mut c = kb();
        c.args(["--session", &s]);
        c
    };
    // No board yet: a data error (exit 1), not a syntax error (exit 2).
    let o = run(kb_s().args(["board", "init", "--goal", "目標", "--workspace", &ws]));
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        stderr(&o).trim_end(),
        "--title is required: the board has no title yet"
    );
    ok(kb_s().args([
        "board",
        "init",
        "--title",
        "原本的標題",
        "--goal",
        "原本的目標",
        "--workspace",
        &ws,
    ]));
    // Only the goal: the title is kept.
    let out = ok(kb_s().args(["board", "init", "--goal", "新的目標"]));
    assert!(out.starts_with("board updated"), "{out}");
    let board = ok(kb_s().args(["board", "show"]));
    assert!(board.contains("board: 原本的標題"), "{board}");
    assert!(board.contains("goal: 新的目標"), "{board}");
}

#[test]
fn unreachable_database_is_exit_3() {
    let o = run(kb()
        .env(
            "KB_WRITER_URL",
            "postgres://kb_writer:x@127.0.0.1:1/kanban_test",
        )
        .args(["--session", "s", "board", "show"]));
    assert_eq!(o.status.code(), Some(3));
    assert!(
        stderr(&o).starts_with("database unreachable:"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn missing_config_is_exit_3() {
    let empty_home = std::env::temp_dir().join(unique("home"));
    std::fs::create_dir_all(&empty_home).unwrap();
    let o = run(kb()
        .env_remove("KB_WRITER_URL")
        .env("USERPROFILE", &empty_home)
        .env("HOME", &empty_home)
        .args(["--session", "s", "board", "show"]));
    assert_eq!(o.status.code(), Some(3));
    assert!(
        stderr(&o).starts_with("no database config:"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn chinese_text_round_trips_through_args_stdin_and_json() {
    migrate();
    let session = unique("utf8");
    let workspace = unique("工作區");
    let out = ok(kb().args([
        "--session",
        &session,
        "board",
        "init",
        "--title",
        "修正登入逾時",
        "--workspace",
        &workspace,
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    assert!(
        out.contains(&format!("in workspace {workspace} (3 lanes)")),
        "{out}"
    );

    let out = ok(kb().args([
        "--session",
        &session,
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "補上逾時測試",
        "--priority",
        "P1",
    ]));
    // Every write ends with the next card (docs/board-v2.md section 10.2).
    assert_eq!(
        out.trim(),
        "card 1 added to 待辦\nnext: #1 補上逾時測試 (待辦)"
    );

    // Note text from stdin (UTF-8 with a BOM and CRLF, as a Windows editor might produce).
    let mut child = kb()
        .args([
            "--session",
            &session,
            "card",
            "note",
            "#1",
            "--kind",
            "decision",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("\u{feff}改用滑動視窗\r\n第二行\r\n".as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));

    let out = ok(kb().args(["--session", &session, "--json", "card", "show", "1"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["card"]["title"], "補上逾時測試");
    assert_eq!(v["lane"]["name"], "待辦");
    assert_eq!(v["notes"][0]["body"], "改用滑動視窗\n第二行");
    assert_eq!(v["notes"][0]["kind"], "decision");

    let out = ok(kb().args(["--session", &session, "board", "show"]));
    assert!(
        out.starts_with("board: 修正登入逾時 [active] rev 3\n"),
        "{out}"
    );
    assert!(
        out.contains("== 待辦 (todo) ==\n  [1] 補上逾時測試"),
        "{out}"
    );

    let out = ok(kb().args(["--session", &session, "--json", "board", "show"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["lanes"][0]["cards"][0]["note_count"], 1);
    assert_eq!(v["lanes"][0]["cards"][0]["link_count"], 0);

    // Invalid values are data errors (exit 1), not syntax errors.
    let o = run(kb().args([
        "--session",
        &session,
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "x",
        "--priority",
        "P7",
    ]));
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).starts_with("invalid priority: P7"));
    let o = run(kb().args(["--session", &session, "card", "move", "1", "--to", "不存在"]));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        "lane not found: 不存在 (lanes: 待辦, 實作, 完成)"
    );

    // Session from KB_SESSION; --session wins over it.
    let out = ok(kb()
        .env("KB_SESSION", &session)
        .args(["--json", "board", "show"]));
    assert!(out.contains("修正登入逾時"));
    let o = run(kb().env("KB_SESSION", &session).args([
        "--session",
        &unique("other"),
        "board",
        "show",
    ]));
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).starts_with("no board for session"));

    // Archived boards reject writes with the documented message.
    ok(kb().args(["--session", &session, "board", "archive"]));
    let o = run(kb().args(["--session", &session, "card", "note", "1", "x"]));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        "board is archived: run \"kb board reopen\" first"
    );
}

#[test]
fn the_next_line_skips_cards_under_discussion() {
    // docs/board-v2.md section 10.2: the head is the first card neither blocked nor
    // discussing; `kb card next` reports the same head.
    migrate();
    let s = unique("next-discuss");
    let kbs = || {
        let mut c = kb();
        c.args(["--session", &s]);
        c
    };
    ok(kbs().args([
        "board",
        "init",
        "--title",
        "討論",
        "--workspace",
        &unique("ws"),
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    let out = ok(kbs().args([
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "還在談",
        "--discuss",
    ]));
    assert_eq!(
        out,
        "card 1 added to 待辦\n\
         next: (no card ready in the todo lanes: all are blocked or under discussion)\n"
    );
    let out = ok(kbs().args(["card", "add", "--lane", "待辦", "--title", "可以做"]));
    assert_eq!(out, "card 2 added to 待辦\nnext: #2 可以做 (待辦)\n");
    // The discussing card moved to the front of the lane is still not next.
    ok(kbs().args(["card", "next", "2"]));
    let out = ok(kbs().args(["card", "next", "1"]));
    assert_eq!(
        out,
        "card 1 moved to the front of 待辦\nnext: #2 可以做 (待辦)\n"
    );
    let v: serde_json::Value =
        serde_json::from_str(&ok(kbs().args(["--json", "card", "note", "1", "談"]))).unwrap();
    assert_eq!(v["next"]["seq"], 2);
    // Once confirmed it is the head again.
    let out = ok(kbs().args(["card", "confirm", "1"]));
    assert!(out.ends_with("\nnext: #1 還在談 (待辦)\n"), "{out}");
}

#[test]
fn the_next_line_is_printed_when_it_changes() {
    // docs/board-v2.md sections 10.2 and 15.2.
    migrate();
    let s = unique("next");
    let kbs = || {
        let mut c = kb();
        c.args(["--session", &s]);
        c
    };
    let rev = || -> i64 {
        let v: serde_json::Value =
            serde_json::from_str(&ok(kbs().args(["--json", "board", "show"]))).unwrap();
        v["board"]["rev"].as_i64().unwrap()
    };
    // The first write of the session always prints it.
    let out = ok(kbs().args([
        "board",
        "init",
        "--title",
        "順序",
        "--workspace",
        &unique("ws"),
        "--lanes",
        "研究:todo,待辦:todo,實作:doing,完成:done",
    ]));
    assert!(
        out.ends_with("next: (no card in the todo lanes)\n"),
        "{out}"
    );
    // The leftmost todo lane with cards: 研究 is empty, so 待辦.
    let out = ok(kbs().args(["card", "add", "--lane", "待辦", "--title", "第一張"]));
    assert_eq!(out, "card 1 added to 待辦\nnext: #1 第一張 (待辦)\n");
    // Unchanged: no line.
    let out = ok(kbs().args(["card", "add", "--lane", "待辦", "--title", "第二張"]));
    assert_eq!(out, "card 2 added to 待辦\n");
    let out = ok(kbs().args(["card", "add", "--lane", "研究", "--title", "先查清楚"]));
    assert!(out.ends_with("next: #3 先查清楚 (研究)\n"), "{out}");
    // --next puts a card first, but 研究 still comes first: unchanged.
    let out = ok(kbs().args(["card", "add", "--lane", "待辦", "--title", "插隊", "--next"]));
    assert!(!out.contains("next:"), "{out}");
    let out = ok(kbs().args(["card", "move", "3", "--to", "實作"]));
    assert!(out.ends_with("next: #4 插隊 (待辦)\n"), "{out}");
    let show = ok(kbs().args(["board", "show"]));
    let todo: Vec<&str> = show.lines().filter(|l| l.starts_with("  [")).collect();
    assert!(
        todo[0].starts_with("  [4] ") && todo[1].starts_with("  [1] "),
        "{show}"
    );
    let out = ok(kbs().args(["card", "next", "2"]));
    assert_eq!(
        out,
        "card 2 moved to the front of 待辦\nnext: #2 第二張 (待辦)\n"
    );
    // `kb card next` always prints it, even when it did not change.
    let out = ok(kbs().args(["card", "next", "2"]));
    assert!(out.ends_with("\nnext: #2 第二張 (待辦)\n"), "{out}");
    let out = ok(kbs().args(["card", "order", "1", "#4"]));
    assert_eq!(
        out,
        "cards 1, 4 moved to the front of 待辦\nnext: #1 第一張 (待辦)\n"
    );
    // No change: success, nothing written, and no line.
    let before = ok(kbs().args(["activity", "--limit", "1"]));
    let out = ok(kbs().args(["card", "order", "1", "4", "2"]));
    assert_eq!(out, "order of 待辦 unchanged\n");
    assert_eq!(ok(kbs().args(["activity", "--limit", "1"])), before);
    // Different lanes: exit 1 with the message.
    let o = kbs().args(["card", "order", "1", "3"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o).trim(), "cards are not in the same lane");
    // move --next.
    let out = ok(kbs().args(["card", "move", "3", "--to", "待辦", "--next"]));
    assert!(out.ends_with("next: #3 先查清楚 (待辦)\n"), "{out}");
    // Every todo card blocked: no card is ready (docs/board-v2.md section 10.2).
    for n in ["1", "2", "3"] {
        ok(kbs().args(["card", "block", n, "--reason", "等待"]));
    }
    let out = ok(kbs().args(["card", "block", "4", "--reason", "等待"]));
    assert!(
        out.ends_with(
            "next: (no card ready in the todo lanes: all are blocked or under discussion)\n"
        ),
        "{out}"
    );
    let out = ok(kbs().args(["card", "note", "3", "--kind", "blocker", "等待確認"]));
    assert!(!out.contains("next:"), "{out}");
    // A blocked card sorts after the unblocked ones.
    let out = ok(kbs().args(["card", "unblock", "2"]));
    assert!(out.ends_with("next: #2 第二張 (待辦)\n"), "{out}");

    // --json: always a `next` field instead of the line; it is remembered too.
    let out = ok(kbs().args(["--json", "card", "unblock", "4"]));
    assert!(!out.contains("next:"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    // #4 sits before #2 by position; both are unblocked now.
    assert_eq!(v["next"]["seq"], 4);
    assert_eq!(v["next"]["title"], "插隊");
    assert_eq!(v["next"]["lane"], "待辦");
    assert_eq!(v["next"]["blocked"], false);
    let out = ok(kbs().args(["--json", "card", "note", "4", "沒有改變"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["next"]["seq"], 4, "{out}");
    let out = ok(kbs().args(["card", "note", "4", "仍然沒有改變"]));
    assert!(!out.contains("next:"), "{out}");
    // Remembering the line does not count as a change: rev is the write's rev.
    let v: serde_json::Value =
        serde_json::from_str(&ok(kbs().args(["--json", "card", "note", "4", "rev"]))).unwrap();
    assert_eq!(rev(), v["rev"].as_i64().unwrap());

    // `kb board start` shows the board without the line: the next write prints it again.
    ok(kbs().args(["board", "start"]));
    let out = ok(kbs().args(["card", "note", "4", "重新開始"]));
    assert!(out.ends_with("\nnext: #4 插隊 (待辦)\n"), "{out}");

    // Read commands never print it.
    for args in [
        &["board", "show"][..],
        &["card", "show", "2"][..],
        &["inbox", "--peek"][..],
        &["activity"][..],
        &["question", "list"][..],
        &["epic", "list"][..],
        &["board", "list"][..],
    ] {
        let out = ok(kbs().args(args));
        assert!(!out.contains("next:"), "{args:?}: {out}");
    }

    // An empty todo: the empty form, printed once.
    let s2 = unique("next-empty");
    let out = ok(kb().args([
        "--session",
        &s2,
        "board",
        "init",
        "--title",
        "空",
        "--workspace",
        &unique("ws"),
    ]));
    assert!(
        out.ends_with("next: (no card in the todo lanes)\n"),
        "{out}"
    );
    let out = ok(kb().args([
        "--session",
        &s2,
        "card",
        "add",
        "--lane",
        "進行中",
        "--title",
        "做中",
    ]));
    assert_eq!(out, "card 1 added to 進行中\n");
}

#[test]
fn board_show_lists_finished_lanes_only_with_all() {
    // docs/board-v2.md section 15.1.
    migrate();
    let s = unique("show-all");
    let kbs = || {
        let mut c = kb();
        c.args(["--session", &s]);
        c
    };
    ok(kbs().args([
        "board",
        "init",
        "--title",
        "精簡",
        "--workspace",
        &unique("ws"),
        "--lanes",
        "待辦:todo,實作:doing,完成:done,捨棄:dropped",
    ]));
    for title in ["一", "二", "三", "四"] {
        ok(kbs().args(["card", "add", "--lane", "待辦", "--title", title]));
    }
    ok(kbs().args(["card", "move", "1", "--to", "實作"]));
    ok(kbs().args(["card", "done", "2", "--result", "好了"]));
    ok(kbs().args(["card", "done", "3", "--result", "也好了"]));
    ok(kbs().args(["card", "drop", "4", "--reason", "不需要"]));

    let show = ok(kbs().args(["board", "show"]));
    assert!(
        show.ends_with(
            "\n== 待辦 (todo) ==\n== 實作 (doing) ==\n  [1] 一 (derived)\n\
             == 完成 (done) == 2 cards\n== 捨棄 (dropped) == 1 card\n"
        ),
        "{show}"
    );
    // Finished lanes list the card that entered last first (docs/board-v2.md 16.3).
    let all = ok(kbs().args(["board", "show", "--all"]));
    assert!(
        all.ends_with(
            "\n== 完成 (done) ==\n  [3] 三 (derived)\n  [2] 二 (derived)\n\
             == 捨棄 (dropped) ==\n  [4] 四 (derived)\n"
        ),
        "{all}"
    );
    // --json is complete with or without --all.
    let v: serde_json::Value =
        serde_json::from_str(&ok(kbs().args(["--json", "board", "show"]))).unwrap();
    assert_eq!(v["lanes"][2]["cards"].as_array().unwrap().len(), 2);
    assert_eq!(v["lanes"][3]["cards"].as_array().unwrap().len(), 1);
    // The same order as the text.
    assert_eq!(v["lanes"][2]["cards"][0]["seq"], 3);
    assert_eq!(v["lanes"][2]["cards"][1]["seq"], 2);
}

/// A home directory whose `~/.config/kb/config.toml` still has an `admin_url`. Every URL
/// points at port 1 of the loopback address and at `kanban_test`: nothing can connect.
fn home_with_admin_url() -> std::path::PathBuf {
    let home = std::env::temp_dir().join(unique("home-admin"));
    let dir = home.join(".config").join("kb");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[database]\n\
         writer_url = \"postgres://kb_writer:x@127.0.0.1:1/kanban_test\"\n\
         reader_url = \"postgres://kb_reader:x@127.0.0.1:1/kanban_test\"\n\
         admin_url  = \"postgres://kb_admin:x@127.0.0.1:1/kanban_test\"\n",
    )
    .unwrap();
    home
}

const ADMIN_WARNING: &str =
    "warning: admin_url in config.toml is ignored; set KB_ADMIN_URL for kb db commands";
const NO_ADMIN: &str = "no admin connection: set KB_ADMIN_URL for kb db commands \
     (admin_url in config.toml is not read)";

#[test]
fn the_admin_url_comes_only_from_the_environment() {
    // docs/board-v2.md section 15.4. Nothing here connects to a database.
    let home = home_with_admin_url();
    let from_file = || {
        let mut c = kb();
        c.env_remove("KB_WRITER_URL")
            .env_remove("KB_READER_URL")
            .env_remove("KB_ADMIN_URL")
            .env("USERPROFILE", &home)
            .env("HOME", &home);
        c
    };
    // The file's admin_url is ignored with a warning; without KB_ADMIN_URL: exit 1.
    let o = run(from_file().args(["db", "migrate", "--database", "kanban_test", "--yes"]));
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stderr(&o).trim(), format!("{ADMIN_WARNING}\n{NO_ADMIN}"));
    assert!(o.stdout.is_empty());
    // Every kb db command prints the warning (here: the file's unreachable writer URL).
    let o = run(from_file().args(["db", "status"]));
    let err = stderr(&o);
    assert_eq!(err.lines().next(), Some(ADMIN_WARNING), "{err}");
    // Other commands run without the warning: agents cannot fix the file.
    for args in [&["board", "list"][..], &["--session", "s", "board", "show"]] {
        let o = run(from_file().args(args));
        assert_eq!(o.status.code(), Some(3), "{args:?}");
        let err = stderr(&o);
        assert!(!err.contains("admin_url"), "{args:?}: {err}");
        assert!(err.starts_with("database unreachable:"), "{args:?}: {err}");
    }
    // Writer and reader from the environment: the file is not read, so no warning.
    let o = run(kb()
        .env_remove("KB_ADMIN_URL")
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .args(["db", "migrate", "--database", "kanban_test", "--yes"]));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o).trim(), NO_ADMIN);
    // Hooks never print the warning (kb hook wait's stderr wakes the agent).
    let mut child = from_file()
        .args(["hook", "stop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"none"}"#)
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(!stderr(&o).contains("admin_url"), "{}", stderr(&o));
}

#[test]
fn migrate_without_yes_is_unchanged_when_nothing_is_pending() {
    // docs/board-v2.md section 15.5: kanban_test has boards, but nothing is pending.
    migrate();
    let out = ok(kb().args(["db", "migrate", "--database", "kanban_test"]));
    assert!(
        out.starts_with("database kanban_test is up to date (schema version "),
        "{out}"
    );
}

/// docs/board-v2.md sections 19.1, 19.3 and 19.4, end to end.
#[test]
fn text_values_may_start_with_a_hyphen() {
    migrate();
    let s = unique("hyphen");
    let ws = unique("ws");
    let kb_s = || {
        let mut c = kb();
        c.args(["--session", &s]);
        c
    };
    ok(kb_s().args([
        "board",
        "init",
        "--title",
        "-看板",
        "--goal",
        "--na-values 的修正",
        "--workspace",
        &ws,
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    // The command that failed in the experiment, and the "=" form.
    ok(kb_s().args([
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "--na-values 改為空字串",
    ]));
    ok(kb_s().args(["card", "add", "--lane", "待辦", "--title=--第二張"]));
    ok(kb_s().args([
        "card",
        "note",
        "1",
        "--kind",
        "decision",
        "--na-values 只接受一個值",
        "--because",
        "user",
    ]));
    // A text that is exactly an option name: "--" before it.
    let o = run(kb_s().args(["card", "note", "1", "--kind"]));
    assert_eq!(o.status.code(), Some(2));
    ok(kb_s().args(["card", "note", "1", "--", "--kind"]));
    ok(kb_s().args(["card", "block", "1", "--reason", "-等待確認"]));
    let show = ok(kb_s().args(["card", "show", "1"]));
    assert!(show.contains("card 1: --na-values 改為空字串"), "{show}");
    assert!(show.contains("-等待確認"), "{show}");
    // kb card show keeps the oldest-first order (only the TUI reverses it).
    let first = show.find("--na-values 只接受一個值").expect(&show);
    let second = show.find("progress  --kind").expect(&show);
    assert!(first < second, "{show}");
    let board = ok(kb_s().args(["board", "show"]));
    assert!(board.contains("--第二張"), "{board}");

    // R11's immediate echo: done on a card with an open question.
    let v: serde_json::Value = serde_json::from_str(&ok(kb_s().args([
        "--json",
        "ask",
        "--card",
        "2",
        "-要不要保留？",
        "--option",
        "-保留",
        "--option",
        "--移除",
    ])))
    .unwrap();
    let q = v["question"].as_i64().unwrap();
    let o = run(kb_s().args(["card", "done", "2", "--result", "--已完成"]));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stderr(&o).trim_end(),
        format!(
            "warning: R11: card 2 is finished but question {q} is still open; if it was \
             answered in the session, run: kb question cancel {q} --reason \"answered in the \
             session\""
        )
    );
    // One line, with the same command as R11's fix (docs/board-v2.md section 20.3).
    assert_eq!(stderr(&o).trim_end().lines().count(), 1);
    let shown = ok(kb_s().args(["question", "show", &q.to_string()]));
    assert!(
        shown.contains("-要不要保留？") && shown.contains("--移除"),
        "{shown}"
    );
    ok(kb_s().args([
        "question",
        "cancel",
        &q.to_string(),
        "--reason",
        "-在對話裡回答了",
    ]));
    let o = run(kb_s().args(["card", "done", "2", "--result=--已完成"]));
    assert_eq!(o.status.code(), Some(0));
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));
}
