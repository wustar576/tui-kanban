//! End-to-end tests of `kb hook <event>` (JSON piped on stdin), the rule commands and the
//! question commands. Database: `kanban_test` only.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use kb_core::{Config, Role};
use serde_json::{Value, json};

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
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "test-hook-{prefix}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// A rules file in the temp directory; `None` content means "does not exist".
fn rules_file(content: Option<&str>) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{}.toml", unique("rules")));
    if let Some(c) = content {
        std::fs::write(&path, c).unwrap();
    }
    path
}

/// `kb` against kanban_test, default rule levels, no inherited session variables.
fn kb() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_WRITER_URL", test_url(Role::Writer))
        .env("KB_READER_URL", test_url(Role::Reader))
        .env("KB_ADMIN_URL", test_url(Role::Admin))
        .env("KB_RULES_FILE", rules_file(None))
        .env_remove("KB_SESSION")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("KB_TERMINAL_ID")
        .env_remove("KB_TERMINAL_SPACE")
        .env_remove("KB_TERMINAL_TAB")
        .env_remove("KB_WORKSPACE_NAME")
        .env_remove("CLAUDE_ENV_FILE");
    cmd
}

fn run_with_stdin(cmd: &mut Command, input: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kb");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn hook_with(cmd: &mut Command, event: &str, input: &Value) -> Output {
    run_with_stdin(cmd.args(["hook", event]), &input.to_string())
}

fn hook(event: &str, input: &Value) -> Output {
    hook_with(&mut kb(), event, input)
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

fn kb_s(session: &str) -> Command {
    let mut c = kb();
    c.args(["--session", session]);
    c
}

fn migrate() {
    ok(kb().args(["db", "migrate", "--database", "kanban_test", "--yes"]));
}

/// Exit 0 and ASCII-only JSON on stdout.
fn json_out(o: &Output) -> Value {
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(o));
    assert!(
        o.stdout.is_ascii(),
        "hook output must be ASCII: {}",
        stdout(o)
    );
    // A hook that hits its connection time limit prints nothing on stdout and says why on
    // stderr; show stderr so an intermittent failure names its cause.
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}); stdout: {:?}; stderr: {}",
            stdout(o),
            stderr(o)
        )
    })
}

fn context(o: &Output, event: &str) -> String {
    let v = json_out(o);
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], event);
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

fn silent(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(o));
    assert!(o.stdout.is_empty(), "unexpected stdout: {}", stdout(o));
}

fn board_json(session: &str) -> Value {
    serde_json::from_str(&ok(kb_s(session).args(["--json", "board", "show"]))).unwrap()
}

fn activity(session: &str) -> Vec<Value> {
    let v: Value = serde_json::from_str(&ok(
        kb_s(session).args(["--json", "activity", "--limit", "50"])
    ))
    .unwrap();
    v.as_array().unwrap().clone()
}

fn verbs(session: &str) -> Vec<String> {
    activity(session)
        .iter()
        .map(|a| a["verb"].as_str().unwrap().to_string())
        .collect()
}

/// Answers a question as kb_reader through kb-core (what the TUI does).
fn answer(question: i64, option_ids: &[i64], chat: bool) {
    answer_with_text(question, option_ids, chat, None);
}

/// [`answer`] with typed text (docs/board-v2.md section 7.1).
fn answer_with_text(question: i64, option_ids: &[i64], chat: bool, text: Option<&str>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut reader = kb_core::db::connect(&test_url(Role::Reader)).await.unwrap();
        kb_core::answer::answer_question(&mut reader, question, option_ids, chat, text)
            .await
            .unwrap();
    });
}

fn option_ids(session: &str, question: i64) -> Vec<i64> {
    let v: Value = serde_json::from_str(&ok(kb_s(session).args([
        "--json",
        "question",
        "show",
        &question.to_string(),
    ])))
    .unwrap();
    v["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_i64().unwrap())
        .collect()
}

fn ask(session: &str, card: &str, text: &str, options: &[&str]) -> i64 {
    let mut cmd = kb_s(session);
    cmd.args(["--json", "ask", "--card", card, text]);
    for o in options {
        cmd.args(["--option", o]);
    }
    let v: Value = serde_json::from_str(&ok(&mut cmd)).unwrap();
    v["question"].as_i64().unwrap()
}

/// The usage guide as the spec states it: the code block of docs/board-v2.md section 8.
/// A checkout without the design documents has no spec to compare with; there the guide is
/// the text `kb --skill` prints before the command reference.
fn usage_guide() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/board-v2.md");
    let Ok(doc) = std::fs::read_to_string(path) else {
        let text = stdout(&kb().arg("--skill").output().unwrap());
        let end = text
            .find("\n\n# Command reference\n")
            .expect("command reference");
        return text[..end].to_string();
    };
    let doc = doc.replace("\r\n", "\n");
    let section = &doc[doc.find("\n## 8. ").expect("section 8")..];
    let section = &section[..section[1..].find("\n## ").map_or(section.len(), |i| i + 1)];
    let start = section.find("```\n").expect("code block") + 4;
    let len = section[start..].find("\n```").expect("end of code block");
    section[start..start + len].to_string()
}

const EMPTY_BOARD: &str = "This session has an empty board. Design it now with \"kb board init\".";
const START_TERMINAL_HINT: &str = "hint: no terminal is recorded for this board. If you run in a \
     terminal workspace (tabs or panes), run \"kb board terminal\" to see how to record it.";

/// Starts the session's board the way `/kb` does (`--new`: other test boards share the
/// folder).
fn start(session: &str) {
    ok(kb_s(session).args(["board", "start", "--new"]));
}

// --- session-start / session-end (docs/hooks.md 3.1, 3.6; docs/board-v2.md 12.2) ----------------

#[test]
fn session_start_injects_context_only_for_a_started_board() {
    migrate();
    let s = unique("start");
    let cwd = std::env::temp_dir().join(unique("專案"));
    std::fs::create_dir_all(&cwd).unwrap();
    let env_file = std::env::temp_dir().join(format!("{}.sh", unique("env")));
    let input = json!({"session_id": s, "source": "startup", "cwd": cwd, "hook_event_name": "SessionStart", "unknown": {"x": 1}});

    // No board: silent, nothing created, no KB_SESSION export (docs/board-v2.md 12.2).
    let o = hook_with(
        kb().env("CLAUDE_ENV_FILE", &env_file),
        "session-start",
        &input,
    );
    silent(&o);
    assert!(o.stderr.is_empty(), "{}", stderr(&o));
    assert!(!env_file.exists());
    let o = kb_s(&s).args(["board", "show"]).output().unwrap();
    assert!(stderr(&o).starts_with("no board for session"));

    // kb board start (what /kb runs) creates the board and prints the guide.
    let out = ok(kb_s(&s).current_dir(&cwd).args(["board", "start"]));
    let guide = usage_guide();
    assert!(
        guide.starts_with("# Kanban board (kb)\nA human watches"),
        "{guide}"
    );
    assert!(guide.ends_with("in the user's language."), "{guide}");
    assert!(guide.contains("--agreed") && guide.contains("Do not invent work"));
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("board ")
            && first.contains(&format!(" created for session {s} in workspace ")),
        "{out}"
    );
    assert!(first.contains("專案"), "{first}");
    // No terminal is known: the last line tells the agent how to record it
    // (docs/board-v2.md section 26.5).
    assert_eq!(
        out.trim_end(),
        format!("{first}\n\n{guide}\n\n{EMPTY_BOARD}\n\n{START_TERMINAL_HINT}")
    );
    let v = board_json(&s);
    assert_eq!(v["board"]["title"], "");
    assert_eq!(v["board"]["status"], "active");
    assert_eq!(v["lanes"].as_array().unwrap().len(), 0);
    assert_eq!(v["workspace"]["source"], "dir");
    assert!(v["board"]["cwd"].as_str().unwrap().contains("專案"));
    // The board start also starts the turn (docs/board-v2.md section 12.4).
    assert_eq!(verbs(&s), ["user.prompt", "board.create"]);

    // Now session-start injects the guide and exports KB_SESSION.
    let o = hook_with(
        kb().env("CLAUDE_ENV_FILE", &env_file),
        "session-start",
        &input,
    );
    let ctx = context(&o, "SessionStart");
    assert_eq!(ctx, format!("{guide}\n\n{EMPTY_BOARD}"));
    // The raw output escapes the Chinese of the usage guide.
    assert!(stdout(&o).contains("\\u540d\\u7a31:kind"), "{}", stdout(&o));
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        format!("export KB_SESSION={s}\n")
    );
    assert_eq!(verbs(&s), ["session.start", "user.prompt", "board.create"]);

    // With lanes, the context shows the board; output stays ASCII.
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "修正登入逾時",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args([
        "card",
        "add",
        "--lane",
        "實作",
        "--title",
        "修正 token 更新時間",
        "--priority",
        "P1",
    ]));
    let o = hook(
        "session-start",
        &json!({"session_id": s, "source": "compact"}),
    );
    let ctx = context(&o, "SessionStart");
    assert!(ctx.starts_with(&guide));
    assert!(
        ctx.contains("\n\nboard: 修正登入逾時 [active] rev 5\n"),
        "{ctx}"
    );
    assert!(
        ctx.ends_with(
            "== 實作 (doing) ==\n  [1] 修正 token 更新時間 (derived)\n== 完成 (done) == 0 cards"
        ),
        "{ctx}"
    );
    assert_eq!(activity(&s)[0]["detail"]["source"], "compact");
    // The context got the board without a `next:` line: the next write prints it again,
    // although it is unchanged since `board init` (docs/board-v2.md section 15.2).
    let out = ok(kb_s(&s).args(["card", "note", "1", "--kind", "decision", "先查 log"]));
    // A decision note without --because ends with the hint (docs/board-v2.md 18.5).
    assert!(
        out.ends_with(
            "\nnext: (no card in the todo lanes)\nhint: if this follows from a card, a question, \
             a comment or the user, name it with --because <card> | q<id> | c<id> | user. \
             Your own choice needs no --because.\n"
        ),
        "{out}"
    );
    let out = ok(kb_s(&s).args(["card", "note", "1", "--kind", "result", "找到了"]));
    assert!(!out.contains("next:"), "{out}");

    // session-end archives with the automatic outcome; resume reopens and clears the
    // archive summary.
    silent(&hook(
        "session-end",
        &json!({"session_id": s, "reason": "clear"}),
    ));
    let v = board_json(&s);
    assert_eq!(v["board"]["status"], "archived");
    assert_eq!(v["board"]["outcome"], "incomplete");
    assert_eq!(v["board"]["summary"], "session ended: clear");
    assert_eq!(activity(&s)[1]["detail"]["reason"], "clear");
    let o = hook(
        "session-start",
        &json!({"session_id": s, "source": "resume"}),
    );
    context(&o, "SessionStart");
    let v = board_json(&s);
    assert_eq!(v["board"]["status"], "active");
    assert_eq!(v["board"]["summary"], Value::Null);

    // A board without cards ends as abandoned.
    let empty = unique("empty");
    start(&empty);
    silent(&hook(
        "session-end",
        &json!({"session_id": empty, "reason": "logout"}),
    ));
    assert_eq!(board_json(&empty)["board"]["outcome"], "abandoned");
}

// --- kb board start / adopt, kb --skill (docs/board-v2.md 12.4, 12.5, 12.11) ----------------

#[test]
fn board_start_lists_candidates_and_adopt_takes_over() {
    migrate();
    let cwd = std::env::temp_dir().join(unique("folder"));
    std::fs::create_dir_all(&cwd).unwrap();
    let in_dir = |s: &str| {
        let mut c = kb_s(s);
        c.current_dir(&cwd);
        c
    };
    let first = unique("first");
    ok(in_dir(&first).args(["board", "start", "--title", "看板", "--goal", "目標"]));
    let v = board_json(&first);
    assert_eq!(
        (v["board"]["title"].as_str(), v["board"]["goal"].as_str()),
        (Some("看板"), Some("目標"))
    );
    let id = v["board"]["id"].as_i64().unwrap();
    // Idempotent.
    let out = ok(in_dir(&first).args(["board", "start", "--title", "別的"]));
    assert!(
        out.starts_with(&format!(
            "board {id} is already active for session {first}\n\n# Kanban board (kb)"
        )),
        "{out}"
    );
    assert_eq!(board_json(&first)["board"]["title"], "看板");

    // Same folder, new session: candidates instead of a board.
    let second = unique("second");
    let out = ok(in_dir(&second).args(["board", "start"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        format!(
            "no board for session {second}. Recent boards that may belong to this conversation:"
        )
    );
    assert!(
        lines[1].starts_with(&format!("  board {id} [active] 看板  (changed ")),
        "{out}"
    );
    assert_eq!(lines[2], format!("    kb board adopt {id}"));
    assert_eq!(
        lines.last().unwrap(),
        &"To start a new board instead: kb board start --new"
    );
    let o = kb_s(&second).args(["board", "show"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim_end(),
        format!(
            "no board for session {second}: run \"kb board init\" first\n\
             hint: start a board with \"kb board start\", or take over an earlier session's board with \"kb board adopt <board id>\""
        )
    );
    let v: Value =
        serde_json::from_str(&ok(in_dir(&second).args(["--json", "board", "start"]))).unwrap();
    assert_eq!(v["status"], "candidates");
    assert_eq!(v["board"], Value::Null);
    assert_eq!(v["candidates"][0]["id"], id);

    // Adopt: errors, then success (by board id).
    let o = kb_s(&second)
        .args(["board", "adopt", "999999999999"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o).trim(), "board not found: 999999999999");
    let o = kb_s(&first)
        .args(["board", "adopt", &id.to_string()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        format!("session {first} already has board {id}")
    );
    let out = ok(kb_s(&second).args(["board", "adopt", &id.to_string()]));
    assert_eq!(
        out.trim_end(),
        format!(
            "board {id} adopted by session {second} (was {first})\n\n{}\n\n{EMPTY_BOARD}",
            usage_guide()
        )
    );
    assert_eq!(board_json(&second)["board"]["id"], id);
    let a = &activity(&second)[0];
    assert_eq!(a["verb"], "board.adopt");
    assert_eq!(a["detail"], json!({"from": first, "auto": false}));

    // --new creates a board linked to the first candidate.
    let third = unique("third");
    let out = ok(in_dir(&third).args(["board", "start", "--new"]));
    assert!(out.contains(" created for session "), "{out}");
    assert_eq!(board_json(&third)["board"]["prev_board_id"], id);

    // The session key of start/adopt never comes from the terminal (KB_TERMINAL_ID).
    let o = kb()
        .env("KB_TERMINAL_ID", unique("t"))
        .args(["board", "start"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        "no session: set KB_SESSION or pass --session"
    );
}

#[test]
fn skill_and_help_point_agents_to_the_guide() {
    let o = kb().arg("--skill").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let text = stdout(&o);
    let guide = usage_guide();
    assert!(
        text.starts_with(&format!("{guide}\n\n# Command reference\n")),
        "{text}"
    );
    for cmd in [
        "kb board start",
        "kb board adopt",
        "kb board init",
        "kb board show",
        "kb board check",
        "kb epic add",
        "kb lane add",
        "kb card add",
        "kb card move",
        "kb card note",
        "kb card done",
        "kb link add",
        "kb ask",
        "kb inbox",
        "kb question list",
        "kb activity",
        "$KB_SESSION",
    ] {
        assert!(text.contains(cmd), "missing {cmd}");
    }
    let o = kb().arg("--help").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        stdout(&o).trim_end().lines().last(),
        Some("Agent? Run: kb --skill")
    );
    let o = kb().output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = kb().args(["--skill", "board", "show"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

/// What `SessionStart` prints when `kb` cannot run (docs/board-v2.md section 12.12).
const MISSING_KB_NOTICE: &str = "[kb] The kb plugin is installed, but the kb program was not found on PATH, so the kanban board cannot work. Tell the user this once. Only the user can install it, by typing /kb:kb. Do not invoke /kb:kb yourself.";

/// The six events with a shell-form `kb hook` command.
const HOOK_EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

/// The shell-form command of each event (docs/board-v2.md sections 12.12 and 13.5).
fn shell_hook_command(event: &str) -> Option<String> {
    Some(match event {
        "SessionStart" => format!("kb hook session-start || echo '{MISSING_KB_NOTICE}'"),
        "UserPromptSubmit" => "kb hook prompt || cd .".to_string(),
        "PreToolUse" => "kb hook pre-tool || cd .".to_string(),
        "PostToolUse" => "kb hook tool-used || cd .".to_string(),
        "Stop" => "kb hook stop || cd .".to_string(),
        "SessionEnd" => "kb hook session-end || cd .".to_string(),
        _ => return None,
    })
}

#[test]
fn plugin_matches_the_manual_settings_example() {
    // docs/board-v2.md section 12.12: same bindings on six events; `kb hook <event>`, followed
    // by the notice on SessionStart and by `|| cd .` on the five others (exit 0 when `kb`
    // cannot run). Section 13.5: the waiter is `kb` + args ["hook", "wait"] (exec form), async with
    // asyncRewake and an explicit 24-hour timeout, on SessionStart and Stop.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
    let read = |p: &str| std::fs::read_to_string(format!("{root}{p}")).unwrap();
    let plugin: Value = serde_json::from_str(&read("plugin/hooks/hooks.json")).unwrap();
    let example: Value = serde_json::from_str(&read("examples/claude-settings.json")).unwrap();
    assert_eq!(plugin, example);
    let events = plugin["hooks"].as_object().unwrap();
    assert_eq!(events.len(), 6);
    let mut waiters = Vec::new();
    let mut judges = Vec::new();
    let mut shell_forms = Vec::new();
    for (event, groups) in events {
        for group in groups.as_array().unwrap() {
            for h in group["hooks"].as_array().unwrap() {
                let cmd = h["command"].as_str().unwrap();
                assert!(!cmd.contains('\\') && !cmd.contains(".ps1"), "{cmd}");
                if let Some(args) = h.get("args") {
                    assert_eq!(cmd, "kb");
                    assert_eq!(h["async"], true);
                    assert_eq!(h["asyncRewake"], true);
                    if args == &json!(["hook", "judge"]) {
                        // docs/hooks.md section 3.7: the model judge, in the background.
                        assert_eq!(h["timeout"], 60);
                        judges.push(event.as_str());
                        continue;
                    }
                    assert_eq!(args, &json!(["hook", "wait"]));
                    assert_eq!(h["timeout"], 86400);
                    waiters.push(event.as_str());
                } else {
                    assert_eq!(Some(cmd.to_string()), shell_hook_command(event), "{event}");
                    assert!(h.get("asyncRewake").is_none(), "{cmd}");
                    shell_forms.push(event.as_str());
                }
            }
        }
    }
    shell_forms.sort();
    let mut expected = HOOK_EVENTS.to_vec();
    expected.sort();
    assert_eq!(shell_forms, expected);
    // The notice is quoted with single quotes in the same way by sh and PowerShell: it holds
    // no character that either shell or JSON treats specially inside such quotes.
    assert!(
        MISSING_KB_NOTICE
            .chars()
            .all(|c| c.is_ascii() && !"'\"`$!\\;&|<>(){}".contains(c)),
        "{MISSING_KB_NOTICE}"
    );
    waiters.sort();
    assert_eq!(waiters, ["SessionStart", "Stop"]);
    assert_eq!(judges, ["Stop"]);
    let manifest: Value = serde_json::from_str(&read("plugin/.claude-plugin/plugin.json")).unwrap();
    assert_eq!(manifest["name"], "kb");
    let skill = read("plugin/skills/kb/SKILL.md").replace("\r\n", "\n");
    assert!(skill.starts_with("---\nname: kb\n"), "{skill}");
    let front = &skill[..skill[4..].find("\n---\n").unwrap() + 4];
    assert!(
        front.contains("\ndisable-model-invocation: true"),
        "{front}"
    );
    assert!(
        !front.contains("shell:"),
        "the skill uses the default shell"
    );
    assert!(skill.contains(&format!("\n!`{SKILL_START}`\n")), "{skill}");
    assert!(skill.contains("$ARGUMENTS"));
}

/// The skill's injected command (docs/board-v2.md section 12.12): the same text runs in
/// bash, Git Bash and PowerShell 7, and prints `KB_NOT_INSTALLED` only when `kb` is missing.
const SKILL_START: &str =
    "kb board start --session ${CLAUDE_SESSION_ID} || kb --version || echo KB_NOT_INSTALLED";
const SKILL_MARKER: &str = "KB_NOT_INSTALLED";

#[test]
fn skill_reports_a_missing_kb_with_a_marker() {
    // docs/board-v2.md section 12.12: every part of the injected command is pre-allowed for
    // both shells (a part that is not allowed aborts the invocation), and the skill tells the
    // agent what the marker means and to ask before installing.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");
    let skill = std::fs::read_to_string(format!("{root}plugin/skills/kb/SKILL.md"))
        .unwrap()
        .replace("\r\n", "\n");
    let allowed = skill
        .lines()
        .find_map(|l| l.strip_prefix("allowed-tools: "))
        .expect("allowed-tools line");
    let rules: Vec<&str> = allowed
        .split(") ")
        .map(|r| r.trim_end_matches(')'))
        .collect();
    let session = SKILL_START.replace("${CLAUDE_SESSION_ID}", "3f2a9c1e");
    for part in session.split(" || ") {
        for tool in ["Bash", "PowerShell"] {
            let covered = rules.iter().any(|r| {
                let Some(pattern) = r.strip_prefix(&format!("{tool}(")) else {
                    return false;
                };
                match pattern.strip_suffix(" *") {
                    Some(prefix) => part.starts_with(&format!("{prefix} ")),
                    None => part == pattern,
                }
            });
            assert!(covered, "{tool} does not allow {part:?}: {allowed}");
        }
    }
    assert!(skill.contains(&format!("If the last line above is `{SKILL_MARKER}`")));
    assert!(skill.contains("\nInstalling kb:\n"));
    assert!(skill.contains("do nothing until the user says yes"));
    assert!(skill.contains("cargo install --path crates/kb --locked"));
}

#[cfg(unix)]
#[test]
fn skill_command_prints_the_marker_only_without_kb() {
    // Runs the injected command the way Claude Code's Bash tool would, without a database.
    let script = SKILL_START.replace("${CLAUDE_SESSION_ID}", "skill-marker-test");
    // A refused port: no database is touched. An empty HOME: no config file is read.
    let refused = "postgres://kb:x@127.0.0.1:1/kanban_test";
    let empty = std::env::temp_dir().join(unique("no-kb"));
    std::fs::create_dir_all(&empty).unwrap();
    let run = |path: &str| {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .env("PATH", path)
            .env("HOME", &empty)
            .env("KB_WRITER_URL", refused)
            .env("KB_READER_URL", refused)
            .env_remove("KB_SESSION")
            .env_remove("KB_TERMINAL_ID")
            .env_remove("KB_TERMINAL_SPACE")
            .env_remove("KB_TERMINAL_TAB")
            .env_remove("KB_WORKSPACE_NAME")
            .output()
            .unwrap()
    };
    let o = run(empty.to_str().unwrap());
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(stdout(&o).trim_end().lines().last(), Some(SKILL_MARKER));

    // kb present but the database unreachable: no marker; the version line follows the error.
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_kb"))
        .parent()
        .unwrap();
    let o = run(bin.to_str().unwrap());
    assert_eq!(o.status.code(), Some(0));
    let out = stdout(&o);
    assert!(!out.contains(SKILL_MARKER), "{out}");
    assert!(out.trim_end().ends_with(env!("CARGO_PKG_VERSION")), "{out}");
}

#[cfg(unix)]
#[test]
fn hook_commands_without_kb_notice_once_and_stay_quiet() {
    // docs/board-v2.md section 12.12: run each shell-form hook command the way Claude Code
    // does on macOS and Linux (`sh -c`), without a database.
    let refused = "postgres://kb:x@127.0.0.1:1/kanban_test";
    let empty = std::env::temp_dir().join(unique("no-kb"));
    std::fs::create_dir_all(&empty).unwrap();
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_kb"))
        .parent()
        .unwrap();
    let run = |cmd: &str, path: &std::path::Path| {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .env("PATH", path)
            .env("HOME", &empty)
            .env("KB_WRITER_URL", refused)
            .env("KB_READER_URL", refused)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    for event in HOOK_EVENTS {
        let cmd = shell_hook_command(event).unwrap();
        // kb missing: SessionStart tells the agent; the others exit 0 without output.
        let o = run(&cmd, &empty);
        match event {
            "SessionStart" => {
                assert_eq!(o.status.code(), Some(0));
                assert_eq!(stdout(&o).trim_end(), MISSING_KB_NOTICE);
            }
            _ => {
                assert_eq!(o.status.code(), Some(0), "{event}");
                assert!(o.stdout.is_empty(), "{event}");
            }
        }
        // kb present (invalid input): exit 0 and no notice.
        let o = run(&cmd, bin);
        assert_eq!(o.status.code(), Some(0), "{event}");
        assert!(o.stdout.is_empty(), "{event}: {}", stdout(&o));
    }
}

// --- automatic adoption (docs/board-v2.md 12.8) ----------------------------------------------

/// A transcript directory with `<old>.jsonl` ending in a `continued-in` record to `new`.
fn moved_transcripts(old: &str, new: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(unique("projects"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{old}.jsonl")),
        format!(
            "{{\"type\":\"user\",\"sessionId\":\"{old}\"}}\n\
             {{\"type\":\"continued-in\",\"timestamp\":\"2026-11-16T03:28:08.567Z\",\"sessionId\":\"{old}\",\"continuedInSessionId\":\"{new}\"}}\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join(format!("{new}.jsonl")), "{\"type\":\"user\"}\n").unwrap();
    dir
}

#[test]
fn prompt_adopts_the_board_of_a_moved_conversation() {
    migrate();
    let old = unique("old");
    let new = unique("new");
    start(&old);
    ok(kb_s(&old).args([
        "board",
        "init",
        "--title",
        "搬家",
        "--lanes",
        "待辦:todo,完成:done",
    ]));
    ok(kb_s(&old).args(["card", "add", "--lane", "待辦", "--title", "卡片"]));
    let id = board_json(&old)["board"]["id"].clone();
    let dir = moved_transcripts(&old, &new);
    let transcript = dir.join(format!("{new}.jsonl"));

    // Without transcript_path nothing happens.
    silent(&hook(
        "prompt",
        &json!({"session_id": new, "prompt": "繼續"}),
    ));
    // With it, the hook adopts the board and tells the agent about it.
    let env_file = std::env::temp_dir().join(format!("{}.sh", unique("env")));
    let o = hook_with(
        kb().env("CLAUDE_ENV_FILE", &env_file),
        "prompt",
        &json!({"session_id": new, "prompt": "繼續", "transcript_path": transcript}),
    );
    let ctx = context(&o, "UserPromptSubmit");
    assert!(ctx.starts_with(&usage_guide()), "{ctx}");
    assert!(ctx.contains("\n\nboard: 搬家 [active] "), "{ctx}");
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        format!("export KB_SESSION={new}\n")
    );
    let v = board_json(&new);
    assert_eq!(v["board"]["id"], id);
    let rows = activity(&new);
    assert_eq!(rows[0]["verb"], "user.prompt");
    assert_eq!(rows[1]["verb"], "board.adopt");
    assert_eq!(rows[1]["actor"], "hook");
    assert_eq!(rows[1]["detail"], json!({"from": old, "auto": true}));
    // Next prompt: an ordinary turn.
    silent(&hook(
        "prompt",
        &json!({"session_id": new, "prompt": "再一次", "transcript_path": transcript}),
    ));

    // session-start adopts too, and follows a chain (newer -> new -> board).
    let newer = unique("newer");
    std::fs::write(
        dir.join(format!("{new}.jsonl")),
        format!("{{\"type\":\"continued-in\",\"sessionId\":\"{new}\",\"continuedInSessionId\":\"{newer}\"}}\n"),
    )
    .unwrap();
    // `new` keeps the board in this step, so move it back to `old` first: the chain is
    // newer -> new (no board) -> old (board).
    ok(kb_s(&old).args(["board", "adopt", &new]));
    let o = hook(
        "session-start",
        &json!({"session_id": newer, "source": "startup", "transcript_path": dir.join(format!("{newer}.jsonl"))}),
    );
    let ctx = context(&o, "SessionStart");
    assert!(ctx.contains("board: 搬家 [active]"), "{ctx}");
    assert_eq!(board_json(&newer)["board"]["id"], id);
    assert_eq!(activity(&newer)[1]["detail"]["from"], json!(old));

    // A session that was not continued from anywhere stays without a board.
    let other = unique("other");
    silent(&hook(
        "session-start",
        &json!({"session_id": other, "transcript_path": dir.join("other.jsonl")}),
    ));
    let o = kb_s(&other).args(["board", "show"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

// --- connection time limits (docs/board-v2.md 12.9) -----------------------------------------

#[test]
fn unreachable_database_fails_open_within_the_time_limit() {
    // 10.255.255.1 drops packets, so the connection attempt hangs until the limit.
    for (event, budget) in [("prompt", 500u128), ("session-start", 3000u128)] {
        let started = std::time::Instant::now();
        let o = hook_with(
            kb().env(
                "KB_WRITER_URL",
                "postgres://kb_writer:x@10.255.255.1:5432/kanban_test",
            ),
            event,
            &json!({"session_id": unique("slow"), "prompt": "x"}),
        );
        let elapsed = started.elapsed().as_millis();
        assert_eq!(o.status.code(), Some(0), "{event}");
        assert!(o.stdout.is_empty(), "{event}");
        assert_eq!(
            stderr(&o).trim(),
            "kb hook: database unreachable",
            "{event}"
        );
        assert!(elapsed < budget + 1500, "{event} took {elapsed} ms");
    }
    // A refused port also returns within the budget of the short events.
    let started = std::time::Instant::now();
    let o = hook_with(
        kb().env(
            "KB_WRITER_URL",
            "postgres://kb_writer:x@127.0.0.1:1/kanban_test",
        ),
        "stop",
        &json!({"session_id": unique("refused")}),
    );
    assert_eq!(o.status.code(), Some(0));
    let elapsed = started.elapsed().as_millis();
    assert!(elapsed < 500 + 1500, "stop took {elapsed} ms");
}

// --- prompt (3.2) -------------------------------------------------------------------------

#[test]
fn prompt_starts_turn_and_injects_answers_and_hints() {
    migrate();
    let s = unique("prompt");
    start(&s);

    // No lanes and R1 not off: a hint.
    let o = hook(
        "prompt",
        &json!({"session_id": s, "prompt_id": "p-1", "prompt": "幫我修正登入逾時"}),
    );
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        "The board has no lanes. Run \"kb board init\" before starting work."
    );
    let a = &activity(&s)[0];
    assert_eq!(
        (a["verb"].as_str(), a["actor"].as_str()),
        (Some("user.prompt"), Some("user"))
    );
    assert_eq!(a["detail"]["excerpt"], "幫我修正登入逾時");
    assert_eq!(a["turn_key"], "p-1");
    assert_eq!(board_json(&s)["board"]["turn_key"], "p-1");

    // R1 off: no hint, no output.
    let off = rules_file(Some("[rules]\nR1 = \"off\"\n"));
    silent(&hook_with(
        kb().env("KB_RULES_FILE", &off),
        "prompt",
        &json!({"session_id": s, "prompt": "x"}),
    ));

    // With lanes and nothing to deliver: no output.
    ok(kb_s(&s).args(["board", "init", "--title", "看板"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "卡片"]));
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "繼續"})));

    // An undelivered answer is injected and marked delivered.
    let q = ask(
        &s,
        "1",
        "token 的有效期要改成多久？",
        &["15 分鐘", "30 分鐘"],
    );
    let q2 = ask(&s, "1", "要不要一併重構排程器？", &["要", "不要"]);
    let _q3 = ask(&s, "1", "還在等待", &["a", "b"]);
    answer(q, &option_ids(&s, q)[..1], false);
    answer(q2, &[], true);
    let o = hook("prompt", &json!({"session_id": s, "prompt": "好了"}));
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        format!(
            "question {q} on card 1: token 的有效期要改成多久？\n  answer: 15 分鐘\n\
             question {q2} on card 1: 要不要一併重構排程器？\n  answer: (the user will reply in this session)\n\
             open questions: 1"
        )
    );
    assert_eq!(
        &verbs(&s)[..3],
        ["question.deliver", "question.deliver", "user.prompt"]
    );
    silent(&hook(
        "prompt",
        &json!({"session_id": s, "prompt": "再一次"}),
    ));
}

#[test]
fn typed_answers_print_a_text_line_in_every_output() {
    // docs/board-v2.md section 7.1: `text:` under the answer; no `answer:` with text only.
    migrate();
    let s = unique("text");
    ok(kb_s(&s).args(["board", "init", "--title", "看板"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "卡片"]));
    let q = ask(
        &s,
        "1",
        "token 的有效期要改成多久？",
        &["15 分鐘", "30 分鐘"],
    );
    let q2 = ask(&s, "1", "要不要一併重構排程器？", &["要", "不要"]);
    answer_with_text(
        q,
        &option_ids(&s, q)[..1],
        false,
        Some("refresh 的間隔也要一起改"),
    );
    answer_with_text(q2, &[], false, Some("先不要，等這一輪結束再說"));
    let expected = format!(
        "question {q} on card 1: token 的有效期要改成多久？\n  answer: 15 分鐘\n  text: refresh 的間隔也要一起改\n\
         question {q2} on card 1: 要不要一併重構排程器？\n  text: 先不要，等這一輪結束再說\n\
         open questions: 0"
    );
    assert_eq!(ok(kb_s(&s).args(["inbox", "--peek"])).trim_end(), expected);
    let show = ok(kb_s(&s).args(["question", "show", &q2.to_string()]));
    assert!(
        show.contains(
            "  [ ] (the user will reply in this session)\n  text: 先不要，等這一輪結束再說\n"
        ),
        "{show}"
    );
    let card = ok(kb_s(&s).args(["card", "show", "1"]));
    assert!(
        card.contains("[x] 1. 15 分鐘") && card.contains("    text: refresh 的間隔也要一起改"),
        "{card}"
    );
    // The hooks deliver the same text.
    let o = hook("prompt", &json!({"session_id": s, "prompt": "好了"}));
    assert_eq!(context(&o, "UserPromptSubmit"), expected);
}

#[test]
fn prompt_shows_the_last_turn_warnings_once() {
    // docs/board-v2.md section 14.2.
    migrate();
    let edit = |s: &str| json!({"session_id": s, "tool_name": "Edit", "tool_input": {"file_path": "a.rs"}});

    // No board: an edit and the next prompt stay silent.
    let none = unique("remind-none");
    silent(&hook("pre-tool", &edit(&none)));
    silent(&hook(
        "prompt",
        &json!({"session_id": none, "prompt": "hi"}),
    ));

    let s = unique("remind");
    start(&s);
    let o = hook("prompt", &json!({"session_id": s, "prompt": "開始"}));
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        "The board has no lanes. Run \"kb board init\" before starting work."
    );

    // Turn 1: two edits before the board has cards (R1 warns twice, pre-tool), then a lane
    // over its WIP limit (R2 warns at stop).
    silent(&hook("pre-tool", &edit(&s)));
    silent(&hook("pre-tool", &edit(&s)));
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "看板",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args(["lane", "add", "驗證", "--kind", "doing", "--wip", "1"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "驗證", "--title", "甲"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "驗證", "--title", "乙"]));
    silent(&hook("stop", &json!({"session_id": s})));
    let rows = activity(&s);
    assert_eq!(rows[0]["verb"], "turn.summary");
    assert_eq!(rows[1]["verb"], "check.warn");
    assert_eq!(rows[1]["detail"]["rule"], "R2");
    assert_eq!(
        rows[1]["detail"]["text"],
        "R2: lane \"驗證\" has 2 cards, over its WIP limit of 1."
    );

    // kb board check writes nothing; a wake turn neither shows nor consumes the warnings.
    let o = kb_s(&s).args(["board", "check"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    silent(&hook(
        "prompt",
        &json!({"session_id": s, "prompt": "<task-notification>\n[kb] question 1 answered. Run: kb inbox\n</task-notification>"}),
    ));

    // The next real prompt shows R1 (once, from pre-tool) and R2 (from stop).
    let r1 = "R1: the board needs lanes and at least one card before files are edited. \
              Fix: kb board init --title \"<title>\" --lanes \"<name>:todo,<name>:doing,<name>:done\" ; \
              kb card add --lane <lane> --title \"<title>\"";
    let r2 = "R2: lane \"驗證\" has 2 cards, over its WIP limit of 1. \
              Fix: kb card move <n> --to <lane>  (move cards out of \"驗證\")";
    let o = hook("prompt", &json!({"session_id": s, "prompt": "繼續"}));
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        format!("Board reminders from the last turn:\n{r1}\n{r2}")
    );

    // Turn 2: an edit without card activity (R5) and the same lane (R2); R1 is not repeated.
    silent(&hook(
        "tool-used",
        &json!({"session_id": s, "tool_name": "Edit", "tool_input": {"file_path": "a.rs"}}),
    ));
    silent(&hook("stop", &json!({"session_id": s})));
    let o = hook("prompt", &json!({"session_id": s, "prompt": "再來"}));
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        format!(
            "Board reminders from the last turn:\n{r2}\n\
             R5: files were edited this turn (1 edits) but no card has activity this turn. \
             Fix: kb card note <n> \"<what changed>\"  (or add a card for this work)"
        )
    );

    // Each warning is shown once: nothing new, no output.
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "好"})));
}

#[test]
fn the_kb_turn_checks_edits_except_scratch() {
    // docs/board-v2.md section 12.4 and docs/hooks.md section 4: /kb's `kb board start`
    // starts the turn, and R5 counts every edit except scratch ones. The project lives in
    // the temporary folder, so its own files count and the rest of that folder does not.
    migrate();
    let project = std::env::temp_dir().join(unique("專案"));
    std::fs::create_dir_all(&project).unwrap();
    let cwd = project.to_string_lossy().to_string();
    let edit = |s: &str, path: &str| json!({"session_id": s, "cwd": cwd, "tool_name": "Edit", "tool_input": {"file_path": path}});
    let outside = std::env::temp_dir()
        .join(unique("scratch"))
        .join("notes.md")
        .to_string_lossy()
        .to_string();
    let r5 = |s: &str| {
        activity(s)
            .iter()
            .any(|a| a["verb"] == "check.warn" && a["detail"]["rule"] == "R5")
    };

    // The prompt hook of /kb runs before the board exists: silent.
    let s = unique("kb-turn");
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "/kb"})));
    ok(kb_s(&s)
        .current_dir(&project)
        .args(["board", "start", "--new"]));
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "看板",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    // A scratch edit only: R5 passes.
    silent(&hook("tool-used", &edit(&s, &outside)));
    assert_eq!(activity(&s)[0]["detail"]["scratch"], true);
    silent(&hook("stop", &json!({"session_id": s})));
    assert!(!r5(&s));

    // The same turn with an edit of the project (relative to the hook's cwd): R5 warns.
    let s = unique("kb-turn");
    ok(kb_s(&s)
        .current_dir(&project)
        .args(["board", "start", "--new"]));
    ok(kb_s(&s).args(["board", "init", "--title", "看板"]));
    silent(&hook("tool-used", &edit(&s, &outside)));
    silent(&hook("tool-used", &edit(&s, "src/main.rs")));
    assert!(activity(&s)[0]["detail"].get("scratch").is_none());
    assert!(activity(&s)[0]["detail"].get("in_project").is_none());
    silent(&hook("stop", &json!({"session_id": s})));
    assert!(r5(&s));
    // The next human prompt reminds of it.
    let o = hook("prompt", &json!({"session_id": s, "prompt": "繼續"}));
    assert!(
        context(&o, "UserPromptSubmit").contains(
            "R5: files were edited this turn (1 edits) but no card has activity this turn."
        ),
        "{}",
        stdout(&o)
    );
}

#[test]
fn r5_scratch_in_home_and_elsewhere() {
    // docs/hooks.md sections 3.4 and 4 with a home directory of the test's own
    // (`USERPROFILE` and `HOME` of the kb process): hidden top-level folders of home are
    // scratch, other folders of home and other places count.
    migrate();
    // Real folders outside the temporary folder (`kb board start` stores its folder).
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(unique("home"));
    let home = root.join("u");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    let with_home = || {
        let mut c = kb();
        c.env("USERPROFILE", &h).env("HOME", &h);
        c
    };
    let tool = |s: &str, cwd: &str, path: &std::path::Path| {
        silent(&hook_with(
            &mut with_home(),
            "tool-used",
            &json!({"session_id": s, "cwd": cwd, "tool_name": "Write",
                    "tool_input": {"file_path": path.to_string_lossy()}}),
        ));
        activity(s)[0]["detail"].get("scratch").cloned()
    };
    let stop_r5 = |s: &str| {
        silent(&hook_with(&mut with_home(), "stop", &json!({"session_id": s})));
        let rows = activity(s);
        assert_eq!(rows[0]["verb"], "turn.summary");
        rows.iter()
            .take_while(|a| a["verb"] != "user.prompt")
            .any(|a| a["verb"] == "check.warn" && a["detail"]["rule"] == "R5")
    };
    let board_in = |cwd: &str| {
        let s = unique("scratch");
        ok(kb_s(&s).current_dir(cwd).args(["board", "start", "--new"]));
        ok(kb_s(&s).args([
            "board",
            "init",
            "--title",
            "看板",
            "--lanes",
            "待辦:todo,實作:doing,完成:done",
        ]));
        silent(&hook("prompt", &json!({"session_id": s, "prompt": "開始"})));
        s
    };

    // Home as the project: ~/.claude is scratch, ~/src/x counts.
    let s = board_in(&h);
    let memory = home.join(".claude").join("projects").join("MEMORY.md");
    assert_eq!(tool(&s, &h, &memory), Some(json!(true)));
    assert!(!stop_r5(&s));
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "繼續"})));
    assert_eq!(tool(&s, &h, &home.join("src").join("x").join("main.rs")), None);
    assert!(stop_r5(&s));

    // A project elsewhere: a file outside it, on another path, counts; ~/.claude does not.
    let project_dir = root.join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    let project = project_dir.to_string_lossy().to_string();
    let project = project.as_str();
    let s = board_in(project);
    let other = std::path::Path::new(if cfg!(windows) { r"X:\other" } else { "/other" })
        .join("lib.rs");
    assert_eq!(tool(&s, project, &memory), Some(json!(true)));
    assert!(!stop_r5(&s));
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "繼續"})));
    assert_eq!(tool(&s, project, &other), None);
    assert!(stop_r5(&s));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_rules_turn_ends_only_at_a_stop_that_ends_it() {
    // docs/hooks.md section 2.4: the turn the rules check runs from the previous
    // turn.summary of the stop hook. A wake prompt, a message sent while the agent is busy
    // and a turn interrupted with Esc do not move it; a blocked stop does not end it.
    migrate();
    let s = unique("turn");
    start(&s);
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "看板",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "卡片"]));
    let edit = json!({"session_id": s, "tool_name": "Edit", "tool_input": {"file_path": "a.rs"}});
    let r5_block = rules_file(Some("[rules]\nR5 = \"block\"\n"));
    let stop = |active: bool| {
        hook_with(
            kb().env("KB_RULES_FILE", &r5_block),
            "stop",
            &json!({"session_id": s, "stop_hook_active": active}),
        )
    };
    let blocks_on_r5 = |o: &Output| {
        let v = json_out(o);
        v["decision"] == "block"
            && v["reason"].as_str().unwrap().contains("R5: files were edited this turn (1 edits)")
    };
    let summary = || {
        let a = activity(&s)[0].clone();
        assert_eq!((a["verb"].as_str(), a["actor"].as_str()), (Some("turn.summary"), Some("hook")));
        a["detail"].clone()
    };

    // A normal turn: an edit and a note.
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "開始"})));
    silent(&hook("tool-used", &edit));
    ok(kb_s(&s).args(["card", "note", "1", "改了 a.rs"]));
    silent(&stop(false));
    assert_eq!(summary(), json!({"cards_changed": 1, "notes": 1, "tools": 1}));

    // A wake turn: only an edit. The note of the turn before does not cover it.
    let wake = "<task-notification>\n<status>completed</status>\n</task-notification>";
    silent(&hook("prompt", &json!({"session_id": s, "prompt": wake})));
    silent(&hook("tool-used", &edit));
    assert!(blocks_on_r5(&stop(false)));
    // After the block the turn goes on: the note now covers the edit.
    ok(kb_s(&s).args(["card", "note", "1", "補上紀錄"]));
    silent(&stop(true));
    assert_eq!(summary(), json!({"cards_changed": 1, "notes": 1, "tools": 1}));

    // A wake turn with only a note: no warning, and the summary counts only this turn.
    silent(&hook("prompt", &json!({"session_id": s, "prompt": wake})));
    ok(kb_s(&s).args(["card", "note", "1", "只寫紀錄"]));
    silent(&stop(false));
    assert_eq!(summary(), json!({"cards_changed": 1, "notes": 1, "tools": 0}));

    // A message sent while the agent is busy does not cut the turn: the edit before it
    // still counts.
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "改一下"})));
    silent(&hook("tool-used", &edit));
    let o = hook("prompt", &json!({"session_id": s, "prompt": "順便看 log"}));
    assert_eq!(o.status.code(), Some(0));
    assert!(blocks_on_r5(&stop(false)));
    silent(&stop(true));

    // A turn interrupted with Esc (no Stop) is checked with the next one.
    let o = hook("prompt", &json!({"session_id": s, "prompt": "再改"}));
    assert_eq!(o.status.code(), Some(0));
    silent(&hook("tool-used", &edit));
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "算了，先停"})));
    assert!(blocks_on_r5(&stop(false)));
}

// --- pre-tool (3.3) -----------------------------------------------------------------------

/// A `PreToolUse` denial: exit 0, and stdout is exactly the `hookSpecificOutput` object with
/// `permissionDecision: "deny"`. Returns `permissionDecisionReason`.
fn deny_reason(o: &Output) -> String {
    let v = json_out(o);
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("permissionDecisionReason")
        .to_string();
    assert_eq!(
        v,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }})
    );
    assert_eq!(stdout(o).trim_end().lines().count(), 1, "{}", stdout(o));
    reason
}

#[test]
fn pre_tool_warns_or_blocks_by_level() {
    migrate();
    let s = unique("pre");
    start(&s);
    let edit = json!({"session_id": s, "tool_name": "Edit", "tool_input": {"file_path": "a.rs"}});

    // Default R1 = warn: exit 0, no stdout, check.warn recorded.
    let o = hook("pre-tool", &edit);
    silent(&o);
    assert!(stderr(&o).contains("R1:"), "{}", stderr(&o));
    assert_eq!(activity(&s)[0]["verb"], "check.warn");
    assert_eq!(activity(&s)[0]["detail"]["rule"], "R1");

    // R1 = block: exit 0 with a JSON denial whose reason is the block text (exit code 0 so
    // the hook command can end with `|| cd .`, docs/board-v2.md 12.12); check.block recorded.
    let block = rules_file(Some("[rules]\nR1 = \"block\"\nR6 = \"block\"\n"));
    let o = hook_with(kb().env("KB_RULES_FILE", &block), "pre-tool", &edit);
    let reason = deny_reason(&o);
    assert!(
        reason.starts_with("Kanban rules failed. Fix them, then try again.\nR1: "),
        "{reason}"
    );
    assert!(o.stderr.is_empty(), "{}", stderr(&o));
    assert_eq!(activity(&s)[0]["verb"], "check.block");

    // R1 = off: nothing.
    let off = rules_file(Some("[rules]\nR1 = \"off\"\n"));
    let o = hook_with(kb().env("KB_RULES_FILE", &off), "pre-tool", &edit);
    silent(&o);
    assert!(o.stderr.is_empty());

    // R6: block by default (docs/board-v2.md 23.2) with the documented text; off: nothing.
    let ask_tool =
        json!({"session_id": s, "tool_name": "AskUserQuestion", "tool_input": {"questions": []}});
    for o in [
        hook("pre-tool", &ask_tool),
        hook_with(kb().env("KB_RULES_FILE", &block), "pre-tool", &ask_tool),
    ] {
        assert_eq!(
            deny_reason(&o),
            "AskUserQuestion is disabled here. Ask through the board instead:\n  \
             kb ask --card <n> \"<question>\" --option \"<label>::<description>\" --option \"<label>\"\n\
             If no card covers the question, add one first: kb card add --lane <lane> --title \"<topic>\" --discuss\n\
             Then continue with work that does not depend on the answer."
        );
        assert!(o.stderr.is_empty(), "{}", stderr(&o));
        assert_eq!(activity(&s)[0]["verb"], "check.block");
        assert_eq!(activity(&s)[0]["detail"]["rule"], "R6");
    }
    let r6_off = rules_file(Some("[rules]\nR6 = \"off\"\n"));
    let o = hook_with(kb().env("KB_RULES_FILE", &r6_off), "pre-tool", &ask_tool);
    silent(&o);
    assert!(o.stderr.is_empty());

    // A rules-file warning stays on stderr; the denial on stdout is unchanged.
    let block_bad = rules_file(Some("[rules]\nR6 = \"block\"\nR2 = \"loud\"\n"));
    let o = hook_with(kb().env("KB_RULES_FILE", &block_bad), "pre-tool", &ask_tool);
    assert!(deny_reason(&o).starts_with("AskUserQuestion is disabled here."));
    assert!(
        stderr(&o).contains("kb hook: warning:") && stderr(&o).contains("invalid level for R2"),
        "{}",
        stderr(&o)
    );

    // Other tools are not checked, even when blocking; no database needed.
    let o = hook_with(
        kb().env("KB_RULES_FILE", &block).env(
            "KB_WRITER_URL",
            "postgres://kb_writer:x@127.0.0.1:1/kanban_test",
        ),
        "pre-tool",
        &json!({"session_id": s, "tool_name": "Bash", "tool_input": {"command": "ls"}}),
    );
    silent(&o);

    // Board in place: R1 passes even at block.
    ok(kb_s(&s).args(["board", "init", "--title", "看板"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "卡片"]));
    let o = hook_with(kb().env("KB_RULES_FILE", &block), "pre-tool", &edit);
    silent(&o);

    // An invalid level falls back to the default (R6: block) and warns on stderr.
    let bad = rules_file(Some("[rules]\nR6 = \"loud\"\n"));
    let o = hook_with(kb().env("KB_RULES_FILE", &bad), "pre-tool", &ask_tool);
    assert!(deny_reason(&o).starts_with("AskUserQuestion is disabled here."));
    assert!(
        stderr(&o).contains("invalid level for R6"),
        "{}",
        stderr(&o)
    );
}

// --- tool-used (3.4) ----------------------------------------------------------------------

#[test]
fn tool_used_records_without_notification() {
    migrate();
    let s = unique("tool");
    start(&s);
    let rev = board_json(&s)["board"]["rev"].clone();

    silent(&hook(
        "tool-used",
        &json!({"session_id": s, "tool_name": "Edit", "tool_input": {"file_path": "src/登入.rs"}, "duration_ms": 31}),
    ));
    let long = format!("cargo test {}", "x".repeat(100));
    silent(&hook(
        "tool-used",
        &json!({"session_id": s, "tool_name": "Bash", "tool_input": {"command": long}}),
    ));
    silent(&hook(
        "tool-used",
        &json!({"session_id": s, "tool_name": "PowerShell", "tool_input": {"command": "kb card note 1 x"}}),
    ));
    silent(&hook(
        "tool-used",
        &json!({"session_id": s, "tool_name": "Bash", "tool_input": {"command": "kb.exe board show"}}),
    ));

    let rows = activity(&s);
    assert_eq!(rows[0]["verb"], "tool.use");
    assert_eq!(rows[0]["detail"]["tool"], "Bash");
    assert_eq!(
        rows[0]["detail"]["target"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        80
    );
    assert_eq!(rows[0]["detail"]["duration_ms"], Value::Null);
    // A relative path without a hook cwd cannot be resolved: it is not scratch (no field).
    assert_eq!(
        rows[1]["detail"],
        json!({"tool": "Edit", "target": "src/登入.rs", "duration_ms": 31})
    );
    assert_eq!(
        (rows[2]["verb"].as_str(), rows[3]["verb"].as_str()),
        (Some("user.prompt"), Some("board.create")),
        "kb commands are not recorded"
    );
    assert_eq!(
        board_json(&s)["board"]["rev"],
        rev,
        "tool.use does not bump rev"
    );
}

// --- stop (3.5) ---------------------------------------------------------------------------

#[test]
fn stop_order_active_then_answers_then_rules() {
    migrate();
    let s = unique("stop");
    start(&s);
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "看板",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args([
        "card",
        "add",
        "--lane",
        "實作",
        "--title",
        "修正 token 更新時間",
    ]));
    ok(kb_s(&s).args(["card", "add", "--lane", "實作", "--title", "另一張"]));
    let q = ask(&s, "2", "要保留舊的 API 嗎？", &["保留", "移除"]);
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "開始"})));
    answer(q, &option_ids(&s, q)[..1], false);

    // 1. stop_hook_active: never blocks and delivers nothing; the rules are checked (all
    //    pass here) and the turn summary is recorded (docs/board-v2.md section 16.1).
    silent(&hook(
        "stop",
        &json!({"session_id": s, "stop_hook_active": true}),
    ));
    assert_eq!(verbs(&s)[..2], ["turn.summary", "question.answer"]);

    // 2. Undelivered answers block first.
    let v = json_out(&hook(
        "stop",
        &json!({"session_id": s, "stop_hook_active": false}),
    ));
    assert_eq!(v["decision"], "block");
    assert_eq!(
        v["reason"],
        format!(
            "question {q} on card 2: 要保留舊的 API 嗎？\n  answer: 保留\nopen questions: 0\n\
             Act on these answers before stopping."
        )
    );
    assert_eq!(activity(&s)[0]["verb"], "question.deliver");

    // 3. Then the rules. R3 is off by default (docs/board-v2.md 9): the stop passes and
    //    ends the turn. In the next turn both doing cards have no activity (the answer and
    //    its delivery on card 2 are not the agent's rows either); at level block R3 fails.
    silent(&hook("stop", &json!({"session_id": s})));
    assert_eq!(activity(&s)[0]["verb"], "turn.summary");
    let r3 = rules_file(Some("[rules]\nR3 = \"block\"\n"));
    let o = hook_with(
        kb().env("KB_RULES_FILE", &r3),
        "stop",
        &json!({"session_id": s}),
    );
    let v = json_out(&o);
    assert_eq!(v["decision"], "block");
    assert_eq!(
        v["reason"],
        "Kanban rules failed. Fix them, then stop again.\n\
         R3: card 1 \"修正 token 更新時間\" is in a doing lane but has no activity this turn.\n    \
         Fix: kb card note 1 \"<what happened>\"  (or move/block it)\n\
         R3: card 2 \"另一張\" is in a doing lane but has no activity this turn.\n    \
         Fix: kb card note 2 \"<what happened>\"  (or move/block it)"
    );
    assert!(
        stdout(&o).contains("\\u4fee\\u6b63"),
        "card titles are escaped"
    );
    let rows = activity(&s);
    assert_eq!(rows[0]["verb"], "check.block");
    assert_eq!(rows[0]["detail"]["message"], "#2 這一輪沒有紀錄");
    assert_eq!(rows[1]["detail"]["message"], "#1 這一輪沒有紀錄");

    // kb board check agrees and exits 1; at the default level it exits 0.
    let o = kb_s(&s).args(["board", "check"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("R3  off    FAIL"), "{}", stdout(&o));
    let o = kb_s(&s)
        .env("KB_RULES_FILE", &r3)
        .args(["board", "check"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let text = stdout(&o);
    assert!(text.contains("R3  block  FAIL"), "{text}");
    assert!(text.contains("R1  warn   pass"), "{text}");
    let o = kb_s(&s)
        .env("KB_RULES_FILE", &r3)
        .args(["--json", "board", "check"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["blocked"], true);
    // R1-R5 and R7-R11 (docs/board-v2.md sections 17.4 and 19.4).
    assert_eq!(v["rules"].as_array().unwrap().len(), 10);
    assert_eq!(v["rules"][2]["rule"], "R3");
    assert_eq!(v["rules"][2]["violations"][0]["card"], 1);

    // 4. Fixed: passes with a turn summary.
    ok(kb_s(&s).args(["card", "note", "1", "找到原因"]));
    ok(kb_s(&s).args(["card", "note", "2", "保留舊的 API"]));
    let o = kb_s(&s)
        .env("KB_RULES_FILE", &r3)
        .args(["board", "check"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    let rev = board_json(&s)["board"]["rev"].as_i64().unwrap();
    silent(&hook_with(
        kb().env("KB_RULES_FILE", &r3),
        "stop",
        &json!({"session_id": s}),
    ));
    let a = &activity(&s)[0];
    assert_eq!(a["verb"], "turn.summary");
    assert_eq!(a["actor"], "hook");
    assert_eq!(a["detail"]["cards_changed"], 2);
    assert_eq!(a["detail"]["notes"], 2);
    assert_eq!(board_json(&s)["board"]["rev"].as_i64().unwrap(), rev + 1);

    // tidy renumbers and writes a summary.
    let out = ok(kb_s(&s).args(["board", "tidy"]));
    assert!(out.starts_with("board tidied: "), "{out}");
    assert_eq!(activity(&s)[0]["verb"], "turn.summary");
}

#[test]
fn stop_blocks_once_then_records_and_reminds() {
    // docs/board-v2.md sections 16.1 and 16.2.
    migrate();
    let s = unique("after-block");
    start(&s);
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "看板",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args(["lane", "add", "驗證", "--kind", "doing", "--wip", "1"]));
    // Before the turn: a card in a doing lane that will have no activity (R3, off).
    ok(kb_s(&s).args(["card", "add", "--lane", "實作", "--title", "閒置"]));
    assert_eq!(
        ok(kb_s(&s).args(["board", "check", "--history"])).trim_end(),
        "no rule failures recorded"
    );
    silent(&hook(
        "prompt",
        &json!({"session_id": s, "prompt": "開始", "prompt_id": "abcdef12-3456-7890"}),
    ));

    // The first stop blocks on R4 (a done card without a result).
    ok(kb_s(&s).args(["card", "add", "--lane", "完成", "--title", "重現問題"]));
    let v = json_out(&hook("stop", &json!({"session_id": s})));
    assert_eq!(v["decision"], "block");
    assert_eq!(activity(&s)[0]["verb"], "check.block");

    // The agent goes on without fixing R4 and overfills 驗證 (R2); an answer arrives.
    ok(kb_s(&s).args(["card", "add", "--lane", "驗證", "--title", "甲"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "驗證", "--title", "乙"]));
    let q = ask(&s, "3", "要合併嗎？", &["要", "不要"]);
    answer(q, &option_ids(&s, q)[..1], false);

    // The second stop (stop_hook_active) never blocks: it records and summarizes.
    let o = hook("stop", &json!({"session_id": s, "stop_hook_active": true}));
    silent(&o);
    let rows = activity(&s);
    assert_eq!(rows[0]["verb"], "turn.summary");
    assert_eq!(rows[1]["verb"], "check.warn");
    assert_eq!(rows[2]["verb"], "check.warn");
    assert_eq!(rows[3]["verb"], "question.answer", "nothing delivered");
    let r4 = &rows[1]["detail"];
    assert_eq!(r4["rule"], "R4");
    assert_eq!(r4["level"], "block");
    assert_eq!(r4["after_block"], true);
    assert_eq!(r4["message"], "#2 完成但沒有結果");
    assert_eq!(r4["fix"], "kb card done 2 --result \"<result>\"");
    let r2 = &rows[2]["detail"];
    assert_eq!(
        (r2["rule"].as_str(), r2["level"].as_str()),
        (Some("R2"), Some("warn"))
    );
    assert_eq!(r2["after_block"], true);
    assert!(
        rows.iter().all(|a| a["detail"]["rule"] != "R3"),
        "rules at level off are not recorded"
    );
    // The answer is still undelivered.
    let peek = ok(kb_s(&s).args(["inbox", "--peek"]));
    assert!(
        peek.starts_with(&format!("question {q} on card 3")),
        "{peek}"
    );

    // History: newest first, one line each, local time and turn marker.
    let history = ok(kb_s(&s).args(["board", "check", "--history"]));
    let lines: Vec<&str> = history.lines().collect();
    assert_eq!(lines.len(), 3, "{history}");
    let tail = |l: &str| l.split_once("  turn ").map_or("", |(_, t)| t).to_string();
    assert_eq!(
        tail(lines[0]),
        "abcdef12  R4  not enforced  R4: card 2 \"重現問題\" is done but has no result."
    );
    assert_eq!(
        tail(lines[1]),
        "abcdef12  R2  warned        R2: lane \"驗證\" has 2 cards, over its WIP limit of 1."
    );
    assert_eq!(
        tail(lines[2]),
        "abcdef12  R4  blocked       R4: card 2 \"重現問題\" is done but has no result."
    );
    for l in &lines {
        let time = &l[..11];
        assert!(
            time.chars().enumerate().all(|(i, c)| match i {
                2 => c == '-',
                5 => c == ' ',
                8 => c == ':',
                _ => c.is_ascii_digit(),
            }),
            "{l}"
        );
    }
    let one = ok(kb_s(&s).args(["board", "check", "--history", "--limit", "1"]));
    assert_eq!(one.lines().count(), 1);
    let v: Value = serde_json::from_str(&ok(kb_s(&s).args([
        "--json",
        "board",
        "check",
        "--history",
    ])))
    .unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["outcome"], "not_enforced");
    assert_eq!(arr[0]["level"], "block");
    assert_eq!(arr[0]["after_block"], true);
    assert_eq!(arr[0]["turn_key"], "abcdef12-3456-7890");
    assert_eq!(arr[1]["outcome"], "warned");
    assert_eq!(arr[2]["outcome"], "blocked");
    assert_eq!(arr[2]["level"], Value::Null);
    // --limit needs --history.
    let o = kb_s(&s)
        .args(["board", "check", "--limit", "1"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    // Reading the history writes nothing.
    assert_eq!(verbs(&s)[0], "turn.summary");

    // The next real prompt delivers the answer, then reminds once, one line per rule.
    let o = hook("prompt", &json!({"session_id": s, "prompt": "繼續"}));
    let text = context(&o, "UserPromptSubmit");
    assert!(
        text.starts_with(&format!("question {q} on card 3")),
        "{text}"
    );
    assert!(
        text.ends_with(
            "Board reminders from the last turn:\n\
             R2: lane \"驗證\" has 2 cards, over its WIP limit of 1. \
             Fix: kb card move <n> --to <lane>  (move cards out of \"驗證\")\n\
             R4: card 2 \"重現問題\" is done but has no result. \
             (was not enforced: one block per turn) Fix: kb card done 2 --result \"<result>\""
        ),
        "{text}"
    );
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "好"})));
}

#[test]
fn rules_r7_to_r10_reach_the_agent() {
    // docs/board-v2.md section 17: the echo of R7, the records of the stop hook, the next
    // prompt's reminders, kb board check and --history.
    migrate();
    let s = unique("r7-10");
    start(&s);
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "看板",
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "第一張"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "第二張"]));
    ok(kb_s(&s).args([
        "card",
        "add",
        "--lane",
        "待辦",
        "--title",
        "討論中",
        "--discuss",
        "--next",
    ]));
    silent(&hook(
        "prompt",
        &json!({"session_id": s, "prompt": "開始", "prompt_id": "r7r10abc-0001"}),
    ));

    // R7's echo: one warning line, exit 0. Card 3 is under discussion: not part of the
    // queue, so R10 does not check its move (R7 covers it).
    let o = kb_s(&s)
        .args(["card", "move", "3", "--to", "實作"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        stderr(&o).trim_end(),
        "warning: R7: card 3 is still under discussion; run \"kb card confirm 3\" once the \
         user agreed, or move it back"
    );
    // Card 2 skips the head #1 (R10), then gets an open question (R8). An answer on card 1
    // is delivered this turn without a decision note (R9).
    let o = kb_s(&s)
        .args(["card", "move", "2", "--to", "實作"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty(), "{}", stderr(&o));
    let q8 = ask(&s, "2", "要拆開嗎？", &["要", "不要"]);
    let q9 = ask(&s, "1", "先做哪個？", &["甲", "乙"]);
    answer(q9, &option_ids(&s, q9)[..1], false);

    let r7 = "R7: card 3 \"討論中\" is still under discussion but is in a doing lane. \
              Fix: kb card confirm 3  (once the user agreed; otherwise move it back: \
              kb card move 3 --to <lane>)";
    let r8 = format!(
        "R8: card 2 \"第二張\" is in a doing lane with open question {q8} but is not blocked. \
         Fix: kb card block 2 --reason \"waiting for answer to question {q8}\""
    );
    let r9 = format!(
        "R9: the answer to question {q9} on card 1 \"第一張\" was delivered this turn, but the \
         card has no decision note since. Fix: kb card note 1 --kind decision \"<what was decided>\""
    );
    let r10 = "R10: card 2 \"第二張\" moved from \"待辦\" to \"實作\" this turn, but the head of \
               that queue was #1. Fix: kb card order <n> <n> ...  (the head was #1; fix the \
               order if the queue was wrong)";

    // The first stop delivers the answer (condition 2).
    let v = json_out(&hook("stop", &json!({"session_id": s})));
    assert_eq!(v["decision"], "block");
    assert_eq!(activity(&s)[0]["verb"], "question.deliver");

    // At level off nothing is recorded: kb board check shows the failures and writes
    // nothing. (A stop writes the turn.summary that ends the turn, so the stops below run
    // after the checks of this turn.)
    let off = rules_file(Some(
        "[rules]\nR7 = \"off\"\nR8 = \"off\"\nR9 = \"off\"\nR10 = \"off\"\n",
    ));
    let o = ok(kb_s(&s).env("KB_RULES_FILE", &off).args(["board", "check"]));
    assert!(o.contains("R7  off    FAIL"), "{o}");
    assert_eq!(activity(&s)[0]["verb"], "question.deliver");

    // kb board check lists the new rules; warn never changes the exit code.
    let o = kb_s(&s).args(["board", "check"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let text = stdout(&o);
    for line in [
        "R7  warn   FAIL  no card under discussion is in a doing lane",
        "R8  warn   FAIL  every card in a doing lane with an open question is blocked",
        "R9  warn   FAIL  every answer delivered this turn has a decision note on its card",
        "R10  warn   FAIL  cards moved from todo to doing this turn were the head of their queue",
        "      R7: card 3 \"討論中\" is still under discussion but is in a doing lane.",
        "          Fix: kb card block 2 --reason \"waiting for answer to question",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    let v: Value = serde_json::from_str(&ok(kb_s(&s).args(["--json", "board", "check"]))).unwrap();
    let names: Vec<&str> = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rule"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["R1", "R2", "R3", "R4", "R5", "R7", "R8", "R9", "R10", "R11"]
    );
    assert_eq!(v["rules"][8]["violations"][0]["card"], 2);
    // At level block, R8 makes the check exit 1.
    let block = rules_file(Some("[rules]\nR8 = \"block\"\n"));
    let o = kb_s(&s)
        .env("KB_RULES_FILE", &block)
        .args(["board", "check"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));

    // At the default level (warn) the stop after the block records all four.
    silent(&hook(
        "stop",
        &json!({"session_id": s, "stop_hook_active": true}),
    ));
    let rows = activity(&s);
    assert_eq!(rows[0]["verb"], "turn.summary");
    let rules: Vec<&str> = rows[1..5]
        .iter()
        .map(|a| {
            assert_eq!(a["verb"], "check.warn");
            assert_eq!(a["detail"]["level"], "warn");
            a["detail"]["rule"].as_str().unwrap()
        })
        .collect();
    assert_eq!(rules, ["R10", "R9", "R8", "R7"]);
    assert_eq!(
        rows[1]["detail"]["message"],
        "#2 跳過「待辦」的第一張 #1 就開始"
    );

    // --history: newest first; the rule column is two characters wide, so R10 pushes the
    // rest of its line one character right.
    let history = ok(kb_s(&s).args(["board", "check", "--history"]));
    let tails: Vec<String> = history
        .lines()
        .map(|l| l.split_once("  turn ").map_or("", |(_, t)| t).to_string())
        .collect();
    assert_eq!(tails.len(), 4, "{history}");
    assert_eq!(
        tails[0],
        "r7r10abc  R10  warned        R10: card 2 \"第二張\" moved from \"待辦\" to \"實作\" this \
         turn, but the head of that queue was #1."
    );
    assert!(tails[1].starts_with("r7r10abc  R9  warned        R9: the answer to question"));
    assert!(tails[3].starts_with("r7r10abc  R7  warned        R7: card 3 "));

    // The next real prompt reminds once, in rule-number order (R10 last).
    let o = hook("prompt", &json!({"session_id": s, "prompt": "繼續"}));
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        format!("Board reminders from the last turn:\n{r7}\n{r8}\n{r9}\n{r10}")
    );
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "好"})));

    // Fixed: confirmed, blocked, decided; the new turn has no move. All pass.
    ok(kb_s(&s).args(["card", "confirm", "3"]));
    ok(kb_s(&s).args([
        "card",
        "block",
        "2",
        "--reason",
        &format!("waiting for answer to question {q8}"),
    ]));
    let o = ok(kb_s(&s).args(["board", "check"]));
    for rule in ["R7 ", "R8 ", "R9 ", "R10 "] {
        let line = o.lines().find(|l| l.starts_with(rule)).unwrap();
        assert!(line.contains(" pass "), "{line}");
    }
}

// --- fail open (2.3) ----------------------------------------------------------------------

const EVENTS: [&str; 6] = [
    "session-start",
    "prompt",
    "pre-tool",
    "tool-used",
    "stop",
    "session-end",
];

#[test]
fn hooks_fail_open() {
    migrate();
    for event in EVENTS {
        // Invalid input.
        for input in ["not json", "", "[1, 2]"] {
            let o = run_with_stdin(kb().args(["hook", event]), input);
            assert_eq!(o.status.code(), Some(0), "{event}");
            assert!(o.stdout.is_empty(), "{event}");
            assert_eq!(stderr(&o).trim(), "kb hook: invalid input", "{event}");
        }
        // Missing fields: no session, nothing to do.
        let o = hook(event, &json!({}));
        assert_eq!(o.status.code(), Some(0), "{event}");
        assert!(o.stdout.is_empty(), "{event}");

        // Database unreachable.
        let mut cmd = kb();
        cmd.env(
            "KB_WRITER_URL",
            "postgres://kb_writer:x@127.0.0.1:1/kanban_test",
        );
        let input = json!({"session_id": unique("down"), "tool_name": "Edit", "tool_input": {}});
        let o = hook_with(&mut cmd, event, &input);
        assert_eq!(o.status.code(), Some(0), "{event}");
        assert_eq!(
            stderr(&o).trim(),
            "kb hook: database unreachable",
            "{event}"
        );
        // No context at all, not even for session-start (docs/board-v2.md 12.2).
        assert!(o.stdout.is_empty(), "{event}");

        // No board for the session: silent, nothing written, no board created; also when
        // R1 and R6 block (docs/board-v2.md 12.3).
        let block = rules_file(Some("[rules]\nR1 = \"block\"\nR6 = \"block\"\n"));
        for tool in ["Edit", "AskUserQuestion"] {
            let noboard = unique("noboard");
            let o = hook_with(
                kb().env("KB_RULES_FILE", &block),
                event,
                &json!({"session_id": noboard, "tool_name": tool, "prompt": "x", "source": "startup"}),
            );
            assert_eq!(o.status.code(), Some(0), "{event}");
            assert!(
                o.stdout.is_empty() && o.stderr.is_empty(),
                "{event}: {}",
                stderr(&o)
            );
            let o = kb_s(&noboard).args(["board", "show"]).output().unwrap();
            assert_eq!(o.status.code(), Some(1), "{event} created a board");
        }
    }
    // An unknown event is a syntax error of the command line itself.
    let o = run_with_stdin(kb().args(["hook", "frobnicate"]), "{}");
    assert_eq!(o.status.code(), Some(2));
}

// --- question commands (docs/questions.md 5) ------------------------------------------------

#[test]
fn question_commands() {
    migrate();
    let s = unique("ask");
    ok(kb_s(&s).args([
        "board",
        "init",
        "--title",
        "提問",
        "--workspace",
        &unique("ws"),
        "--lanes",
        "待辦:todo,實作:doing,完成:done",
    ]));
    ok(kb_s(&s).args([
        "card",
        "add",
        "--lane",
        "實作",
        "--title",
        "修正 token 更新時間",
    ]));
    ok(kb_s(&s).args(["card", "add", "--lane", "完成", "--title", "已完成"]));

    let out = ok(kb_s(&s).args([
        "ask",
        "--card",
        "1",
        "token 的有效期要改成多久？",
        "--option",
        "15 分鐘::與 refresh 的間隔一致",
        "--option",
        "30 分鐘::減少更新次數",
        "--option",
        "60 分鐘",
    ]));
    let q: i64 = out
        .trim()
        .strip_prefix("question ")
        .and_then(|r| r.split(' ').next())
        .unwrap()
        .parse()
        .unwrap();
    // The next card has not changed since `board init` printed it: no `next:` line
    // (docs/board-v2.md section 15.2).
    assert_eq!(
        out.trim(),
        format!("question {q} asked on card 1 (3 options)")
    );

    // Errors: option count is a data error; no option at all is a syntax error.
    let o = kb_s(&s)
        .args(["ask", "--card", "1", "q", "--option", "a"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o).trim(), "a question needs 2 to 6 options (got 1)");
    let o = kb_s(&s).args(["ask", "--card", "1", "q"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = kb_s(&s)
        .args(["ask", "--card", "2", "q", "--option", "a", "--option", "b"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o).trim(), "card is closed");

    let show = ok(kb_s(&s).args(["board", "show"]));
    assert!(
        show.contains("  [1] 修正 token 更新時間 (derived)  (questions: 1 open)"),
        "{show}"
    );
    let v = board_json(&s);
    assert_eq!(v["open_question_count"], 1);
    assert_eq!(v["lanes"][1]["cards"][0]["open_question_count"], 1);

    let card = ok(kb_s(&s).args(["card", "show", "1"]));
    let qpos = card.find("questions (1):").expect("questions section");
    assert!(
        qpos < card.find("notes (0):").unwrap(),
        "questions before notes: {card}"
    );
    assert!(
        card.contains(&format!("  question {q} [open] token 的有效期要改成多久？")),
        "{card}"
    );
    assert!(
        card.contains("    [ ] 1. 15 分鐘 - 與 refresh 的間隔一致"),
        "{card}"
    );
    let v: Value =
        serde_json::from_str(&ok(kb_s(&s).args(["--json", "card", "show", "1"]))).unwrap();
    assert_eq!(v["questions"][0]["options"][2]["label"], "60 分鐘");

    let list = ok(kb_s(&s).args(["question", "list", "--open"]));
    assert!(
        list.contains(&format!("[{q}] card 1   open      token")),
        "{list}"
    );

    // Inbox before and after an answer.
    assert_eq!(
        ok(kb_s(&s).arg("inbox")).trim(),
        "inbox is empty\nopen questions: 1"
    );
    answer(q, &option_ids(&s, q)[1..2], false);
    let expected = format!(
        "question {q} on card 1: token 的有效期要改成多久？\n  answer: 30 分鐘\nopen questions: 0"
    );
    assert_eq!(ok(kb_s(&s).args(["inbox", "--peek"])).trim(), expected);
    assert_eq!(ok(kb_s(&s).arg("inbox")).trim(), expected);
    assert_eq!(
        ok(kb_s(&s).arg("inbox")).trim(),
        "inbox is empty\nopen questions: 0"
    );
    let shown = ok(kb_s(&s).args(["question", "show", &q.to_string()]));
    assert!(shown.contains("status: answered"), "{shown}");
    assert!(shown.contains("  [x] 2. 30 分鐘 - 減少更新次數"), "{shown}");
    assert!(!shown.contains("delivered: -"), "{shown}");

    // Cancel.
    let q2 = ask(&s, "1", "第二個問題", &["a", "b"]);
    assert_eq!(
        ok(kb_s(&s).args([
            "question",
            "cancel",
            &q2.to_string(),
            "--reason",
            "不需要了"
        ]))
        .trim(),
        format!("question {q2} cancelled")
    );
    assert_eq!(activity(&s)[0]["detail"]["reason"], "不需要了");
    let o = kb_s(&s)
        .args(["question", "cancel", &q.to_string()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).starts_with("question is not open"));
    let o = kb_s(&s)
        .args(["question", "show", "999999999"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o).trim(), "question not found: 999999999");
}

// --- session key source 4 (docs/hooks.md 5.3) ---------------------------------------------

#[test]
fn session_key_from_terminal_id() {
    migrate();
    let terminal = unique("t");
    let s = unique("term");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut conn = kb_core::db::connect(&test_url(Role::Writer)).await.unwrap();
        let space = unique("space");
        let here = kb_core::resolve::terminal_env(|k| match k {
            "KB_TERMINAL_ID" => Some(terminal.clone()),
            "KB_TERMINAL_SPACE" => Some(space.clone()),
            _ => None,
        });
        let ws = kb_core::resolve::workspace_here(None, here, std::path::Path::new("."));
        let args = kb_core::write::BoardInit {
            title: Some("終端機的看板".into()),
            ..Default::default()
        };
        kb_core::write::board_init(&mut conn, &s, args, || ws)
            .await
            .unwrap();
    });
    let out = ok(kb()
        .env("KB_TERMINAL_ID", &terminal)
        .args(["board", "show"]));
    assert!(out.starts_with("board: 終端機的看板 [active]"), "{out}");
    // Sources 1-3 win over the terminal.
    let o = kb()
        .env("KB_TERMINAL_ID", &terminal)
        .env("KB_SESSION", unique("other"))
        .args(["board", "show"])
        .output()
        .unwrap();
    assert!(
        stderr(&o).starts_with("no board for session"),
        "{}",
        stderr(&o)
    );
    // A terminal without an active board: no session.
    let o = kb()
        .env("KB_TERMINAL_ID", unique("none"))
        .args(["card", "note", "1", "x"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        "no session: set KB_SESSION or pass --session"
    );
}

// --- comments and the waiter (docs/board-v2.md section 13) ------------------------------------

/// Adds a comment as kb_reader through kb-core (what the TUI does); returns the note id.
fn comment(session: &str, card: i32, text: &str) -> i64 {
    let v: Value = serde_json::from_str(&ok(kb_s(session).args([
        "--json",
        "card",
        "show",
        &card.to_string(),
    ])))
    .unwrap();
    let card_id = v["card"]["id"].as_i64().unwrap();
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

const REPLY_LINE: &str = "Reply with: kb card note <n> --kind reply \"<text>\"";

fn board_with_card(prefix: &str) -> String {
    migrate();
    let s = unique(prefix);
    start(&s);
    ok(kb_s(&s).args(["board", "init", "--title", "看板"]));
    ok(kb_s(&s).args(["card", "add", "--lane", "待辦", "--title", "登入逾時"]));
    s
}

#[test]
fn comments_are_delivered_and_reminded_until_replied() {
    let s = board_with_card("comment");
    let c = comment(&s, 1, "這張先不要做，先查 log");
    let delivered = format!(
        "comment {c} on card 1 \"登入逾時\": 這張先不要做，先查 log\n\
         open questions: 0\n\
         1 comment without a reply: card 1. {REPLY_LINE}"
    );
    assert_eq!(ok(kb_s(&s).args(["inbox", "--peek"])).trim_end(), delivered);
    let card = ok(kb_s(&s).args(["card", "show", "1"]));
    assert!(card.contains("comment   這張先不要做，先查 log"), "{card}");
    let v: Value = serde_json::from_str(&ok(kb_s(&s).args(["--json", "board", "show"]))).unwrap();
    assert_eq!(v["unreplied_comment_count"], 1);
    assert_eq!(v["lanes"][0]["cards"][0]["unreplied_comment_count"], 1);

    // The prompt hook delivers it once, then only reminds.
    let o = hook("prompt", &json!({"session_id": s, "prompt": "繼續"}));
    assert_eq!(context(&o, "UserPromptSubmit"), delivered);
    assert_eq!(&verbs(&s)[..2], ["comment.deliver", "user.prompt"]);
    let o = hook("prompt", &json!({"session_id": s, "prompt": "再一次"}));
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        format!("1 comment without a reply: card 1. {REPLY_LINE}")
    );
    assert_eq!(
        ok(kb_s(&s).args(["inbox"])).trim_end(),
        format!(
            "inbox is empty\nopen questions: 0\n1 comment without a reply: card 1. {REPLY_LINE}"
        )
    );

    // kb board check: an informational line, the exit code stays 0.
    let o = kb_s(&s).args(["board", "check"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(
        stdout(&o)
            .trim_end()
            .ends_with("comments: 1 without a reply (card 1)"),
        "{}",
        stdout(&o)
    );
    let v: Value = serde_json::from_str(&ok(kb_s(&s).args(["--json", "board", "check"]))).unwrap();
    assert_eq!(v["unreplied_comments"][0]["id"], c);

    // The agent cannot write a comment; it replies.
    let o = kb_s(&s)
        .args(["card", "note", "1", "--kind", "comment", "x"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o).trim(),
        "comment notes come from the user; reply with --kind reply"
    );
    ok(kb_s(&s).args(["card", "note", "1", "--kind", "reply", "好，先查 log"]));
    silent(&hook("prompt", &json!({"session_id": s, "prompt": "好"})));
    assert!(
        ok(kb_s(&s).args(["board", "check"]))
            .trim_end()
            .ends_with("comments: none without a reply")
    );
}

#[test]
fn stop_blocks_with_undelivered_comments() {
    let s = board_with_card("stopc");
    let c = comment(&s, 1, "先查 log");
    let v = json_out(&hook("stop", &json!({"session_id": s})));
    assert_eq!(v["decision"], "block");
    assert_eq!(
        v["reason"],
        format!(
            "comment {c} on card 1 \"登入逾時\": 先查 log\nopen questions: 0\n\
             1 comment without a reply: card 1. {REPLY_LINE}\n\
             Reply to each comment with kb card note <n> --kind reply \"<text>\", then act on it before stopping."
        )
    );
    // Delivered now; an unreplied comment alone never blocks.
    silent(&hook("stop", &json!({"session_id": s})));
    assert_eq!(activity(&s)[0]["verb"], "turn.summary");
}

#[test]
fn task_notification_prompt_delivers_without_a_new_turn() {
    let s = board_with_card("notify");
    silent(&hook(
        "prompt",
        &json!({"session_id": s, "prompt_id": "p-1", "prompt": "開始"}),
    ));
    let c = comment(&s, 1, "先查 log");
    let wake = "<task-notification>\n<summary>Stop hook feedback</summary>\n</task-notification>\n\
                <system-reminder>\n[kb] 1 new comment on card 1. Run: kb inbox\n</system-reminder>";
    let o = hook(
        "prompt",
        &json!({"session_id": s, "prompt_id": "p-2", "prompt": wake}),
    );
    assert_eq!(
        context(&o, "UserPromptSubmit"),
        format!(
            "comment {c} on card 1 \"登入逾時\": 先查 log\nopen questions: 0\n\
             1 comment without a reply: card 1. {REPLY_LINE}"
        )
    );
    // Not a turn: the turn key stays, no user.prompt.
    assert_eq!(board_json(&s)["board"]["turn_key"], "p-1");
    let v = verbs(&s);
    assert_eq!(&v[..3], ["comment.deliver", "comment.add", "user.prompt"]);
}

#[test]
fn inbox_after_a_wake_lists_what_this_turn_already_delivered() {
    let s = board_with_card("again");
    // No turn yet (no user.prompt): the plain empty inbox.
    assert_eq!(
        ok(kb_s(&s).arg("inbox")).trim_end(),
        "inbox is empty\nopen questions: 0"
    );
    silent(&hook(
        "prompt",
        &json!({"session_id": s, "prompt_id": "p-1", "prompt": "開始"}),
    ));
    let q = ask(&s, "1", "要用哪一種結構？", &["欄位陣列", "物件"]);
    answer(q, &option_ids(&s, q)[0..1], false);
    let c = comment(&s, 1, "先查 log");
    // The wake turn: the prompt hook puts both into the context (docs/board-v2.md 19.2).
    let wake = format!(
        "<task-notification>\n<summary>Stop hook feedback</summary>\n</task-notification>\n\
         <system-reminder>\n[kb] question {q} answered; 1 new comment on card 1. The content is \
         delivered in this turn's context; run \"kb inbox\" only if you do not see it.\n\
         </system-reminder>"
    );
    let o = hook("prompt", &json!({"session_id": s, "prompt": wake}));
    let items = format!(
        "question {q} on card 1: 要用哪一種結構？\n  answer: 欄位陣列\n\
         comment {c} on card 1 \"登入逾時\": 先查 log\nopen questions: 0\n\
         1 comment without a reply: card 1. {REPLY_LINE}"
    );
    assert_eq!(context(&o, "UserPromptSubmit"), items);
    // An agent that runs kb inbox anyway gets the same content, marked as already
    // delivered; nothing is written.
    let rows = activity(&s).len();
    let expected = format!("inbox is empty; already delivered in this turn:\n{items}");
    assert_eq!(ok(kb_s(&s).arg("inbox")).trim_end(), expected);
    assert_eq!(ok(kb_s(&s).args(["inbox", "--peek"])).trim_end(), expected);
    assert_eq!(activity(&s).len(), rows);
    let v: Value = serde_json::from_str(&ok(kb_s(&s).args(["--json", "inbox"]))).unwrap();
    assert_eq!(v["answers"].as_array().unwrap().len(), 0);
    assert_eq!(v["delivered"], false);
    assert_eq!(v["delivered_this_turn"]["answers"][0]["id"], q);
    assert_eq!(v["delivered_this_turn"]["comments"][0]["id"], c);

    // A new prompt starts a new turn: the earlier items are no longer listed.
    let o = hook(
        "prompt",
        &json!({"session_id": s, "prompt_id": "p-2", "prompt": "下一步"}),
    );
    let reminder = format!("1 comment without a reply: card 1. {REPLY_LINE}");
    assert_eq!(context(&o, "UserPromptSubmit"), reminder);
    assert_eq!(
        ok(kb_s(&s).arg("inbox")).trim_end(),
        format!("inbox is empty\nopen questions: 0\n{reminder}")
    );
    let v: Value = serde_json::from_str(&ok(kb_s(&s).args(["--json", "inbox"]))).unwrap();
    assert_eq!(v["delivered_this_turn"]["answers"], json!([]));
    assert_eq!(v["delivered_this_turn"]["comments"], json!([]));

    // What kb inbox itself delivered in this turn is listed by the next kb inbox.
    let c2 = comment(&s, 1, "再加一個測試");
    let new = ok(kb_s(&s).arg("inbox"));
    assert!(new.starts_with(&format!("comment {c2} on card 1")), "{new}");
    let again = ok(kb_s(&s).arg("inbox"));
    assert!(
        again.starts_with(&format!(
            "inbox is empty; already delivered in this turn:\ncomment {c2} on card 1"
        )),
        "{again}"
    );
}

/// Waits for `child` up to `limit`; kills it and fails when it does not end.
fn wait_exit(mut child: std::process::Child, limit: std::time::Duration) -> Output {
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let o = child.wait_with_output().unwrap();
            panic!("kb hook wait did not end: {}", stderr(&o));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn spawn_wait(session: &str, claude_pid: u32) -> std::process::Child {
    let mut child = kb()
        .args(["hook", "wait"])
        .env("CLAUDE_PID", claude_pid.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kb hook wait");
    let input = json!({"session_id": session, "hook_event_name": "Stop"}).to_string();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child
}

#[test]
fn wait_hook_wakes_with_exit_2_and_one_line() {
    use std::time::Duration;
    migrate();
    // No board: exit 0 at once, nothing shown.
    let o = wait_exit(
        spawn_wait(&unique("none"), std::process::id()),
        Duration::from_secs(10),
    );
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{}", stderr(&o));

    let s = board_with_card("wait");
    let me = std::process::id();
    let first = spawn_wait(&s, me);
    std::thread::sleep(Duration::from_millis(1500));
    // A second waiter of the same Claude Code process leaves at once.
    let o = wait_exit(spawn_wait(&s, me), Duration::from_secs(10));
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty(), "{}", stderr(&o));

    let c = comment(&s, 1, "先查 log");
    let o = wait_exit(first, Duration::from_secs(15));
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(o.stdout.is_empty());
    assert_eq!(
        stderr(&o).trim_end(),
        "[kb] 1 new comment on card 1. The content is delivered in this turn's context; run \"kb inbox\" only if you do not see it."
    );
    // The waiter never delivers.
    let inbox = ok(kb_s(&s).args(["inbox", "--peek"]));
    assert!(
        inbox.starts_with(&format!("comment {c} on card 1")),
        "{inbox}"
    );
}
