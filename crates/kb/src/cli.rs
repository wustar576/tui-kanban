//! Command-line syntax (docs/cli.md section 3).
//!
//! Enumerated values (kinds, priorities, card numbers) are taken as strings and validated
//! by kb-core, so an invalid value is a data error (exit 1), while unknown options or
//! missing arguments are syntax errors (exit 2).

use clap::{ArgGroup, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "kb",
    version,
    about = "Kanban board that agents write to and humans watch",
    propagate_version = true,
    arg_required_else_help = true,
    after_help = "Agent? Run: kb --skill"
)]
pub struct Cli {
    /// Print the complete agent guide and command reference, then exit
    #[arg(long, exclusive = true)]
    pub skill: bool,

    /// Session key (default: $KB_SESSION, then $CLAUDE_CODE_SESSION_ID, then the latest
    /// active board of the herdr terminal $HC_TERMINAL_ID)
    #[arg(long, global = true, value_name = "KEY")]
    pub session: Option<String>,

    /// Print the result as JSON
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Database schema and connection
    Db {
        #[command(subcommand)]
        cmd: DbCmd,
    },
    /// Boards (one per session)
    Board {
        #[command(subcommand)]
        cmd: BoardCmd,
    },
    /// Workspaces (groups of boards)
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Lanes of the session's board
    Lane {
        #[command(subcommand)]
        cmd: LaneCmd,
    },
    /// Epics (stages of the work) of the session's board
    Epic {
        #[command(subcommand)]
        cmd: EpicCmd,
    },
    /// Cards of the session's board
    Card {
        #[command(subcommand)]
        cmd: CardCmd,
    },
    /// Links between cards, or from a card to an external target
    Link {
        #[command(subcommand)]
        cmd: LinkCmd,
    },
    /// Show the session board's activity, newest first
    Activity {
        /// Number of entries
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Ask the human a question on a card and return immediately (docs/questions.md)
    Ask {
        /// Card the question is about
        #[arg(long, value_name = "CARD")]
        card: String,
        /// The question ("-" reads standard input)
        #[arg(allow_hyphen_values = true)]
        question: String,
        /// A candidate answer: "label" or "label::description" (2 to 6)
        #[arg(
            long = "option",
            value_name = "OPTION",
            required = true,
            allow_hyphen_values = true
        )]
        options: Vec<String>,
        /// Allow several options to be selected
        #[arg(long)]
        multi: bool,
    },
    /// Show answers and comments not yet delivered and mark them delivered
    Inbox {
        /// Only show; do not mark as delivered
        #[arg(long)]
        peek: bool,
    },
    /// Questions of the session's board
    Question {
        #[command(subcommand)]
        cmd: QuestionCmd,
    },
    /// Claude Code hook entry points; read the hook's JSON on standard input (docs/hooks.md)
    Hook {
        #[command(subcommand)]
        event: HookCmd,
    },
    /// Terminal UI for humans; its only write is answering questions (docs/tui.md)
    Tui(TuiArgs),
}

/// `kb tui [--board <KEY>] [--workspace <NAME>] [--snapshot [--width N] [--height N] [--card N]]`.
#[derive(Debug, Args)]
pub struct TuiArgs {
    /// Show the board of this session key
    #[arg(long, value_name = "KEY")]
    pub board: Option<String>,
    /// Show the most recently changed active board of this workspace (name or key)
    #[arg(long, value_name = "NAME")]
    pub workspace: Option<String>,
    /// Print one frame as plain text and exit (no interactive mode)
    #[arg(long)]
    pub snapshot: bool,
    /// Snapshot width in columns (default 120)
    #[arg(long, value_name = "COLS", requires = "snapshot", value_parser = clap::value_parser!(u16).range(1..))]
    pub width: Option<u16>,
    /// Snapshot height in rows (default 30)
    #[arg(long, value_name = "ROWS", requires = "snapshot", value_parser = clap::value_parser!(u16).range(1..))]
    pub height: Option<u16>,
    /// Snapshot the screen of this card instead of the board
    #[arg(long, value_name = "N", requires = "snapshot")]
    pub card: Option<i32>,
    /// Snapshot the screen of this epic (「未歸類」 for cards without an epic)
    #[arg(
        long,
        value_name = "NAME",
        requires = "snapshot",
        conflicts_with = "card"
    )]
    pub epic: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DbCmd {
    /// Create or update tables and roles (connects with $KB_ADMIN_URL only)
    Migrate {
        /// Target database instead of the one in $KB_ADMIN_URL, e.g. kanban_test (created if missing)
        #[arg(long, value_name = "NAME")]
        database: Option<String>,
        /// Apply pending migrations even though the database already has boards
        #[arg(long)]
        yes: bool,
    },
    /// Check the connection and the schema version
    Status,
}

#[derive(Debug, Subcommand)]
pub enum BoardCmd {
    /// Start the board of this session (what /kb runs): create it, or reopen it
    Start {
        /// Title of a new board
        #[arg(long, allow_hyphen_values = true)]
        title: Option<String>,
        /// Goal of a new board ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        goal: Option<String>,
        /// Create a new board even when earlier boards of this terminal or folder exist
        #[arg(long)]
        new: bool,
    },
    /// Take over an earlier session's board (by board id or session key)
    Adopt {
        /// Board id, or the session key the board belongs to
        target: String,
    },
    /// Create the session's board, or update its title and goal
    Init {
        /// Board title (required unless the board already has one, which is then kept)
        #[arg(long, allow_hyphen_values = true)]
        title: Option<String>,
        /// Task goal ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        goal: Option<String>,
        /// Lanes as name:kind,name:kind (kinds: todo, doing, done, dropped)
        #[arg(long, value_name = "LIST")]
        lanes: Option<String>,
        /// Epics as name,name,... created in order (existing names are skipped)
        #[arg(long, value_name = "LIST")]
        epics: Option<String>,
        /// Workspace name (default: the herdr space, else the current directory)
        #[arg(long, value_name = "NAME")]
        workspace: Option<String>,
    },
    /// Show the session's board (done and dropped lanes show only their card count)
    Show {
        /// Also list the cards of done and dropped lanes
        #[arg(long)]
        all: bool,
    },
    /// List boards
    List {
        /// Only boards of this workspace (name or key)
        #[arg(long, value_name = "NAME")]
        workspace: Option<String>,
        /// Boards of all workspaces
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
        /// Include archived boards
        #[arg(long)]
        archived: bool,
    },
    /// Archive the session's board
    Archive {
        /// completed, incomplete or abandoned (default: completed if every card is closed)
        #[arg(long)]
        outcome: Option<String>,
        /// Closing summary ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        summary: Option<String>,
    },
    /// Reopen the session's archived board
    Reopen,
    /// Check rules R1-R5 and R7-R11 for the board and this turn (exit 1 if a block rule fails)
    Check {
        /// List past rule failures of the board instead, newest first (reads only)
        #[arg(long)]
        history: bool,
        /// Number of failures --history lists (default 20)
        #[arg(long, value_name = "N", requires = "history")]
        limit: Option<i64>,
    },
    /// Renumber card positions in every lane and record a turn summary
    Tidy,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCmd {
    /// List workspaces with their board counts
    List,
}

#[derive(Debug, Subcommand)]
pub enum LaneCmd {
    /// Add a lane
    Add {
        name: String,
        /// todo, doing, done or dropped
        #[arg(long)]
        kind: String,
        /// Insert after this lane (default: far right)
        #[arg(long, value_name = "LANE")]
        after: Option<String>,
        /// WIP limit: maximum number of cards in the lane
        #[arg(long, value_name = "N")]
        wip: Option<i32>,
    },
    /// Rename a lane
    Rename { name: String, new_name: String },
    /// Move a lane to a position (1 = leftmost)
    Move {
        name: String,
        #[arg(long)]
        position: i32,
    },
    /// Remove an empty lane
    Remove { name: String },
}

#[derive(Debug, Subcommand)]
pub enum EpicCmd {
    /// Add an epic
    Add {
        name: String,
        /// Insert after this epic (default: last)
        #[arg(long, value_name = "EPIC")]
        after: Option<String>,
    },
    /// Rename an epic
    Rename { name: String, new_name: String },
    /// Move an epic to a position (1 = first)
    Move {
        name: String,
        #[arg(long)]
        position: i32,
    },
    /// Remove an epic that has no cards
    Remove { name: String },
    /// List the epics with their counts
    List,
}

#[derive(Debug, Subcommand)]
pub enum CardCmd {
    /// Add a card (origin derived unless --discuss or --agreed)
    Add {
        #[arg(long)]
        lane: String,
        #[arg(long, allow_hyphen_values = true)]
        title: String,
        /// P0 (highest) to P3
        #[arg(long, default_value = "P2")]
        priority: String,
        #[command(flatten)]
        body: BodyArgs,
        #[arg(long, default_value = "agent")]
        owner: String,
        /// Card this one is spawned from (creates a spawned link; inherits its epic)
        #[arg(long, value_name = "CARD")]
        from: Option<String>,
        /// Epic the card belongs to
        #[arg(long, value_name = "EPIC")]
        epic: Option<String>,
        /// Origin discussing: a topic the user has not confirmed yet
        #[arg(long, conflicts_with = "agreed")]
        discuss: bool,
        /// Origin agreed: the user confirmed it
        #[arg(long)]
        agreed: bool,
        /// Put the card at the front of the lane instead of the end
        #[arg(long)]
        next: bool,
    },
    /// Move a card to the front of its lane: it is what you will do next
    Next { card: String },
    /// Put cards at the front of their lane in this order; the others keep their order after them
    Order {
        #[arg(required = true, value_name = "CARD")]
        cards: Vec<String>,
    },
    /// Set the origin of cards to agreed (the user confirmed them)
    Confirm {
        #[arg(required = true, value_name = "CARD")]
        cards: Vec<String>,
    },
    /// Set the origin of cards to discussing (not confirmed yet)
    Discuss {
        #[arg(required = true, value_name = "CARD")]
        cards: Vec<String>,
    },
    /// Move a card to another lane
    Move {
        card: String,
        #[arg(long, value_name = "LANE")]
        to: String,
        /// Also add a progress note ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        note: Option<String>,
        /// Enter the lane at the front instead of the end
        #[arg(long)]
        next: bool,
    },
    /// Edit a card's fields
    #[command(group(ArgGroup::new("field").required(true).multiple(true)
        .args(["title", "body", "body_file", "priority", "owner", "epic", "no_epic"])))]
    Edit {
        card: String,
        #[arg(long, allow_hyphen_values = true)]
        title: Option<String>,
        #[command(flatten)]
        body: BodyArgs,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        /// Put the card in this epic
        #[arg(long, value_name = "EPIC", conflicts_with = "no_epic")]
        epic: Option<String>,
        /// Take the card out of its epic
        #[arg(long)]
        no_epic: bool,
    },
    /// Add a note to a card
    Note {
        card: String,
        /// progress, decision, blocker, result or reply (reply answers a comment from the user)
        #[arg(long, default_value = "progress")]
        kind: String,
        /// Note text ("-" reads standard input)
        #[arg(allow_hyphen_values = true)]
        text: String,
        #[command(flatten)]
        because: BecauseArgs,
    },
    /// Mark a card as blocked
    Block {
        card: String,
        /// Why it is blocked ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        reason: String,
    },
    /// Clear a card's blocked mark
    Unblock { card: String },
    /// Complete a card: move it to the leftmost done lane
    Done {
        card: String,
        /// Outcome of the work ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        result: String,
        #[command(flatten)]
        because: BecauseArgs,
    },
    /// Drop a card: move it to the leftmost dropped lane
    Drop {
        card: String,
        /// Why it is dropped ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        reason: String,
        #[command(flatten)]
        because: BecauseArgs,
    },
    /// Show a card with its notes and links
    Show { card: String },
}

/// `--because <ref>`, repeatable (docs/board-v2.md section 18.2).
#[derive(Debug, Args)]
pub struct BecauseArgs {
    /// What this decision follows from: a card (30 or #30), a question (q5), a comment
    /// (c75) or the user's instruction in this session (user); repeatable
    #[arg(long = "because", value_name = "REF")]
    pub because: Vec<String>,
}

/// `--body <text|->` or `--body-file <path>`.
#[derive(Debug, Args)]
pub struct BodyArgs {
    /// Card body ("-" reads standard input)
    #[arg(long, allow_hyphen_values = true, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the card body from a file (UTF-8)
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum LinkCmd {
    /// Add a link from a card
    #[command(group(ArgGroup::new("target").required(true)
        .args(["blocks", "relates", "continues", "ref"])))]
    Add {
        card: String,
        /// This card blocks CARD
        #[arg(long, value_name = "CARD")]
        blocks: Option<String>,
        /// This card relates to CARD
        #[arg(long, value_name = "CARD")]
        relates: Option<String>,
        /// This card continues CARD
        #[arg(long, value_name = "CARD")]
        continues: Option<String>,
        /// External target: a file path, URL or any text
        #[arg(long = "ref", value_name = "TARGET", allow_hyphen_values = true)]
        r#ref: Option<String>,
        /// Description of the link
        #[arg(long, allow_hyphen_values = true)]
        label: Option<String>,
    },
    /// Remove a link by its id (shown by "kb card show")
    Remove { id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum QuestionCmd {
    /// List the board's questions
    List {
        /// Only questions waiting for an answer
        #[arg(long)]
        open: bool,
    },
    /// Show a question with its options and times
    Show { id: i64 },
    /// Cancel an open question
    Cancel {
        id: i64,
        /// Why it is cancelled ("-" reads standard input)
        #[arg(long, allow_hyphen_values = true)]
        reason: Option<String>,
    },
}

/// Hook events (docs/hooks.md section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum HookCmd {
    /// SessionStart: if the session has a board, reopen it and inject the usage guide
    SessionStart,
    /// UserPromptSubmit: start a new turn; deliver answers and comments
    Prompt,
    /// PreToolUse: rules R1 and R6 (exit 2 blocks the tool)
    PreTool,
    /// PostToolUse: record the tool use (no notification)
    ToolUsed,
    /// Stop: deliver answers and comments, then rules R2-R5 and R7-R11
    Stop,
    /// SessionEnd: archive the board
    SessionEnd,
    /// SessionStart/Stop, async + asyncRewake: wait for new answers or comments, then wake
    /// the session (exit 2)
    Wait,
    /// Stop, async + asyncRewake: a model checks the agent's last message; wakes the
    /// session (exit 2) for a finding in the wake band
    Judge,
}

#[cfg(test)]
mod tests {
    //! Text values that start with `-` (docs/board-v2.md section 19.1).

    use clap::Parser;

    use super::Cli;

    /// Parses `kb <args>` and returns the debug form of the result.
    fn parse(args: &[&str]) -> String {
        let argv = std::iter::once("kb").chain(args.iter().copied());
        match Cli::try_parse_from(argv) {
            Ok(cli) => format!("{cli:?}"),
            Err(e) => panic!("{args:?} did not parse: {e}"),
        }
    }

    fn fails(args: &[&str]) {
        let argv = std::iter::once("kb").chain(args.iter().copied());
        assert!(Cli::try_parse_from(argv).is_err(), "{args:?} parsed");
    }

    const VALUES: [&str; 3] = ["--na-values 改為空字串", "-x", "--discuss"];

    /// `prefix --opt <value>` and `prefix --opt=<value>` keep the value; `expect` is the
    /// debug form of the field with `{}` for the value's debug form.
    fn both_forms(prefix: &[&str], opt: &str, suffix: &[&str], expect: &str) {
        for v in VALUES {
            let want = expect.replace("{}", &format!("{v:?}"));
            let mut spaced: Vec<&str> = prefix.to_vec();
            spaced.extend([opt, v]);
            spaced.extend(suffix);
            assert!(parse(&spaced).contains(&want), "{spaced:?}: {want}");
            let joined = format!("{opt}={v}");
            let mut eq: Vec<&str> = prefix.to_vec();
            eq.push(&joined);
            eq.extend(suffix);
            assert!(parse(&eq).contains(&want), "{eq:?}: {want}");
        }
    }

    #[test]
    fn free_text_options_take_values_that_start_with_a_hyphen() {
        both_forms(&["board", "start"], "--title", &[], "title: Some({})");
        both_forms(&["board", "start"], "--goal", &[], "goal: Some({})");
        both_forms(&["board", "init"], "--title", &[], "title: Some({})");
        // --title is optional (docs/board-v2.md section 20.1).
        both_forms(&["board", "init"], "--goal", &[], "goal: Some({})");
        both_forms(
            &["board", "init", "--title", "t"],
            "--goal",
            &[],
            "goal: Some({})",
        );
        both_forms(&["board", "archive"], "--summary", &[], "summary: Some({})");
        both_forms(
            &["card", "add", "--lane", "待辦"],
            "--title",
            &[],
            "title: {}",
        );
        both_forms(
            &["card", "add", "--lane", "待辦", "--title", "t"],
            "--body",
            &[],
            "body: Some({})",
        );
        both_forms(&["card", "edit", "1"], "--title", &[], "title: Some({})");
        both_forms(&["card", "edit", "1"], "--body", &[], "body: Some({})");
        both_forms(
            &["card", "move", "1", "--to", "完成"],
            "--note",
            &[],
            "note: Some({})",
        );
        both_forms(&["card", "block", "1"], "--reason", &[], "reason: {}");
        both_forms(&["card", "done", "1"], "--result", &[], "result: {}");
        both_forms(&["card", "drop", "1"], "--reason", &[], "reason: {}");
        both_forms(&["link", "add", "1"], "--ref", &[], "ref: Some({})");
        both_forms(
            &["link", "add", "1", "--blocks", "2"],
            "--label",
            &[],
            "label: Some({})",
        );
        both_forms(
            &["ask", "--card", "1", "--option", "a"],
            "--option",
            &["問題"],
            "options: [\"a\", {}]",
        );
        both_forms(
            &["question", "cancel", "1"],
            "--reason",
            &[],
            "reason: Some({})",
        );
    }

    #[test]
    fn options_after_a_free_text_value_still_parse() {
        let out = parse(&[
            "card", "add", "--lane", "待辦", "--title", "--x", "--epic", "e", "--agreed",
        ]);
        assert!(
            out.contains("title: \"--x\"") && out.contains("agreed: true"),
            "{out}"
        );
        let out = parse(&["card", "done", "1", "--result", "-x", "--because", "user"]);
        assert!(
            out.contains("result: \"-x\"") && out.contains("\"user\""),
            "{out}"
        );
    }

    #[test]
    fn text_positionals_take_values_that_start_with_a_hyphen() {
        for v in ["--na-values 改為空字串", "-x", "--unknown"] {
            let out = parse(&["card", "note", "1", "--kind", "decision", v]);
            assert!(out.contains(&format!("text: {v:?}")), "{out}");
            let out = parse(&["card", "note", "1", v, "--because", "user"]);
            assert!(out.contains(&format!("text: {v:?}")), "{out}");
            let out = parse(&["ask", "--card", "1", v, "--option", "a", "--option", "b"]);
            assert!(out.contains(&format!("question: {v:?}")), "{out}");
        }
        // A text that is exactly an option of the command needs "--" before it, after all
        // options (the command reference says so).
        fails(&["card", "note", "1", "--kind"]);
        let out = parse(&["card", "note", "1", "--kind", "decision", "--", "--kind"]);
        assert!(out.contains("text: \"--kind\""), "{out}");
        let out = parse(&["ask", "--card", "1", "--option", "a", "--", "-h"]);
        assert!(out.contains("question: \"-h\""), "{out}");
    }

    #[test]
    fn identifier_options_stay_strict() {
        fails(&["card", "add", "--lane", "--x", "--title", "t"]);
        fails(&["card", "move", "1", "--to", "-x"]);
        fails(&["card", "move", "1", "--to", "--x"]);
        fails(&["card", "done", "1", "--result", "r", "--because", "--x"]);
        fails(&[
            "card", "add", "--lane", "待辦", "--title", "t", "--epic", "-x",
        ]);
    }
}
