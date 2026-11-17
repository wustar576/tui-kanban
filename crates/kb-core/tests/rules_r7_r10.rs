//! Rules R7-R10 against `kanban_test` (docs/board-v2.md section 17): each rule failing and
//! passing, the levels, and what the stop hook records.

mod common;

use std::path::Path;

use kb_core::hook::{self, Stop};
use kb_core::model::{Board, LinkKind, NoteKind, Origin};
use kb_core::read;
use kb_core::resolve;
use kb_core::rules::{self, Level, Levels, RuleId, RuleResult};
use kb_core::write::{self, BoardInit, CardAdd, LinkTarget, OptionSpec};
use kb_core::{PgConnection, answer};
use serde_json::{Value, json};

const LANES: &str = "待辦:todo,實作:doing,完成:done";

async fn new_board(conn: &mut PgConnection) -> String {
    let session = common::unique("r7-10");
    let args = BoardInit {
        title: Some("新守則".into()),
        goal: None,
        lanes: Some(write::parse_lanes(LANES).unwrap()),
        epics: None,
        cwd: None,
    };
    let ws = resolve::resolve_workspace(Some(&common::unique("ws")), None, Path::new("."));
    write::board_init(conn, &session, args, || ws)
        .await
        .unwrap();
    session
}

async fn board(conn: &mut PgConnection, session: &str) -> Board {
    read::board_by_session(conn, session)
        .await
        .unwrap()
        .unwrap()
}

async fn add(conn: &mut PgConnection, session: &str, lane: &str, title: &str) -> i32 {
    write::card_add(conn, session, CardAdd::new(lane, title))
        .await
        .unwrap()
        .card
        .unwrap()
}

async fn add_discussing(conn: &mut PgConnection, session: &str, lane: &str, title: &str) -> i32 {
    let mut args = CardAdd::new(lane, title);
    args.origin = Origin::Discussing;
    write::card_add(conn, session, args)
        .await
        .unwrap()
        .card
        .unwrap()
}

async fn start_turn(conn: &mut PgConnection, session: &str, key: &str) {
    hook::prompt(conn, session, Some(key), "繼續")
        .await
        .unwrap()
        .expect("board exists");
}

/// Ends the current turn: a stop that passes (every rule off) writes the turn.summary
/// that starts the next turn for the rules (docs/hooks.md section 2.4).
async fn end_turn(conn: &mut PgConnection, session: &str) {
    let mut off = Levels::default();
    for r in RuleId::ALL {
        off.set(r, Level::Off);
    }
    assert!(matches!(
        hook::stop(conn, session, &off).await.unwrap(),
        Stop::Pass { .. }
    ));
}

async fn mv(conn: &mut PgConnection, session: &str, seq: i32, to: &str) -> Vec<String> {
    write::card_move(conn, session, seq, to, None)
        .await
        .unwrap()
        .warnings
}

async fn ask(conn: &mut PgConnection, session: &str, seq: i32, text: &str) -> i64 {
    let opts: Vec<OptionSpec> = ["要", "不要"]
        .iter()
        .map(|l| write::parse_option(l).unwrap())
        .collect();
    write::question_ask(conn, session, seq, text, opts, false)
        .await
        .unwrap()
        .question
        .unwrap()
}

/// Answers as kb_reader (what the TUI does): the first option, or "explain in the session".
async fn answer(conn: &mut PgConnection, board_id: i64, question: i64, chat: bool) {
    let first = read::question(conn, board_id, question)
        .await
        .unwrap()
        .unwrap()
        .options[0]
        .id;
    let mut reader = common::reader().await;
    let ids = if chat { vec![] } else { vec![first] };
    answer::answer_question(&mut reader, question, &ids, chat, None)
        .await
        .unwrap();
}

fn only(rule: RuleId, level: Level) -> Levels {
    let mut levels = Levels::default();
    for r in RuleId::ALL {
        levels.set(r, Level::Off);
    }
    levels.set(rule, level);
    levels
}

async fn check(conn: &mut PgConnection, session: &str, rule: RuleId) -> RuleResult {
    let b = board(conn, session).await;
    rules::check_turn(conn, &b, &Levels::default())
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.rule == rule)
        .unwrap()
}

/// `(card, text, fix, message)` of each violation.
fn violations(r: &RuleResult) -> Vec<(Option<i32>, String, String, String)> {
    r.violations
        .iter()
        .map(|v| (v.card, v.text.clone(), v.fix.clone(), v.message.clone()))
        .collect()
}

async fn last_rows(conn: &mut PgConnection, board_id: i64, n: i64) -> Vec<(String, Value)> {
    read::recent_activity(conn, board_id, n)
        .await
        .unwrap()
        .into_iter()
        .map(|a| (a.verb, a.detail))
        .collect()
}

#[tokio::test]
async fn check_turn_lists_r7_to_r10_after_r2_to_r5() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let results = rules::check_turn(&mut conn, &b, &Levels::default())
        .await
        .unwrap();
    let ids: Vec<RuleId> = results.iter().map(|r| r.rule).collect();
    use RuleId::*;
    assert_eq!(ids, [R2, R3, R4, R5, R7, R8, R9, R10, R11]);
    for r in &results[4..] {
        assert_eq!(r.level, Level::Warn, "{:?}", r.rule);
        assert!(r.passed, "{:?}", r.rule);
    }
    // R9 and R10 need a turn, like R3 and R5; R7, R8 and R11 do not.
    let checked: Vec<bool> = results[4..].iter().map(|r| r.checked).collect();
    assert_eq!(checked, [true, true, false, false, true]);
    assert_eq!(
        rules::check_board(&mut conn, &b, &Levels::default())
            .await
            .unwrap()
            .len(),
        10
    );
}

#[tokio::test]
async fn r7_discussing_card_in_a_doing_lane() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let d = add_discussing(&mut conn, &session, "待辦", "還在討論的功能").await;
    let agreed = add(&mut conn, &session, "待辦", "已約定").await;
    // In a todo lane it is fine.
    assert!(check(&mut conn, &session, RuleId::R7).await.passed);

    // The move succeeds with the immediate echo (section 17.5); an agreed card has none.
    assert_eq!(
        mv(&mut conn, &session, d, "實作").await,
        [format!(
            "R7: card {d} is still under discussion; run \"kb card confirm {d}\" once the \
             user agreed, or move it back"
        )]
    );
    assert!(mv(&mut conn, &session, agreed, "實作").await.is_empty());

    // Checked without a turn: it is the board's state.
    let r = check(&mut conn, &session, RuleId::R7).await;
    assert!(r.checked && !r.passed);
    assert_eq!(
        violations(&r),
        [(
            Some(d),
            format!(
                "R7: card {d} \"還在討論的功能\" is still under discussion but is in a doing lane."
            ),
            format!(
                "kb card confirm {d}  (once the user agreed; otherwise move it back: \
                 kb card move {d} --to <lane>)"
            ),
            format!("#{d} 還在討論中，卻在進行中的泳道"),
        )]
    );

    // Default level warn: the stop passes and records check.warn.
    let b = board(&mut conn, &session).await;
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    let rows = last_rows(&mut conn, b.id, 2).await;
    assert_eq!(rows[0].0, "turn.summary");
    assert_eq!(rows[1].0, "check.warn");
    assert_eq!(rows[1].1["rule"], "R7");
    assert_eq!(
        rows[1].1["message"],
        format!("#{d} 還在討論中，卻在進行中的泳道")
    );

    // Off: nothing recorded. Block: the stop blocks.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R7, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].0, "turn.summary");
    match hook::stop(&mut conn, &session, &only(RuleId::R7, Level::Block))
        .await
        .unwrap()
    {
        Stop::Block(results) => assert_eq!(
            rules::block_text(&results, rules::STOP_HEADER),
            format!(
                "Kanban rules failed. Fix them, then stop again.\n\
                 R7: card {d} \"還在討論的功能\" is still under discussion but is in a doing lane.\n    \
                 Fix: kb card confirm {d}  (once the user agreed; otherwise move it back: \
                 kb card move {d} --to <lane>)"
            )
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "check.block");

    // Confirmed: passes.
    write::card_set_origin(&mut conn, &session, &[d], Origin::Agreed)
        .await
        .unwrap();
    assert!(check(&mut conn, &session, RuleId::R7).await.passed);
}

#[tokio::test]
async fn r8_open_question_on_an_unblocked_doing_card() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let card = add(&mut conn, &session, "實作", "串接付款 API").await;
    let todo = add(&mut conn, &session, "待辦", "之後再做").await;
    // A question on a todo card is not checked.
    ask(&mut conn, &session, todo, "先做哪個？").await;
    assert!(check(&mut conn, &session, RuleId::R8).await.passed);

    let q1 = ask(&mut conn, &session, card, "要支援退款嗎？").await;
    let r = check(&mut conn, &session, RuleId::R8).await;
    assert!(r.checked && !r.passed, "checked without a turn");
    assert_eq!(
        violations(&r),
        [(
            Some(card),
            format!(
                "R8: card {card} \"串接付款 API\" is in a doing lane with open question {q1} but is not blocked."
            ),
            format!("kb card block {card} --reason \"waiting for answer to question {q1}\""),
            format!("#{card} 有未回答的提問 {q1}，卻沒有標記阻擋"),
        )]
    );
    // Two open questions: both named; the fix uses the first.
    let q2 = ask(&mut conn, &session, card, "幣別？").await;
    let r = check(&mut conn, &session, RuleId::R8).await;
    assert_eq!(
        violations(&r)[0].1,
        format!(
            "R8: card {card} \"串接付款 API\" is in a doing lane with open questions {q1}, {q2} but is not blocked."
        )
    );
    assert_eq!(
        violations(&r)[0].2,
        format!("kb card block {card} --reason \"waiting for answer to question {q1}\"")
    );
    assert_eq!(
        violations(&r)[0].3,
        format!("#{card} 有未回答的提問 {q1}、{q2}，卻沒有標記阻擋")
    );

    // Off records nothing; warn records check.warn.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R8, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "turn.summary");
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].0, "question.ask");
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].1["rule"], "R8");

    // Blocked with a reason: passes.
    write::card_block(&mut conn, &session, card, "waiting for answer to question")
        .await
        .unwrap();
    assert!(check(&mut conn, &session, RuleId::R8).await.passed);
    write::card_unblock(&mut conn, &session, card)
        .await
        .unwrap();
    assert!(!check(&mut conn, &session, RuleId::R8).await.passed);

    // Blocked by an unfinished card through a `blocks` link: passes; once that card is
    // done, it is not blocked any more.
    let blocker = add(&mut conn, &session, "待辦", "先做的事").await;
    write::link_add(
        &mut conn,
        &session,
        blocker,
        LinkKind::Blocks,
        LinkTarget::Card(card),
        None,
    )
    .await
    .unwrap();
    assert!(check(&mut conn, &session, RuleId::R8).await.passed);
    write::card_done(&mut conn, &session, blocker, "完成")
        .await
        .unwrap();
    assert!(!check(&mut conn, &session, RuleId::R8).await.passed);

    // Answered (also "explain in the session"): no open question left.
    answer(&mut conn, b.id, q1, false).await;
    answer(&mut conn, b.id, q2, true).await;
    assert!(check(&mut conn, &session, RuleId::R8).await.passed);
}

#[tokio::test]
async fn r9_delivered_answers_need_a_decision_note() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let card = add(&mut conn, &session, "實作", "設定檔格式").await;
    let other = add(&mut conn, &session, "實作", "另一張").await;

    // No user.prompt yet: not checked, even with a delivered answer.
    let q0 = ask(&mut conn, &session, card, "用 TOML 嗎？").await;
    answer(&mut conn, b.id, q0, false).await;
    write::inbox(&mut conn, &session, false).await.unwrap();
    let r = check(&mut conn, &session, RuleId::R9).await;
    assert!(!r.checked && r.passed);

    // The turn ends; in the next one that delivery belongs to the previous turn.
    end_turn(&mut conn, &session).await;
    start_turn(&mut conn, &session, "t1").await;
    assert!(check(&mut conn, &session, RuleId::R9).await.passed);

    // An answered question and a chat answer are delivered this turn; a comment too.
    let q1 = ask(&mut conn, &session, card, "要保留舊格式嗎？").await;
    let q2 = ask(&mut conn, &session, other, "怎麼命名？").await;
    answer(&mut conn, b.id, q1, false).await;
    answer(&mut conn, b.id, q2, true).await;
    let card_id: i64 = sqlx::query_scalar("SELECT id FROM card WHERE board_id = $1 AND seq = $2")
        .bind(b.id)
        .bind(other)
        .fetch_one(&mut conn)
        .await
        .unwrap();
    let mut reader = common::reader().await;
    kb_core::comment::add_comment(&mut reader, card_id, "名稱用英文")
        .await
        .unwrap();
    let inbox = write::inbox(&mut conn, &session, false).await.unwrap();
    assert_eq!((inbox.answers.len(), inbox.comments.len()), (2, 1));

    // Only the answered question fails: chat answers and comments are not part of R9.
    let r = check(&mut conn, &session, RuleId::R9).await;
    assert_eq!(
        violations(&r),
        [(
            Some(card),
            format!(
                "R9: the answer to question {q1} on card {card} \"設定檔格式\" was delivered \
                 this turn, but the card has no decision note since."
            ),
            format!("kb card note {card} --kind decision \"<what was decided>\""),
            format!("#{card} 收到提問 {q1} 的回答，但沒有記下決定"),
        )]
    );

    // Block through the stop hook. A block writes no turn.summary: the turn goes on.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R9, Level::Block))
            .await
            .unwrap(),
        Stop::Block(_)
    ));
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].0, "check.block");

    // A note of another kind, or on another card, does not count.
    write::card_note(&mut conn, &session, card, NoteKind::Progress, "改好了")
        .await
        .unwrap();
    write::card_note(&mut conn, &session, other, NoteKind::Decision, "用英文")
        .await
        .unwrap();
    assert!(!check(&mut conn, &session, RuleId::R9).await.passed);
    // A decision note after the delivery fixes it.
    write::card_note(&mut conn, &session, card, NoteKind::Decision, "保留舊格式")
        .await
        .unwrap();
    assert!(check(&mut conn, &session, RuleId::R9).await.passed);
    end_turn(&mut conn, &session).await;

    // An answer the prompt hook delivers at the start of a turn belongs to that turn; a
    // decision note written before the delivery does not count.
    let q3 = ask(&mut conn, &session, card, "預設值？").await;
    answer(&mut conn, b.id, q3, false).await;
    start_turn(&mut conn, &session, "t2").await;
    let r = check(&mut conn, &session, RuleId::R9).await;
    assert_eq!(violations(&r).len(), 1);
    assert!(
        violations(&r)[0]
            .1
            .starts_with(&format!("R9: the answer to question {q3} "))
    );
    // Off records nothing.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R9, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].0, "question.deliver");

    // Warn records check.warn.
    let q4 = ask(&mut conn, &session, card, "逾時多久？").await;
    answer(&mut conn, b.id, q4, false).await;
    start_turn(&mut conn, &session, "t3").await;
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].1["rule"], "R9");
}

#[tokio::test]
async fn r10_moves_must_take_the_head_of_the_queue() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let head = add(&mut conn, &session, "待辦", "第一張").await;
    let second = add(&mut conn, &session, "待辦", "第二張").await;
    let third = add(&mut conn, &session, "待辦", "第三張").await;

    // Before the first user.prompt: the move records the head, but R10 is not checked.
    mv(&mut conn, &session, third, "實作").await;
    let rows = last_rows(&mut conn, b.id, 1).await;
    assert_eq!(rows[0].0, "card.move");
    assert_eq!(
        rows[0].1,
        json!({"seq": third, "from": "待辦", "to": "實作", "queue_head": head})
    );
    let r = check(&mut conn, &session, RuleId::R10).await;
    assert!(!r.checked && r.passed);

    // The turn ends; the earlier move is not part of the next one.
    end_turn(&mut conn, &session).await;
    start_turn(&mut conn, &session, "t1").await;
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);

    // Skipping the head fails.
    mv(&mut conn, &session, second, "實作").await;
    let r = check(&mut conn, &session, RuleId::R10).await;
    assert_eq!(
        violations(&r),
        [(
            Some(second),
            format!(
                "R10: card {second} \"第二張\" moved from \"待辦\" to \"實作\" this turn, \
                 but the head of that queue was #{head}."
            ),
            format!(
                "kb card order <n> <n> ...  (the head was #{head}; fix the order if the queue \
                 was wrong)"
            ),
            format!("#{second} 跳過「待辦」的第一張 #{head} 就開始"),
        )]
    );
    // The move still counts after the card leaves the doing lane.
    mv(&mut conn, &session, second, "待辦").await;
    assert_eq!(
        check(&mut conn, &session, RuleId::R10)
            .await
            .violations
            .len(),
        1
    );
    // A move that is not todo -> doing (here back to the todo lane) records no head.
    let moved_back = last_rows(&mut conn, b.id, 1).await;
    assert!(
        moved_back[0].1.get("queue_head").is_none(),
        "{moved_back:?}"
    );

    // Block blocks (the turn goes on); then warn records check.warn and ends the turn.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R10, Level::Block))
            .await
            .unwrap(),
        Stop::Block(_)
    ));
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].1["rule"], "R10");

    // Taking the head passes.
    start_turn(&mut conn, &session, "t2").await;
    mv(&mut conn, &session, head, "實作").await;
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);

    // A card created directly in a doing lane, or entering it from a done lane, is not
    // checked.
    add(&mut conn, &session, "實作", "直接開始").await;
    let done = add(&mut conn, &session, "完成", "做完又重開").await;
    mv(&mut conn, &session, done, "實作").await;
    assert!(
        last_rows(&mut conn, b.id, 1).await[0]
            .1
            .get("queue_head")
            .is_none()
    );
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);
}

#[tokio::test]
async fn r10_a_blocked_head_is_not_the_head() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let blocked = add(&mut conn, &session, "待辦", "等待回答").await;
    let free = add(&mut conn, &session, "待辦", "可以做").await;
    let later = add(&mut conn, &session, "待辦", "之後").await;
    write::card_block(
        &mut conn,
        &session,
        blocked,
        "waiting for answer to question 1",
    )
    .await
    .unwrap();
    start_turn(&mut conn, &session, "t1").await;

    // The blocked first card is skipped: the head is the first card that is not blocked.
    mv(&mut conn, &session, free, "實作").await;
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].1["queue_head"], free);
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);

    // Every card left in the lane is blocked (one by a reason, one by a link): no head,
    // the move records null and passes.
    let blocker = add(&mut conn, &session, "實作", "前置工作").await;
    write::link_add(
        &mut conn,
        &session,
        blocker,
        LinkKind::Blocks,
        LinkTarget::Card(later),
        None,
    )
    .await
    .unwrap();
    mv(&mut conn, &session, later, "實作").await;
    assert_eq!(
        last_rows(&mut conn, b.id, 1).await[0].1["queue_head"],
        Value::Null
    );
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);
}

#[tokio::test]
async fn r10_a_discussing_card_is_not_the_head() {
    // docs/board-v2.md sections 10.2 and 17.1: the head is the first card that is neither
    // blocked nor under discussion.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let talk = add_discussing(&mut conn, &session, "待辦", "還在討論").await;
    let blocked = add(&mut conn, &session, "待辦", "等待回答").await;
    let free = add(&mut conn, &session, "待辦", "可以做").await;
    let later = add(&mut conn, &session, "待辦", "之後").await;
    write::card_block(&mut conn, &session, blocked, "waiting")
        .await
        .unwrap();
    start_turn(&mut conn, &session, "t1").await;

    // Discussing first, blocked second: skipping both to take the ready card passes.
    mv(&mut conn, &session, free, "實作").await;
    assert_eq!(last_rows(&mut conn, b.id, 1).await[0].1["queue_head"], free);
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);

    // Ready card first, then a discussing one moved in front of it: the head is still the
    // ready one, so taking it passes.
    write::card_next(&mut conn, &session, talk).await.unwrap();
    let snap = read::board_snapshot(&mut conn, b.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.lanes[0].cards[0].card.seq, talk);
    mv(&mut conn, &session, later, "實作").await;
    assert_eq!(
        last_rows(&mut conn, b.id, 1).await[0].1["queue_head"],
        later
    );
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);

    // Only a discussing and a blocked card are left: no head (null), and moving the
    // discussing card records nothing for R10 (R7 covers it).
    let warnings = mv(&mut conn, &session, talk, "實作").await;
    assert!(warnings[0].starts_with("R7: "), "{warnings:?}");
    assert!(
        last_rows(&mut conn, b.id, 1).await[0]
            .1
            .get("queue_head")
            .is_none()
    );
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);
    mv(&mut conn, &session, blocked, "實作").await;
    assert_eq!(
        last_rows(&mut conn, b.id, 1).await[0].1["queue_head"],
        Value::Null
    );
    assert!(check(&mut conn, &session, RuleId::R10).await.passed);

    // A ready card behind a discussing one: taking the later ready card skips the head.
    let first = add(&mut conn, &session, "待辦", "第一張可做").await;
    add_discussing(&mut conn, &session, "待辦", "討論二").await;
    let second = add(&mut conn, &session, "待辦", "第二張可做").await;
    mv(&mut conn, &session, second, "實作").await;
    let r = check(&mut conn, &session, RuleId::R10).await;
    assert_eq!(violations(&r).len(), 1);
    assert!(
        violations(&r)[0]
            .1
            .ends_with(&format!("but the head of that queue was #{first}."))
    );
}

#[tokio::test]
async fn r7_to_r10_after_the_turns_one_block() {
    // docs/board-v2.md sections 16.1 and 17.4: after a block, the new rules are recorded
    // with their level, and the next prompt reminds in rule-number order.
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let head = add(&mut conn, &session, "待辦", "第一張").await;
    let skip = add(&mut conn, &session, "待辦", "第二張").await;
    let discuss = add_discussing(&mut conn, &session, "實作", "討論中").await;
    start_turn(&mut conn, &session, "t1").await;
    mv(&mut conn, &session, skip, "實作").await;
    let q = ask(&mut conn, &session, skip, "要不要拆開？").await;

    let mut levels = Levels::default();
    levels.set(RuleId::R8, Level::Block);
    let after = hook::stop_after_block(&mut conn, &session, &levels)
        .await
        .unwrap();
    assert!(matches!(after, Stop::AfterBlock { .. }));
    let rows = last_rows(&mut conn, b.id, 4).await;
    assert_eq!(rows[0].0, "turn.summary");
    let recorded: Vec<(&str, &str, bool)> = rows[1..]
        .iter()
        .map(|(verb, d)| {
            assert_eq!(verb, "check.warn");
            (
                d["rule"].as_str().unwrap(),
                d["level"].as_str().unwrap(),
                d["after_block"].as_bool().unwrap(),
            )
        })
        .collect();
    // Newest first: R10, R8, R7 were written in rule order.
    assert_eq!(
        recorded,
        [
            ("R10", "warn", true),
            ("R8", "block", true),
            ("R7", "warn", true)
        ]
    );

    let p = hook::prompt(&mut conn, &session, Some("t2"), "好")
        .await
        .unwrap()
        .unwrap();
    let lines: Vec<String> = p.reminders.iter().map(|r| r.line()).collect();
    assert_eq!(
        lines,
        [
            format!(
                "R7: card {discuss} \"討論中\" is still under discussion but is in a doing lane. \
                 Fix: kb card confirm {discuss}  (once the user agreed; otherwise move it back: \
                 kb card move {discuss} --to <lane>)"
            ),
            format!(
                "R8: card {skip} \"第二張\" is in a doing lane with open question {q} but is not \
                 blocked. (was not enforced: one block per turn) \
                 Fix: kb card block {skip} --reason \"waiting for answer to question {q}\""
            ),
            format!(
                "R10: card {skip} \"第二張\" moved from \"待辦\" to \"實作\" this turn, but the head \
                 of that queue was #{head}. Fix: kb card order <n> <n> ...  (the head was \
                 #{head}; fix the order if the queue was wrong)"
            ),
        ]
    );
}

// ----- R11 (docs/board-v2.md section 19.4) ------------------------------------------

fn r11_fix(seq: i32, q: i64) -> String {
    format!(
        "kb question cancel {q} --reason \"answered in the session\"  (if it was answered in \
         the session; otherwise move the card back: kb card move {seq} --to <lane>)"
    )
}

#[tokio::test]
async fn r11_open_question_on_a_finished_card() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let b = board(&mut conn, &session).await;
    let card = add(&mut conn, &session, "實作", "選定輸出格式").await;
    let q = ask(&mut conn, &session, card, "要用哪一種結構？").await;
    // Open questions on unfinished cards are not R11's business.
    assert!(check(&mut conn, &session, RuleId::R11).await.passed);

    // kb card done succeeds and echoes the warning at once; the exit code is unchanged.
    let out = write::card_done(&mut conn, &session, card, "改用欄位陣列")
        .await
        .unwrap();
    assert_eq!(
        out.warnings,
        [format!(
            "R11: card {card} is finished but question {q} is still open; if it was answered \
             in the session, run: kb question cancel {q} --reason \"answered in the session\""
        )]
    );
    // The echo suggests exactly the command of R11's fix (docs/board-v2.md section 20.3).
    let fix = r11_fix(card, q);
    let command = fix.split("  (").next().unwrap();
    assert!(out.warnings[0].ends_with(&format!("run: {command}")));
    assert!(!out.warnings[0].contains('\n'));

    // Checked without a turn: it is the board's state.
    let r = check(&mut conn, &session, RuleId::R11).await;
    assert!(r.checked && !r.passed);
    assert_eq!(
        violations(&r),
        [(
            Some(card),
            format!(
                "R11: card {card} \"選定輸出格式\" is finished but question {q} is still open."
            ),
            r11_fix(card, q),
            format!("#{card} 已經結束，提問 {q} 仍然待回答"),
        )]
    );

    // Default level warn: the stop passes and records check.warn.
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    let rows = last_rows(&mut conn, b.id, 2).await;
    assert_eq!(rows[0].0, "turn.summary");
    assert_eq!(rows[1].0, "check.warn");
    assert_eq!(rows[1].1["rule"], "R11");
    assert_eq!(
        rows[1].1["message"],
        format!("#{card} 已經結束，提問 {q} 仍然待回答")
    );

    // Off: nothing recorded. Block: the stop blocks with the text and the fix.
    assert!(matches!(
        hook::stop(&mut conn, &session, &only(RuleId::R11, Level::Off))
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    assert_eq!(last_rows(&mut conn, b.id, 2).await[1].0, "turn.summary");
    let r = check(&mut conn, &session, RuleId::R11).await;
    assert!(!r.passed, "off only changes what the stop does");
    match hook::stop(&mut conn, &session, &only(RuleId::R11, Level::Block))
        .await
        .unwrap()
    {
        Stop::Block(results) => assert_eq!(
            rules::block_text(&results, rules::STOP_HEADER),
            format!(
                "Kanban rules failed. Fix them, then stop again.\n\
                 R11: card {card} \"選定輸出格式\" is finished but question {q} is still open.\n    \
                 Fix: {}",
                r11_fix(card, q)
            )
        ),
        other => panic!("{other:?}"),
    }

    // Cancelled: passes, and a later done has no echo.
    write::question_cancel(
        &mut conn,
        &session,
        q,
        Some("answered in the session".into()),
    )
    .await
    .unwrap();
    assert!(check(&mut conn, &session, RuleId::R11).await.passed);
    let out = write::card_done(&mut conn, &session, card, "改用欄位陣列")
        .await
        .unwrap();
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
}

#[tokio::test]
async fn r11_drop_with_several_open_questions_and_the_reminder() {
    let mut conn = common::writer().await;
    let session = new_board(&mut conn).await;
    let card = add(&mut conn, &session, "待辦", "舊的匯出").await;
    let q1 = ask(&mut conn, &session, card, "還要嗎？").await;
    let q2 = ask(&mut conn, &session, card, "誰在用？").await;
    start_turn(&mut conn, &session, "t1").await;
    let out = write::card_drop(&mut conn, &session, card, "改用新的匯出")
        .await
        .unwrap();
    assert_eq!(
        out.warnings,
        [format!(
            "R11: card {card} is finished but questions {q1}, {q2} are still open; if it was \
             answered in the session, run: kb question cancel {q1} --reason \"answered in the \
             session\""
        )]
    );
    let r = check(&mut conn, &session, RuleId::R11).await;
    assert_eq!(
        violations(&r),
        [(
            Some(card),
            format!(
                "R11: card {card} \"舊的匯出\" is finished but questions {q1}, {q2} are still open."
            ),
            r11_fix(card, q1),
            format!("#{card} 已經結束，提問 {q1}、{q2} 仍然待回答"),
        )]
    );
    // The stop records the warning; the next prompt reminds the agent.
    assert!(matches!(
        hook::stop(&mut conn, &session, &Levels::default())
            .await
            .unwrap(),
        Stop::Pass { .. }
    ));
    let p = hook::prompt(&mut conn, &session, Some("t2"), "繼續")
        .await
        .unwrap()
        .unwrap();
    let lines: Vec<String> = p.reminders.iter().map(|r| r.line()).collect();
    assert_eq!(
        lines,
        [format!(
            "R11: card {card} \"舊的匯出\" is finished but questions {q1}, {q2} are still open. \
             Fix: {}",
            r11_fix(card, q1)
        )]
    );
    // A card that only moved into a done lane has no echo; the stop finds it (R11).
    let other = add(&mut conn, &session, "實作", "另一張").await;
    ask(&mut conn, &session, other, "要不要？").await;
    assert!(mv(&mut conn, &session, other, "完成").await.is_empty());
    assert_eq!(
        check(&mut conn, &session, RuleId::R11)
            .await
            .violations
            .len(),
        2
    );
}
