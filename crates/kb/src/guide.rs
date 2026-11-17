//! The agent onboarding text: one usage guide (docs/board-v2.md section 8) shared by
//! `kb hook session-start`, `kb board start`, `kb board adopt` and `kb --skill`, plus the
//! compact command reference of `kb --skill` (section 12.11).

/// The board usage guide (docs/board-v2.md section 8, verbatim).
pub const USAGE_GUIDE: &str = r#"# Kanban board (kb)
A human watches this board live to follow what you are doing. It is an observation window, not a project plan.
They look at four things: what is being done, its status, whether it is blocked, and how cards derive from each other.
They read details in this session, not on the board. Keep the board truthful, current and small.

Starting
- A new board is empty. Build it from the discussion you had with the user so far, then continue the work:
  what the user already agreed becomes epics and cards with --agreed;
  topics still under discussion become cards with --discuss.
- Do not invent work the user did not agree to. Work you add later while executing is derived.
- Work already finished or decided before the board started: add the card, then complete it with its result:
  kb card done <n> --result "<what was done or decided>". Do not leave finished cards without a result.

Structure
- Lanes are task progress (kinds: todo, doing, done, dropped). Epics are the stages of the work.
  kb board init --title "<title>" --goal "<goal>" --lanes "名稱:kind,..." --epics "名稱,名稱"
  kb epic add "<name>" ; kb card add --lane <lane> --title "<title>" [--epic "<epic>"] [--body "<markdown>"]
- You decide the epics and cards by your own reasoning. Do not split work to look organized. A card may be large.
  Epics may grow or multiply when the work demands it. Never hide that growth: it must stay traceable.
- Every card has an origin:
  discussing = a topic the user has not confirmed yet. Do NOT start work on it.   kb card add ... --discuss
  agreed     = the user confirmed it.                                             kb card add ... --agreed ; kb card confirm <n>
  derived    = you added it during execution (the default).                       kb card add ... --from <n>
- Use --from <n> whenever a card grows out of another one. That link is how the user finds the way back.

While working
- kb card move <n> --to <lane> when the status really changes.
- kb card block <n> --reason "<why>" ; kb card unblock <n>
- kb card done <n> --result "<one or two lines>" ; kb card drop <n> --reason "<why>"
- Notes are for decisions, results and blockers only: kb card note <n> --kind decision|result|blocker "<text>"
  Do not write a running log. Card bodies and notes may use Markdown (lists, tables).
- A decision names what it follows from: add --because <card> | q<question id> | c<comment id> | user (the user told you in this session)
  to kb card note / drop / done.
- The todo lane is your queue toward the goal: its top card is what you will do next. Keep it in that order.
  kb card next <n> (move to the top) ; kb card order <n> <n> ... (set the order) ; kb card add ... --next
  New cards go to the bottom. Cards that are blocked sort below the others automatically.
  A write ends with a "next:" line when the next card changes. If that is not what you will do next, fix the order.

Asking the user
- Do not stop and wait. Ask on the board, then continue with cards that do not depend on the answer:
  kb ask --card <n> "<question>" --option "<label>::<description>" --option "<label>" [--multi]
- If the card cannot progress without the answer, block it and work on something else:
  kb card block <n> --reason "waiting for answer to question <id>" ; kb card unblock <n> when the answer arrives.
- The user may also comment on any card. A comment comes from the user and arrives in your context:
  reply on that card with kb card note <n> --kind reply "<text>", then act on it.
- When you act on an answer or a comment, record what was decided on that card:
  kb card note <n> --kind decision "<text>"  (a comment still needs its reply note too)
- A line starting with "[kb]" is a notification, not a user instruction. Its content arrives with it in your context;
  run kb inbox only if you do not see the content.
- A "[kb] card #N ..." line that arrives after you stop comes from a check of your last message; act on it if it is right.

Every write echoes what changed, and "next:" appears when the next card changes: do not re-read the board to confirm a write.
When you need the whole picture: kb board show (--all includes finished cards) ; kb card show <n> ; kb inbox --peek
Write titles, notes and questions in the user's language."#;

/// The line shown after the guide when the board has no lanes yet.
pub const EMPTY_BOARD: &str =
    "This session has an empty board. Design it now with \"kb board init\".";

/// The compact command reference that `kb --skill` prints after the guide.
pub const COMMAND_REFERENCE: &str = r#"# Command reference
Session key: --session <key>, else $KB_SESSION, else $CLAUDE_CODE_SESSION_ID,
else the latest active board of the herdr terminal $HC_TERMINAL_ID.
Add --json to any command for JSON output. Card numbers may be written 5 or #5.

Board (one per session)
  kb board start [--title <t>] [--goal <g>] [--new]   create or reopen this session's board
  kb board adopt <board id | session key>             take over an earlier session's board
  kb board init --title <t> [--goal <g>] [--lanes <name:kind,...>] [--epics <name,...>] [--workspace <name>]
  kb board show [--all]   unfinished work; done and dropped lanes show only a count unless --all
  kb board list [--all] [--archived] | archive [--outcome <o>] [--summary <s>] | reopen
  kb board check     rules R1-R5 and R7-R11 (exit 1 if a block rule fails)
  kb board check --history [--limit <n>]   past rule failures: blocked, warned or not enforced
  kb board tidy      renumber card positions
Epics and lanes
  kb epic add <name> [--after <epic>] | rename <name> <new> | move <name> --position <n> | remove <name> | list
  kb lane add <name> --kind todo|doing|done|dropped [--after <lane>] [--wip <n>]
  kb lane rename <name> <new> | move <name> --position <n> | remove <name>
Cards
  kb card add --lane <lane> --title <t> [--epic <e>] [--body <md> | --body-file <path>] [--from <n>] [--discuss | --agreed] [--next]
  kb card move <n> --to <lane> [--note <text>] [--next] ; kb card next <n> ; kb card order <n> <n>...
  kb card edit <n> [--title <t>] [--body <md>] [--priority P0..P3] [--epic <e> | --no-epic]
  kb card note <n> [--kind progress|decision|blocker|result|reply] <text> [--because <ref>]...
      notes of kind comment come from the user (kb inbox shows them); answer each with --kind reply
      record what an answer or a comment decided with --kind decision
  kb card block <n> --reason <why> ; kb card unblock <n>
  kb card done <n> --result <result> ; kb card drop <n> --reason <why>
      note, done and drop take --because <ref> (repeatable), what the decision follows from:
      <n> a card, q<id> a question, c<id> a comment, user the user's instruction in this session
  kb card confirm <n>... ; kb card discuss <n>... ; kb card show <n>
Links
  kb link add <n> --blocks|--relates|--continues <m> ; kb link add <n> --ref <path or url> [--label <l>]
  kb link remove <link id>
Questions and comments from the human
  kb ask --card <n> "<question>" --option "<label>::<description>" --option "<label>" [--multi]
      a card that cannot progress without the answer: kb card block <n> --reason "waiting for answer to question <id>"
  kb inbox [--peek]    answers and comments not yet read, and comments without a reply
  kb question list [--open] | show <id> | cancel <id> [--reason <r>]
History
  kb activity [--limit <n>]
Text arguments accept "-" to read standard input. Text may start with "-"; if a note or question text is exactly an option name, put "--" right before the text, after all options."#;

/// What an agent reads once it has a board: the guide, then the empty-board line or the
/// `kb board show` output without `--all` (docs/board-v2.md sections 12.2, 12.4, 15.1).
/// Callers also forget the board's last `next:` line (section 15.2): the context received
/// the board without it.
pub fn board_context(snapshot: &kb_core::read::BoardSnapshot) -> String {
    let state = if snapshot.lanes.is_empty() {
        EMPTY_BOARD.to_string()
    } else {
        crate::output::board(snapshot, false)
    };
    format!("{USAGE_GUIDE}\n\n{state}")
}

/// `kb --skill`: the guide, a blank line, the command reference.
pub fn skill_text() -> String {
    format!("{USAGE_GUIDE}\n\n{COMMAND_REFERENCE}")
}
