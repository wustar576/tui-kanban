//! End-to-end tests of the board v2 commands (docs/board-v2.md section 4): `kb epic`,
//! card origin and epic options, and the `kb board show` additions. Database: `kanban_test`.

use std::process::{Command, Output};

use kb_core::{Config, Role};
use serde_json::Value;

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
    format!("test-epic-{prefix}-{}-{nanos}", std::process::id())
}

fn kb(session: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_WRITER_URL", test_url(Role::Writer))
        .env("KB_READER_URL", test_url(Role::Reader))
        .env("KB_ADMIN_URL", test_url(Role::Admin))
        .env_remove("KB_SESSION")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("HC_TERMINAL_ID")
        .args(["--session", session]);
    cmd
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).expect("stderr is UTF-8")
}

fn ok(cmd: &mut Command) -> String {
    let o = cmd.output().expect("run kb");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    stdout(&o)
}

/// The message of a write command: its output without the `next:` line, which a write
/// prints only when the next card changed (docs/board-v2.md section 15.2).
fn message(out: &str) -> String {
    let out = out.trim_end();
    match out.rsplit_once('\n') {
        Some((msg, next)) if next.starts_with("next: ") => msg.to_string(),
        _ => {
            assert!(!out.contains("next: "), "{out}");
            out.to_string()
        }
    }
}

/// Exit code and trimmed stderr.
fn fail(cmd: &mut Command) -> (Option<i32>, String) {
    let o = cmd.output().expect("run kb");
    (o.status.code(), stderr(&o).trim().to_string())
}

fn migrate() {
    ok(kb("none").args(["db", "migrate", "--database", "kanban_test", "--yes"]));
}

fn board(session: &str) {
    ok(kb(session).args([
        "board",
        "init",
        "--title",
        "改版",
        "--workspace",
        &unique("ws"),
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
        "--epics",
        "P1 只讀畫面,P2",
    ]));
}

#[test]
fn epic_commands_and_board_show() {
    migrate();
    let s = unique("show");
    board(&s);
    assert_eq!(
        message(&ok(kb(&s).args(["epic", "add", "P3 總覽"]))),
        "epic P3 總覽 added at position 3"
    );
    ok(kb(&s).args([
        "card",
        "add",
        "--lane",
        "完成",
        "--title",
        "完成的",
        "--epic",
        "P1 只讀畫面",
        "--agreed",
    ]));
    ok(kb(&s).args([
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "討論中",
        "--epic",
        "P2",
        "--discuss",
    ]));
    ok(kb(&s).args([
        "card", "add", "--lane", "待辦", "--title", "延伸", "--from", "2",
    ]));
    ok(kb(&s).args(["card", "add", "--lane", "待辦", "--title", "沒有 epic"]));

    let show = ok(kb(&s).args(["board", "show"]));
    assert!(
        show.contains(
            "epic: P1 只讀畫面  1/1  (+0 derived, 0 discussing)\n\
             epic: P2  0/2  (+1 derived, 1 discussing)\n\
             epic: P3 總覽  0/0  (+0 derived, 0 discussing)\n"
        ),
        "{show}"
    );
    assert!(
        show.contains("\n  [2] 討論中 {P2} (discussing)"),
        "{show}"
    );
    assert!(show.contains("\n  [3] 延伸 {P2} (derived)"), "{show}");
    assert!(show.contains("\n  [4] 沒有 epic (derived)"), "{show}");
    // Finished lanes: only a count unless --all (docs/board-v2.md section 15.1).
    assert!(show.ends_with("\n== 完成 (done) == 1 card\n"), "{show}");
    assert!(!show.contains("完成的"), "{show}");
    let all = ok(kb(&s).args(["board", "show", "--all"]));
    assert!(
        all.ends_with("\n== 完成 (done) ==\n  [1] 完成的 {P1 只讀畫面}\n"),
        "{all}"
    );

    let v: Value = serde_json::from_str(&ok(kb(&s).args(["--json", "board", "show"]))).unwrap();
    assert_eq!(v["discussing_count"], 1);
    assert_eq!(v["epics"][1]["name"], "P2");
    assert_eq!(v["epics"][1]["total"], 2);
    assert_eq!(v["no_epic"]["total"], 1);
    let todo = &v["lanes"][0]["cards"];
    assert_eq!(todo[0]["origin"], "discussing");
    assert_eq!(todo[1]["epic_name"], "P2");
    assert_eq!(todo[1]["spawned_from"], 2);
    assert_eq!(todo[0]["spawned_count"], 1);

    // epic list, text and JSON; no summary any more (docs/board-v2.md section 10.1).
    assert_eq!(
        ok(kb(&s).args(["epic", "list"])).trim(),
        "P1 只讀畫面  1/1  +0 derived  0 discussing\n\
         P2  0/2  +1 derived  1 discussing\n\
         P3 總覽  0/0  +0 derived  0 discussing"
    );
    let v: Value = serde_json::from_str(&ok(kb(&s).args(["--json", "epic", "list"]))).unwrap();
    assert!(v[0].get("summary").is_none(), "{v}");
    assert_eq!(v[0]["done"], 1);

    // card show: epic and origin.
    let card = ok(kb(&s).args(["card", "show", "2"]));
    assert!(card.contains("\nepic: P2\norigin: discussing\n"), "{card}");

    // rename, move, remove.
    ok(kb(&s).args(["epic", "rename", "P3 總覽", "P3"]));
    ok(kb(&s).args(["epic", "move", "P3", "--position", "1"]));
    assert!(ok(kb(&s).args(["epic", "list"])).starts_with("P3  0/0"));
    assert_eq!(
        message(&ok(kb(&s).args(["epic", "remove", "P3"]))),
        "epic P3 removed"
    );
}

#[test]
fn origin_commands_and_warnings() {
    migrate();
    let s = unique("origin");
    board(&s);
    ok(kb(&s).args([
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "討論",
        "--discuss",
    ]));
    ok(kb(&s).args(["card", "add", "--lane", "待辦", "--title", "衍生"]));

    // Moving a discussing card into a doing lane succeeds with a warning.
    let o = kb(&s)
        .args(["card", "move", "1", "--to", "實作"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(message(&stdout(&o)), "card 1 moved to 實作");
    assert_eq!(
        stderr(&o).trim(),
        "warning: R7: card 1 is still under discussion; run \"kb card confirm 1\" once the user \
         agreed, or move it back"
    );

    assert_eq!(
        message(&ok(kb(&s).args(["card", "confirm", "1", "#2"]))),
        "card 1: discussing -> agreed\ncard 2: derived -> agreed"
    );
    assert_eq!(
        message(&ok(kb(&s).args(["card", "confirm", "2"]))),
        "card 2 is already agreed"
    );
    assert_eq!(
        message(&ok(kb(&s).args(["card", "discuss", "2"]))),
        "card 2: agreed -> discussing"
    );

    // Epic of a card.
    assert_eq!(
        message(&ok(kb(&s).args(["card", "edit", "1", "--epic", "P2"]))),
        "card 1 edited (epic P2)"
    );
    assert_eq!(
        message(&ok(kb(&s).args(["card", "edit", "1", "--no-epic"]))),
        "card 1 edited (no epic)"
    );
}

#[test]
fn errors_and_exit_codes() {
    migrate();
    let s = unique("errors");
    board(&s);
    ok(kb(&s).args([
        "card", "add", "--lane", "待辦", "--title", "卡片", "--epic", "P2",
    ]));

    // Data errors: exit 1.
    assert_eq!(
        fail(kb(&s).args(["epic", "add", "P2"])),
        (Some(1), "epic already exists: P2".into())
    );
    assert_eq!(
        fail(kb(&s).args(["epic", "remove", "P2"])),
        (Some(1), "epic is not empty".into())
    );
    // `kb epic summary` was removed (docs/board-v2.md section 10.1).
    let (code, err) = fail(kb(&s).args(["epic", "summary", "P2", "x"]));
    assert_eq!(code, Some(2));
    assert!(err.contains("unrecognized subcommand 'summary'"), "{err}");
    assert_eq!(
        fail(kb(&s).args(["epic", "move", "P2", "--position", "9"])),
        (Some(1), "invalid position: 9 (board has 2 epics)".into())
    );
    assert_eq!(
        fail(kb(&s).args([
            "card", "add", "--lane", "待辦", "--title", "x", "--epic", "無"
        ])),
        (
            Some(1),
            "epic not found: 無 (epics: P1 只讀畫面, P2)".into()
        )
    );
    assert_eq!(
        fail(kb(&s).args(["card", "confirm", "9"])),
        (Some(1), "card not found: 9".into())
    );
    assert_eq!(
        fail(kb(&s).args(["board", "init", "--title", "t", "--epics", "a,a"])),
        (Some(1), "duplicate epic name: a".into())
    );

    // Usage errors: exit 2.
    for args in [
        &[
            "card",
            "add",
            "--lane",
            "待辦",
            "--title",
            "x",
            "--discuss",
            "--agreed",
        ][..],
        &["card", "edit", "1", "--epic", "P2", "--no-epic"][..],
        &["card", "confirm"][..],
        &["card", "discuss"][..],
        &["epic", "move", "P2"][..],
        &["epic", "summary", "P2"][..],
        &["epic", "frobnicate"][..],
    ] {
        assert_eq!(fail(kb(&s).args(args)).0, Some(2), "{args:?}");
    }
}
