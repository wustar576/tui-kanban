//! `--because` through the `kb` binary (docs/board-v2.md section 18): `kb card show` text
//! and JSON, the hint, and the errors. Database commands run against `kanban_test` only.

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
    format!("test-because-{prefix}-{}-{nanos}", std::process::id())
}

fn kb_s(session: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_WRITER_URL", test_url(Role::Writer))
        .env("KB_READER_URL", test_url(Role::Reader))
        .env("KB_ADMIN_URL", test_url(Role::Admin))
        .env("KB_SESSION", session)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("KB_TERMINAL_ID")
        .env_remove("KB_TERMINAL_SPACE")
        .env_remove("KB_TERMINAL_TAB")
        .env_remove("KB_WORKSPACE_NAME");
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

fn json(cmd: &mut Command) -> Value {
    serde_json::from_str(&ok(cmd)).expect("JSON output")
}

/// A board with cards 1 「統一提問畫面」, 2 「舊的提問畫面」, 3 「留言的卡片」.
fn board() -> String {
    let s = unique("b");
    ok(kb_s(&s).args(["db", "migrate", "--database", "kanban_test", "--yes"]));
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "依據",
        "--workspace",
        &unique("ws"),
        "--lanes",
        "待辦:todo,實作:doing,完成:done,捨棄:dropped",
    ]));
    for title in ["統一提問畫面", "舊的提問畫面", "留言的卡片"] {
        ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", title]));
    }
    s
}

fn ask(s: &str, card: &str) -> i64 {
    json(kb_s(s).args([
        "--json",
        "ask",
        "--card",
        card,
        "選哪個？",
        "--option",
        "甲",
        "--option",
        "乙",
    ]))["question"]
        .as_i64()
        .unwrap()
}

/// Adds a comment as kb_reader through kb-core (what the TUI does); returns the note id.
fn comment(s: &str, card: &str, text: &str) -> i64 {
    let card_id = json(kb_s(s).args(["--json", "card", "show", card]))["card"]["id"]
        .as_i64()
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut reader = kb_core::db::connect(&test_url(Role::Reader)).await.unwrap();
        kb_core::comment::add_comment(&mut reader, card_id, text)
            .await
            .unwrap()
    })
}

const HINT: &str = "hint: if this follows from a card, a question, a comment or the user, \
     name it with --because <card> | q<id> | c<id> | user. Your own choice needs no --because.";

#[test]
fn card_show_prints_causes_and_what_a_card_led_to() {
    let s = board();
    let q = ask(&s, "1");
    let cm = comment(&s, "3", "別做舊畫面");
    let (q, cm) = (q.to_string(), cm.to_string());

    let out = ok(kb_s(&s).args([
        "card",
        "note",
        "2",
        "--kind",
        "decision",
        "先不做舊畫面\n第二行",
        "--because",
        &format!("q{q}"),
        "--because",
        &format!("c{cm}"),
        "--because",
        "user",
    ]));
    assert!(!out.contains("hint:"), "{out}");
    let out = ok(kb_s(&s).args([
        "card",
        "drop",
        "2",
        "--reason",
        "改由 #1 一起處理\n細節",
        "--because",
        "#1",
    ]));
    assert!(out.starts_with("card 2 dropped to 捨棄"), "{out}");
    assert!(!out.contains("hint:"), "{out}");

    // Forward: under the result and under the note, aligned with their text.
    let show = ok(kb_s(&s).args(["card", "show", "2"]));
    assert!(
        show.contains(
            "\nresult: 改由 #1 一起處理\n        細節\n        because: #1 \"統一提問畫面\"\n"
        ),
        "{show}"
    );
    let pad = " ".repeat(30);
    assert!(
        show.contains(&format!(
            "  decision  先不做舊畫面\n{pad}第二行\n\
             {pad}because: question {q} (card #1)\n\
             {pad}because: comment {cm} (card #3)\n\
             {pad}because: the user's instruction\n"
        )),
        "{show}"
    );
    assert!(!show.contains("led to"), "{show}");

    // Reverse: on #1 (the card and its question) and on #3 (its comment).
    let show1 = ok(kb_s(&s).args(["card", "show", "1"]));
    assert!(
        show1.ends_with(&format!(
            "\n\nled to (2):\n  #2 decision — 先不做舊畫面  (via question {q})\n  #2 dropped — 改由 #1 一起處理\n"
        )),
        "{show1}"
    );
    let show3 = ok(kb_s(&s).args(["card", "show", "3"]));
    assert!(
        show3.ends_with(&format!(
            "\n\nled to (1):\n  #2 decision — 先不做舊畫面  (via comment {cm})\n"
        )),
        "{show3}"
    );

    // JSON carries both directions.
    let v = json(kb_s(&s).args(["--json", "card", "show", "2"]));
    assert_eq!(v["result_because"][0]["kind"], "card");
    assert_eq!(v["result_because"][0]["card_seq"], 1);
    assert_eq!(v["notes"][0]["because"].as_array().unwrap().len(), 3);
    assert_eq!(v["notes"][0]["because"][2]["kind"], "user");
    assert_eq!(v["led_to"], serde_json::json!([]));
    let v = json(kb_s(&s).args(["--json", "card", "show", "1"]));
    assert_eq!(v["led_to"][1]["decision"], "drop");
    assert_eq!(v["led_to"][1]["text"], "改由 #1 一起處理\n細節");
    assert_eq!(v["result_because"], serde_json::json!([]));

    // The activity of the write includes the causes.
    let acts = json(kb_s(&s).args(["--json", "activity", "--limit", "1"]));
    assert_eq!(
        acts[0]["detail"]["because"],
        serde_json::json!([{"kind": "card", "seq": 1}])
    );
}

#[test]
fn the_hint_comes_last_and_never_changes_the_exit_code() {
    let s = board();
    // A decision note without --because: the hint is the last line.
    let o = kb_s(&s)
        .args(["card", "note", "1", "--kind", "decision", "改用方案 B"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let out = stdout(&o);
    assert!(
        out.starts_with("note (decision) added to card 1\n"),
        "{out}"
    );
    assert!(out.ends_with(&format!("\n{HINT}\n")), "{out}");
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));

    // Other kinds and done: no hint.
    let out = ok(kb_s(&s).args(["card", "note", "1", "--kind", "progress", "x"]));
    assert!(!out.contains("hint:"), "{out}");
    let out = ok(kb_s(&s).args(["card", "done", "1", "--result", "完成"]));
    assert!(!out.contains("hint:"), "{out}");
    // `user` alone is a cause: no hint.
    let out = ok(kb_s(&s).args([
        "card",
        "note",
        "1",
        "--kind",
        "decision",
        "照做",
        "--because",
        "user",
    ]));
    assert!(!out.contains("hint:"), "{out}");
    // A drop without --because.
    let out = ok(kb_s(&s).args(["card", "drop", "2", "--reason", "不需要"]));
    assert!(out.ends_with(&format!("\n{HINT}\n")), "{out}");

    // --json: a `hint` field, nothing else on stdout.
    let v = json(kb_s(&s).args(["--json", "card", "drop", "3", "--reason", "不需要"]));
    assert_eq!(
        v["hint"],
        "if this follows from a card, a question, a comment or the user, \
         name it with --because <card> | q<id> | c<id> | user. Your own choice needs no --because."
    );
    let v = json(kb_s(&s).args([
        "--json",
        "card",
        "note",
        "1",
        "--kind",
        "decision",
        "x",
        "--because",
        "2",
    ]));
    assert!(v.get("hint").is_none(), "{v}");
}

#[test]
fn bad_refs_exit_1_with_a_clear_message_and_write_nothing() {
    let s = board();
    let rev = |s: &str| json(kb_s(s).args(["--json", "board", "show"]))["board"]["rev"].clone();
    let before = rev(&s);
    for (args, message) in [
        (
            vec![
                "card",
                "note",
                "2",
                "--kind",
                "decision",
                "x",
                "--because",
                "abc",
            ],
            "invalid --because: abc (expected a card number like 30 or #30, q<question id>, c<comment id> or user)",
        ),
        (
            vec!["card", "drop", "2", "--reason", "x", "--because", "99"],
            "--because 99: card 99 not found on this board",
        ),
        (
            vec!["card", "done", "2", "--result", "x", "--because", "#2"],
            "--because 2: a decision cannot follow from its own card; name a question (q<id>) or a comment (c<id>) on it instead",
        ),
        (
            vec!["card", "note", "2", "x", "--because", "q999999999"],
            "--because q999999999: question 999999999 not found on this board",
        ),
        (
            vec![
                "card",
                "note",
                "2",
                "x",
                "--because",
                "user",
                "--because",
                "c999999999",
            ],
            "--because c999999999: comment 999999999 not found on this board",
        ),
    ] {
        let o = kb_s(&s).args(&args).output().unwrap();
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert_eq!(stderr(&o).trim(), message, "{args:?}");
        assert!(stdout(&o).is_empty(), "{args:?}");
    }
    assert_eq!(rev(&s), before, "nothing written");
}
