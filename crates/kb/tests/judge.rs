//! End-to-end tests of `kb hook judge` (docs/hooks.md section 3.7) against a local fake of
//! the Decisions API (std `TcpListener`, canned JSON) and a dummy key. Database:
//! `kanban_test` only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kb_core::{Config, Role};
use serde_json::{Value, json};

const KEY: &str = "dummy-key-0123";
/// The plugin option `judge_api_key` as Claude Code hands it to the plugin's hooks.
const PLUGIN_KEY_ENV: &str = "CLAUDE_PLUGIN_OPTION_JUDGE_API_KEY";
const PLUGIN_KEY: &str = "dummy-plugin-key-4567";

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
        "test-judge-{prefix}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// `kb` against kanban_test with `rules` as the rules file and no judge key.
fn kb(rules: &PathBuf) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_WRITER_URL", test_url(Role::Writer))
        .env("KB_READER_URL", test_url(Role::Reader))
        .env("KB_ADMIN_URL", test_url(Role::Admin))
        .env("KB_RULES_FILE", rules)
        .env_remove("KB_JUDGE_API_KEY")
        .env_remove(PLUGIN_KEY_ENV)
        .env_remove("KB_SESSION")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("HC_TERMINAL_ID")
        .env_remove("CLAUDE_ENV_FILE");
    cmd
}

fn ok(cmd: &mut Command) -> String {
    let o = cmd.output().expect("run kb");
    assert_eq!(o.status.code(), Some(0), "stderr: {}", text(&o.stderr));
    text(&o.stdout)
}

fn text(b: &[u8]) -> String {
    String::from_utf8(b.to_vec()).unwrap()
}

fn run_with_stdin(cmd: &mut Command, input: &Value) -> Output {
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
        .write_all(input.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Exit 0, nothing on stdout or stderr.
fn silent(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "stderr: {}", text(&o.stderr));
    assert!(o.stdout.is_empty(), "stdout: {}", text(&o.stdout));
    assert!(o.stderr.is_empty(), "stderr: {}", text(&o.stderr));
}

// --- the fake model -----------------------------------------------------------------------

/// A canned reply: status, body, delay.
type Reply = (u16, String, Duration);

#[derive(Clone)]
struct Request {
    key: Option<String>,
    body: Value,
}

struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl FakeModel {
    fn start(reply: impl Fn(&Value) -> Reply + Send + Sync + 'static) -> FakeModel {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(reply);
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (seen, reply) = (seen.clone(), reply.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut length = 0usize;
                    let mut key = None;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            match name.trim().to_ascii_lowercase().as_str() {
                                "content-length" => length = value.trim().parse().unwrap(),
                                "x-goog-api-key" => key = Some(value.trim().to_string()),
                                _ => {}
                            }
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    seen.lock().unwrap().push(Request {
                        key,
                        body: body.clone(),
                    });
                    let (status, text, delay) = reply(&body);
                    std::thread::sleep(delay);
                    let mut stream = stream;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    );
                    let _ = stream.flush();
                });
            }
        });
        FakeModel {
            endpoint: format!("http://127.0.0.1:{port}/v1beta/decisions"),
            requests,
        }
    }

    /// Answers every question: "no" (0.01) for statements, the "none" option for choices;
    /// `overrides` replace the answer of a question id.
    fn answering(overrides: Vec<(&'static str, Value)>) -> FakeModel {
        FakeModel::start(move |body| (200, answers(body, &overrides), Duration::ZERO))
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// A rules file pointing the judge at this server.
    fn rules(&self, extra: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{}.toml", unique("rules")));
        std::fs::write(
            &path,
            format!(
                "[judge]\nendpoint = \"{}\"\nsummary_wait_ms = 400\n{extra}",
                self.endpoint
            ),
        )
        .unwrap();
        path
    }
}

fn answers(body: &Value, overrides: &[(&str, Value)]) -> String {
    let mut out = serde_json::Map::new();
    for (id, q) in body["questions"].as_object().unwrap() {
        if let Some((_, v)) = overrides.iter().find(|(k, _)| k == id) {
            out.insert(id.clone(), v.clone());
            continue;
        }
        let a = if q["type"] == "noul" {
            json!({"type": "noul", "noul": 0.01, "confidence": 0.99})
        } else {
            let none = q["criteria"]
                .as_object()
                .unwrap()
                .keys()
                .find(|k| k.starts_with("none") || k.starts_with("no project work"))
                .unwrap()
                .clone();
            json!({"type": "choice", "choice": none, "confidence": 0.99,
                   "probabilities": {none: 0.99}})
        };
        out.insert(id.clone(), a);
    }
    json!({ "answers": out, "usage": {"total_tokens": 1} }).to_string()
}

fn noul(p: f64) -> Value {
    json!({"type": "noul", "noul": p, "confidence": p})
}

fn choice(first: &str, probabilities: Value) -> Value {
    json!({"type": "choice", "choice": first, "confidence": 0.9, "probabilities": probabilities})
}

// --- the board side -------------------------------------------------------------------------

struct Session {
    key: String,
    rules: PathBuf,
}

impl Session {
    /// A started board with lanes and card 1 "登入逾時", and a first prompt.
    fn new(prefix: &str, rules: PathBuf) -> Session {
        let s = Session {
            key: unique(prefix),
            rules,
        };
        s.ok(&["db", "migrate", "--database", "kanban_test", "--yes"]);
        s.ok(&["board", "start", "--new"]);
        s.ok(&["board", "init", "--title", "看板"]);
        s.ok(&["card", "add", "--lane", "待辦", "--title", "登入逾時"]);
        s
    }

    fn cmd(&self) -> Command {
        let mut c = kb(&self.rules);
        c.args(["--session", &self.key]);
        c
    }

    fn ok(&self, args: &[&str]) -> String {
        ok(self.cmd().args(args))
    }

    fn hook(&self, event: &str, input: Value) -> Output {
        let mut c = kb(&self.rules);
        run_with_stdin(c.args(["hook", event]), &input)
    }

    fn prompt(&self, prompt: &str) {
        silent(&self.hook("prompt", json!({"session_id": self.key, "prompt": prompt})));
    }

    /// `kb hook stop`, expected to pass (writes `turn.summary`).
    fn stop(&self) {
        let o = self.hook("stop", json!({"session_id": self.key}));
        assert_eq!(o.status.code(), Some(0));
        assert!(o.stdout.is_empty(), "stop blocked: {}", text(&o.stdout));
    }

    fn judge_cmd(&self, with_key: bool) -> Command {
        let mut c = kb(&self.rules);
        if with_key {
            c.env("KB_JUDGE_API_KEY", KEY);
        }
        c.args(["hook", "judge"]);
        c
    }

    fn judge(&self, message: &str) -> Output {
        run_with_stdin(
            &mut self.judge_cmd(true),
            &json!({"session_id": self.key, "hook_event_name": "Stop",
                    "stop_hook_active": false, "last_assistant_message": message}),
        )
    }

    fn activity(&self) -> Vec<Value> {
        let v: Value =
            serde_json::from_str(&self.ok(&["--json", "activity", "--limit", "100"])).unwrap();
        v.as_array().unwrap().clone()
    }

    /// Rows of `verb`, newest first.
    fn rows(&self, verb: &str) -> Vec<Value> {
        self.activity()
            .into_iter()
            .filter(|a| a["verb"] == verb)
            .collect()
    }

    fn card_id(&self, seq: i32) -> i64 {
        let v: Value =
            serde_json::from_str(&self.ok(&["--json", "card", "show", &seq.to_string()])).unwrap();
        v["card"]["id"].as_i64().unwrap()
    }
}

// --- tests ----------------------------------------------------------------------------------

#[test]
fn without_a_key_or_a_message_nothing_happens() {
    let model = FakeModel::answering(vec![("advice_1", noul(0.9))]);
    let s = Session::new("nokey", model.rules(""));
    s.prompt("看一下登入逾時");
    s.stop();
    let input = json!({"session_id": s.key, "last_assistant_message": "建議先不要做 #1"});
    silent(&run_with_stdin(&mut s.judge_cmd(false), &input));
    // An empty key counts as none.
    let mut c = s.judge_cmd(false);
    c.env("KB_JUDGE_API_KEY", "  ");
    silent(&run_with_stdin(&mut c, &input));
    // No final message.
    silent(&run_with_stdin(
        &mut s.judge_cmd(true),
        &json!({"session_id": s.key}),
    ));
    // Disabled in the rules file.
    let off = Session {
        key: s.key.clone(),
        rules: model.rules("enabled = false\n"),
    };
    silent(&off.judge("建議先不要做 #1"));
    // Not JSON, no session: silent too.
    let o = run_with_stdin(&mut s.judge_cmd(true), &json!("x"));
    silent(&o);
    silent(&run_with_stdin(
        &mut s.judge_cmd(true),
        &json!({"last_assistant_message": "x"}),
    ));
    assert!(model.requests().is_empty());
    assert!(s.rows("judge.run").is_empty());
}

/// The key comes from the plugin option first, then from `[judge] api_key_env`; an empty
/// value counts as unset.
#[test]
fn the_plugin_option_key_comes_before_the_environment_variable() {
    let model = FakeModel::answering(vec![]);
    let s = Session::new("pluginkey", model.rules(""));
    let input = json!({"session_id": s.key, "last_assistant_message": "好了。"});
    let judge_with = |plugin: Option<&str>, env: Option<&str>| {
        let mut c = s.judge_cmd(false);
        if let Some(k) = plugin {
            c.env(PLUGIN_KEY_ENV, k);
        }
        if let Some(k) = env {
            c.env("KB_JUDGE_API_KEY", k);
        }
        silent(&run_with_stdin(&mut c, &input));
    };
    let keys = || -> Vec<Option<String>> {
        model.requests().into_iter().map(|r| r.key).collect()
    };

    // Neither set (empty values count as unset): nothing is asked or written.
    s.prompt("繼續");
    s.stop();
    judge_with(None, None);
    judge_with(Some(" "), Some(""));
    assert!(model.requests().is_empty());
    assert!(s.rows("judge.run").is_empty());

    // Only the plugin option.
    judge_with(Some(PLUGIN_KEY), None);
    assert_eq!(keys(), [Some(PLUGIN_KEY.to_string())]);
    assert_eq!(s.rows("judge.run").len(), 1);

    // Both set: the plugin option wins.
    s.prompt("繼續");
    s.stop();
    judge_with(Some(PLUGIN_KEY), Some(KEY));
    assert_eq!(keys()[1].as_deref(), Some(PLUGIN_KEY));

    // An empty plugin option falls back to the api_key_env variable.
    s.prompt("繼續");
    s.stop();
    judge_with(Some("  "), Some(KEY));
    assert_eq!(keys()[2].as_deref(), Some(KEY));
    assert_eq!(s.rows("judge.run").len(), 3);
}

#[test]
fn a_turn_that_did_not_end_is_not_judged() {
    let model = FakeModel::answering(vec![]);
    let s = Session::new("nosummary", model.rules(""));
    s.prompt("看一下登入逾時");
    // kb hook stop did not run (or blocked): no turn.summary after the prompt. The
    // turn.summary of kb board tidy (written by the agent mid-turn) is not a turn end.
    s.ok(&["board", "tidy"]);
    silent(&s.judge("我看完了。"));
    // A stop that blocks to deliver a comment writes no turn.summary either.
    let card = s.card_id(1);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut reader = kb_core::db::connect(&test_url(Role::Reader)).await.unwrap();
        kb_core::comment::add_comment(&mut reader, card, "先查 log")
            .await
            .unwrap();
    });
    let o = s.hook("stop", json!({"session_id": s.key}));
    assert!(text(&o.stdout).contains("\"decision\""), "{}", text(&o.stdout));
    silent(&s.judge("我看完了。"));
    assert!(model.requests().is_empty());
    assert!(s.rows("judge.run").is_empty());
    assert!(s.rows("judge.finding").is_empty());
}

#[test]
fn a_record_band_finding_is_written_silently() {
    let model = FakeModel::answering(vec![("advice_1", noul(0.5))]);
    let s = Session::new("record", model.rules(""));
    s.prompt("登入逾時這張要不要做？");
    s.ok(&["card", "add", "--lane", "待辦", "--title", "加測試"]);
    s.ok(&["card", "note", "2", "--kind", "decision", "先寫測試"]);
    s.stop();
    silent(&s.judge("我建議 #1 先不要做，等 log 再說。"));

    // The request: the key in its header, one call with every question.
    let reqs = model.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].key.as_deref(), Some(KEY));
    let body = &reqs[0].body;
    assert_eq!(body["model"], "gemini-decision-1");
    let mut ids: Vec<&String> = body["questions"].as_object().unwrap().keys().collect();
    ids.sort();
    // Card 2 was written this turn: no advice question for it.
    assert_eq!(
        ids,
        ["advice_1", "offcard_work", "waiting", "waiting_which"]
    );
    assert_eq!(
        body["questions"]["waiting_which"]["criteria"],
        json!({"#1 登入逾時": "", "#2 加測試": "",
               "none — no card waits on the user": "The final message does not end by asking \
               the user for a choice or information that a card depends on; it ends with a \
               report, an answer, or an offer to do more."})
    );
    let input = body["input"].as_str().unwrap();
    assert_eq!(
        input,
        "User message that started the turn:\n登入逾時這張要不要做？\n\n\
         Tools the agent used this turn (other than board commands):\n- none\n\n\
         Board commands (kb) the agent ran this turn:\n\
         - card.add #2 \"加測試\" in 待辦\n- card.note #2 (decision): 先寫測試\n\n\
         Agent's final message of the turn:\n<message>\n我建議 #1 先不要做，等 log 再說。\n</message>"
    );
    // kb hook prompt stores the prompt text for this (detail.text).
    let prompt = &s.rows("user.prompt")[0];
    assert_eq!(prompt["detail"]["text"], "登入逾時這張要不要做？");
    assert_eq!(prompt["detail"]["excerpt"], "登入逾時這張要不要做？");

    // The rows: one finding on card 1, one run; the turn key of the board.
    let findings = s.rows("judge.finding");
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f["actor"], "hook");
    assert_eq!(f["card_id"], s.card_id(1));
    let turn_key = prompt["turn_key"].clone();
    assert_eq!(f["turn_key"], turn_key);
    assert_eq!(
        f["detail"],
        json!({
            "question": "advice", "p": 0.5, "band": "record", "woke": false,
            "text": "card #1 \"登入逾時\": your last message recommends or decides something about \
                     it, but nothing was recorded on the card. If so, record it: kb card note 1 \
                     --kind decision \"<text>\"",
            "message": "#1 最後的回覆對這張卡片提出建議或決定，卡片上沒有紀錄",
        })
    );
    let run = &s.rows("judge.run")[0];
    assert_eq!(run["actor"], "hook");
    assert_eq!(run["card_id"], Value::Null);
    let d = &run["detail"];
    assert_eq!((d["questions"].as_i64(), d["findings"].as_i64()), (Some(4), Some(1)));
    assert_eq!(d["model"], "gemini-decision-1");
    assert_eq!(d["input_chars"].as_u64(), Some(input.chars().count() as u64));
    assert!(d["ms"].is_u64(), "{d}");
    assert!(d.get("error").is_none() && d.get("left_out").is_none(), "{d}");

    // Readable in kb activity.
    let out = s.ok(&["activity", "--limit", "2"]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].contains("  judge.run     4 questions, 1 finding, "),
        "{out}"
    );
    assert!(lines[0].ends_with(" ms, gemini-decision-1"), "{out}");
    assert!(
        lines[1].contains("judge.finding advice p=0.50 (record): card #1 \"登入逾時\": your last"),
        "{out}"
    );

    // The same summary is never judged twice.
    silent(&s.judge("我建議 #1 先不要做。"));
    assert_eq!(model.requests().len(), 1);
}

#[test]
fn a_wake_band_finding_wakes_once_per_turn() {
    let model = FakeModel::answering(vec![("advice_1", noul(0.8))]);
    let s = Session::new("wake", model.rules(""));
    s.prompt("繼續");
    s.stop();
    let o = s.judge("結論：#1 改成只查 log。");
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    assert_eq!(
        text(&o.stderr).trim_end(),
        "[kb] card #1 \"登入逾時\": your last message recommends or decides something about it, \
         but nothing was recorded on the card. If so, record it: kb card note 1 --kind decision \
         \"<text>\""
    );
    let f = &s.rows("judge.finding")[0];
    assert_eq!((&f["detail"]["band"], &f["detail"]["woke"]), (&json!("wake"), &json!(true)));

    // The woken agent continues in the same turn (a task notification starts no turn) and
    // stops again: recorded, but no second wake.
    s.hook(
        "prompt",
        json!({"session_id": s.key, "prompt": "<task-notification>\n[kb] card #1</task-notification>"}),
    );
    s.stop();
    silent(&s.judge("好的，#1 我再想想。"));
    let findings = s.rows("judge.finding");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0]["turn_key"], findings[1]["turn_key"]);
    assert_eq!(
        (&findings[0]["detail"]["band"], &findings[0]["detail"]["woke"]),
        (&json!("wake"), &json!(false))
    );
    assert_eq!(s.rows("judge.run").len(), 2);
    // The second segment starts at the first judge.run: no user message.
    let reqs = model.requests();
    assert!(
        reqs[1].body["input"]
            .as_str()
            .unwrap()
            .starts_with(
                "User message that started the turn:\n(no user message: the agent continued \
                 after a background notification)\n\n"
            ),
        "{}",
        reqs[1].body["input"]
    );

    // A new turn may wake again.
    s.prompt("下一步");
    s.stop();
    let o = s.judge("#1 建議拆成兩張。");
    assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
}

#[test]
fn provider_errors_are_recorded_without_output() {
    // An HTTP error.
    let model = FakeModel::start(|_| {
        (
            500,
            json!({"error": {"message": "internal"}}).to_string(),
            Duration::ZERO,
        )
    });
    let s = Session::new("http", model.rules(""));
    s.prompt("繼續");
    s.stop();
    silent(&s.judge("好了。"));
    let run = &s.rows("judge.run")[0];
    assert_eq!(run["detail"]["error"], "HTTP 500: internal");
    assert_eq!(run["detail"]["findings"], 0);
    assert!(s.rows("judge.finding").is_empty());
    assert!(!run.to_string().contains(KEY));

    // A timeout.
    let slow = FakeModel::start(|b| (200, answers(b, &[]), Duration::from_millis(2500)));
    let s = Session::new("timeout", slow.rules("timeout_ms = 300\n"));
    s.prompt("繼續");
    s.stop();
    silent(&s.judge("好了。"));
    assert_eq!(s.rows("judge.run")[0]["detail"]["error"], "timeout");

    // An answer that is not JSON; and nothing listening.
    let bad = FakeModel::start(|_| (200, "<html>".to_string(), Duration::ZERO));
    let s = Session::new("bad", bad.rules(""));
    s.prompt("繼續");
    s.stop();
    silent(&s.judge("好了。"));
    assert_eq!(
        s.rows("judge.run")[0]["detail"]["error"],
        "unparsable answer: not JSON"
    );
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let rules = std::env::temp_dir().join(format!("{}.toml", unique("rules")));
    std::fs::write(
        &rules,
        format!(
            "[judge]\nendpoint = \"http://127.0.0.1:{closed}/x\"\nsummary_wait_ms = 400\n"
        ),
    )
    .unwrap();
    let s = Session::new("closed", rules);
    s.prompt("繼續");
    s.stop();
    silent(&s.judge("好了。"));
    let e = s.rows("judge.run")[0]["detail"]["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(e.starts_with("request failed"), "{e}");
    assert!(!e.contains(KEY));
}

#[test]
fn waiting_respects_what_the_board_already_shows() {
    let waits_on = |which: &'static str| {
        FakeModel::answering(vec![
            ("waiting", noul(0.9)),
            ("waiting_which", choice(which, json!({which: 0.9}))),
        ])
    };
    let none = "none — no card waits on the user";

    // A card already blocked: no finding.
    let model = waits_on("#1 登入逾時");
    let s = Session::new("blocked", model.rules(""));
    s.prompt("繼續");
    s.ok(&["card", "block", "1", "--reason", "等使用者"]);
    s.stop();
    silent(&s.judge("要用哪一種結構？"));
    assert!(s.rows("judge.finding").is_empty());
    assert_eq!(s.rows("judge.run")[0]["detail"]["findings"], 0);

    // A card with an open question: no finding; and "none" with an open question on the
    // board: no finding either.
    for which in ["#1 登入逾時", none] {
        let model = waits_on(which);
        let s = Session::new("asked", model.rules(""));
        s.prompt("繼續");
        s.ok(&["ask", "--card", "1", "要用哪一種結構？", "--option", "A", "--option", "B"]);
        s.stop();
        silent(&s.judge("我在看板上問了：要用哪一種結構？"));
        assert!(s.rows("judge.finding").is_empty(), "{which}");
    }

    // A card that does not show it: a finding on it (record only by default).
    let model = waits_on("#1 登入逾時");
    let s = Session::new("cardwait", model.rules(""));
    s.prompt("繼續");
    s.stop();
    silent(&s.judge("要用哪一種結構？"));
    let f = &s.rows("judge.finding")[0];
    assert_eq!(f["card_id"], s.card_id(1));
    assert_eq!(f["detail"]["question"], "waiting");
    assert_eq!(f["detail"]["band"], "record");

    // "none" on a board without questions or blocked cards: a board-level finding; with
    // the wake threshold lowered it wakes.
    let model = waits_on(none);
    let s = Session::new("boardwait", model.rules("[judge.waiting]\nwake = 0.7\n"));
    s.prompt("繼續");
    s.stop();
    let o = s.judge("你想先做哪一個？");
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).starts_with(
        "[kb] your last message ends with a question for the user, but the board shows no open \
         question and no blocked card."
    ));
    let f = &s.rows("judge.finding")[0];
    assert_eq!(f["card_id"], Value::Null);
    assert_eq!(f["detail"]["message"], "最後的回覆在等使用者回答，看板上沒有提問，也沒有卡片標為阻擋");
}

#[test]
fn a_second_judge_for_the_same_summary_writes_nothing() {
    let model = FakeModel::start(|b| (200, answers(b, &[]), Duration::from_millis(1200)));
    let s = Session::new("twice", model.rules(""));
    s.prompt("繼續");
    s.stop();
    let input = json!({"session_id": s.key, "last_assistant_message": "好了。"});
    let spawn = || {
        let mut c = s.judge_cmd(true);
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        child
    };
    let (a, b) = (spawn(), spawn());
    silent(&a.wait_with_output().unwrap());
    silent(&b.wait_with_output().unwrap());
    assert_eq!(model.requests().len(), 1);
    assert_eq!(s.rows("judge.run").len(), 1);
}

#[test]
fn offcard_and_unconfirmed_work_are_recorded() {
    let model = FakeModel::answering(vec![
        (
            "offcard_work",
            choice(
                "project work that no listed card covers",
                json!({"project work that no listed card covers": 0.82,
                       "no project work this turn": 0.1}),
            ),
        ),
        ("unconfirmed_work_2", noul(0.6)),
    ]);
    let s = Session::new("offcard", model.rules(""));
    s.ok(&["card", "add", "--lane", "待辦", "--title", "日期欄", "--discuss"]);
    s.prompt("順便把 README 改一下");
    s.hook(
        "tool-used",
        json!({"session_id": s.key, "tool_name": "Edit",
               "tool_input": {"file_path": "/p/README.md"}, "cwd": "/p"}),
    );
    s.stop();
    silent(&s.judge("README 改好了。"));
    let input = model.requests()[0].body["input"].as_str().unwrap().to_string();
    assert!(input.contains("\n- Edit README.md\n"), "{input}");
    let findings = s.rows("judge.finding");
    let topics: Vec<&str> = findings
        .iter()
        .map(|f| f["detail"]["question"].as_str().unwrap())
        .collect();
    // Newest first: offcard_work was written before unconfirmed_work.
    assert_eq!(topics, ["unconfirmed_work", "offcard_work"]);
    assert_eq!(findings[1]["card_id"], Value::Null);
    assert_eq!(findings[1]["detail"]["p"], 0.82);
    assert_eq!(findings[0]["card_id"], s.card_id(2));
    assert_eq!(findings[0]["detail"]["band"], "record");
}

/// The active findings the TUI reads (docs/tui.md section 12.5), through the reader role.
fn active(s: &Session) -> Vec<(Option<i64>, kb_core::read::JudgeTag)> {
    let board: Value = serde_json::from_str(&s.ok(&["--json", "board", "show"])).unwrap();
    let board_id = board["board"]["id"].as_i64().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut reader = kb_core::db::connect(&test_url(Role::Reader)).await.unwrap();
        kb_core::read::active_judge_findings(&mut reader, board_id)
            .await
            .unwrap()
            .into_iter()
            .map(|f| (f.card_id, f.tag))
            .collect()
    })
}

/// Findings are shown to the human in the TUI only: `kb board show` and `kb card show`
/// (and their `--json`) do not print them (docs/tui.md section 12). The next user message
/// settles a `waiting` finding; an agent write on the card settles an `advice` finding.
#[test]
fn findings_stay_out_of_the_agent_views_and_settle_in_the_tui() {
    use kb_core::read::JudgeTag;
    let which = "#1 登入逾時";
    let model = FakeModel::answering(vec![
        ("advice_1", noul(0.5)),
        ("waiting", noul(0.9)),
        ("waiting_which", choice(which, json!({which: 0.9}))),
    ]);
    let s = Session::new("views", model.rules(""));
    s.prompt("登入逾時怎麼處理？");
    s.stop();
    silent(&s.judge("我建議 #1 先只查 log。要我改成這樣嗎？"));
    let findings = s.rows("judge.finding");
    assert_eq!(findings.len(), 2);

    let card = Some(s.card_id(1));
    assert_eq!(
        active(&s),
        [(card, JudgeTag::Waiting), (card, JudgeTag::Advice)]
    );

    for args in [
        &["board", "show"][..],
        &["--json", "board", "show"],
        &["card", "show", "1"],
        &["--json", "card", "show", "1"],
    ] {
        let out = s.ok(args);
        for f in &findings {
            let message = f["detail"]["message"].as_str().unwrap();
            let text = f["detail"]["text"].as_str().unwrap();
            assert!(!out.contains(message), "{args:?}: {out}");
            assert!(!out.contains(text), "{args:?}: {out}");
        }
        // (Not plain "judge": the worktree path may contain it.)
        assert!(!out.contains("judge."), "{args:?}: {out}");
        assert!(!out.contains("finding"), "{args:?}: {out}");
    }

    // The agent writes on the card: the advice is settled, the waiting is not.
    s.ok(&["card", "note", "1", "--kind", "decision", "先只查 log"]);
    assert_eq!(active(&s), [(card, JudgeTag::Waiting)]);
    // The user replies in the chat: the waiting is settled.
    s.prompt("好，就這樣");
    assert!(active(&s).is_empty());
}
