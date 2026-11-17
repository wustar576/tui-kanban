# tui-kanban

English | [繁體中文](README.zh-TW.md)

`kb` is a terminal kanban board for watching AI coding agents work. The agent writes the
board through the `kb` command line while it works; you watch it in a read-only terminal UI,
`kb tui`. It is wired into Claude Code through hooks, and the data lives in a local
PostgreSQL database.

## Why

When a coding agent runs a long task, its plan and progress are spread over a long
transcript. To learn what it did, why it decided something, or where it is stuck, you have
to read that transcript. A checklist in a `todo.md` only says done or not done; it does not
show stages, blockers, the history of a piece of work, or how pieces relate.

tui-kanban moves that information onto a board:

- The agent plans the work as cards in lanes and epics, moves cards as it works, and writes
  notes, decisions and results on the cards.
- You observe. One screen shows the current stage, what is blocked, and what changed last,
  without reading the transcript.
- Claude Code hooks connect the two: they deliver your answers and comments to the agent and
  check the agent against a set of behaviour rules at the end of each turn.

The board is started by you, not by the agent. A session without a board is not affected:
the hooks give the agent no board text until you type `/kb:kb`.

## What you see

An illustration of the board screen (the real interface text is Traditional Chinese; the
labels below are translated):

```
 Acme_DevOps › Fix login timeout › 3f2a9c1e…       Questions 1  Discuss 1  ● connected
┌ EPIC ──────────────┐┌ To do (2) ─────────────┐┌ Doing (1) ─────────────┐┌ Done 3 ──┐
│ ✓ P0 Reproduce 2/2 ││┌──────────────────────┐││┏━━━━━━━━━━━━━━━━━━━━━━┓││ #1 Repr… │
│   P1 Fix   1/4 +1  │││ #6 Token lifetime    │││┃▶#5 Refresh the token ┃││ #2 Coll… │
│   P2 Verify    0/2 │││ P1  Question         │││┃ on idle              ┃││ #3 Writ… │
│                    ││└──────────────────────┘││┃ P1  Derived          ┃││          │
│                    ││┌──────────────────────┐││┃ ↳ derived from #3    ┃││          │
│                    │││ #7 Load test         │││┗━━━━━━━━━━━━━━━━━━━━━━┛││          │
│                    │││ P2  Discuss          │││                        ││          │
│                    ││└──────────────────────┘││                        ││          │
└────────────────────┘└────────────────────────┘└────────────────────────┘└──────────┘
 11:20 #3 done
 11:19 #6 question: how long should a token live?
 11:18 #5 moved to "Doing"
 ←→ lanes  ↑↓ cards  Enter open  a question  i comment  e epic  o sort  s sidebar  q quit
```

The screen redraws within a second after the agent writes. What you can do with it:

1. **Follow the plan and the progress.** Lanes show progress; epics in the sidebar show the
   stages with `done/total` counts. Each card shows where it came from: agreed with you,
   still under discussion, or derived by the agent while working (with a link to the card it
   came from). Cards that just changed are outlined in green for two seconds.
2. **See decisions and their stated cause.** The agent records decisions as notes, and can
   name what a decision follows from: another card, a question, a comment, or your
   instruction in the conversation. The card screen lists the cause and, the other way
   round, the decisions a card led to; `Enter` jumps between them.
3. **Answer the agent's questions from the TUI.** Instead of stopping to wait, the agent
   writes a question with options on a card and keeps working on other cards. Press `a` to
   jump to the next open question, tick an option or type an answer, and press `Enter`. A
   background waiter wakes the session and the answer is delivered to the agent.
4. **Leave a comment on any card.** Press `i` on a card. The comment is delivered to the
   agent the same way, and stays marked as unanswered until the agent replies on the card.
5. **Let behaviour rules flag an agent that works without a card.** Before file edits and at
   the end of each turn, hooks check rules such as: the agent edits files while the board has
   no lanes or cards; files changed this turn but no card shows activity from the agent; a card
   still under discussion is in a doing lane; a delivered answer has no decision recorded; a
   card was finished without a result. Each rule can be set to off, warn or block in
   `~/.config/kb/rules.toml`; `kb board check --history` lists past violations.
6. **Optionally, have a model judge each turn.** Rules cannot see the conversation. With a
   Gemini API key, a background hook asks a decision model after each turn whether, for
   example, the agent gave advice about a card without writing it on the card, or is waiting
   for your reply in the chat. Findings appear in the activity line and as card labels.
   Without a key nothing is sent. With a key, each turn sends your message (up to 4,000
   characters), delivered answers and comments, the tools the agent used, its board
   commands, its last reply, and card numbers and titles.

The TUI writes only two things, answers and comments, and does so on a read-only database
role through two database functions. Everything else on the board is written by the agent.

## How it works

```
 Claude Code session                 local machine
┌──────────────────────┐   kb hook <event>   ┌──────────────┐
│ agent ── kb card ... ├────────────────────►│   kb (CLI)   │
│ hooks: SessionStart, │                     └──────┬───────┘
│ UserPromptSubmit,    │◄── answers, comments,      │ writer role
│ PreToolUse, Stop ... │    rule warnings           ▼
└──────────────────────┘                     ┌──────────────┐
                                             │  PostgreSQL  │
                                             │  (Docker)    │
                                             └──────┬───────┘
                                                    │ reader role
                                                    ▼
                                             ┌──────────────┐
                                             │    kb tui    │ ◄── you
                                             └──────────────┘
```

One binary, `kb`, is both the agent's command line and your viewer. The agent and the hooks
write with a writer role; `kb tui` reads with a reader role. Schema changes need a separate
admin connection that is read only from the environment variable `KB_ADMIN_URL`, never from
the config file, so an agent cannot use it by accident.

## Requirements and install

- Windows 11 with PowerShell 7 (`pwsh`), or macOS (zsh or bash).
- Docker Desktop, running.
- Rust 1.94 or later (`cargo`).
- Claude Code. On Windows, Git Bash, which Claude Code uses to run hook commands.

Run the setup script from the project folder:

```sh
pwsh scripts/setup.ps1     # Windows
bash scripts/setup.sh      # macOS (not yet verified on a real machine)
```

The script generates random database passwords into `.env`, starts the PostgreSQL container
(`127.0.0.1:55432`), writes `~/.config/kb/config.toml`, builds `kb` with
`cargo build --release` and applies the schema. It can be run again; it keeps existing
passwords and data. Put `target/release` on your `PATH`: the hooks run `kb` directly.

Install the Claude Code plugin. The project folder is itself a plugin marketplace named
`tui-kanban`:

```sh
claude plugin install kb --marketplace <project folder>
```

To turn on the model judge, run `/plugin configure kb@tui-kanban` once in Claude Code and
enter the API key, or pass `{"judge_api_key": "<key>"}` to
`claude plugin configure kb@tui-kanban --values-stdin`. The key is kept in the operating
system's credential store, not in a file.

Then, in a Claude Code session, discuss the work with the agent, and when you want a board,
type `/kb:kb` (optionally with a title, e.g. `/kb:kb fix login timeout`). Open `kb tui` in
another terminal.

## Command reference

| Command | Purpose |
| --- | --- |
| `kb tui` | Watch the most recently changed active board |
| `kb tui --board <session>` / `--workspace <name>` | Watch a given board, or the latest board in a workspace |
| `kb tui --snapshot --width 100 --height 30` | Print the board screen as plain text and exit |
| `kb board show [--all]` | Print the board as text (`--json` for JSON) |
| `kb board list` / `kb workspace list` | List boards and workspaces |
| `kb card show <n>` | Print one card with its notes, questions and links |
| `kb activity` | Print the activity trail |
| `kb board check [--history]` | Check the behaviour rules now, or list past violations |
| `kb db status` | Check the connection and the schema version |
| `kb --skill` | Print the full usage guide the agent reads |

Every command accepts `--json`. Command output is English; card titles and lane names are
shown as written.

The design documents are not part of this repository. Code comments that cite `docs/...`
refer to them.

## Status

Early. Built for the author's own use with Claude Code; other agents are not supported.
Commands, the database schema and the hook interface may change.

## Licence

MIT. See [LICENSE](LICENSE).
