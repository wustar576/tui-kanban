# AGENTS.md

For coding agents working on this repository. (Agents that only use `kb` on a board run `kb --skill` instead.)

## What this is

`kb` is a terminal kanban that AI coding agents write to through the CLI and Claude Code hooks, while a person watches it in a read-only TUI (`kb tui`).
Data lives in a local PostgreSQL in Docker (port 55432).

## Layout

| Path | Content |
| --- | --- |
| `crates/kb-core` | config, database access, writes, reads, hook logic, rules, the model judge (`src/judge/`) |
| `crates/kb` | the `kb` binary: CLI, `kb hook <event>` entry points, the agent guide (`src/guide.rs`) |
| `crates/kb-tui` | the read-only terminal UI |
| `migrations/` | SQL schema versions, embedded in the binary; append-only |
| `plugin/` | Claude Code plugin: hooks (`hooks/hooks.json`) and the user-only skill `/kb:kb` |
| `.claude-plugin/` | marketplace manifest; the repository root is the marketplace `tui-kanban` |
| `examples/` | `claude-settings.json`: the same hooks for a manual `.claude/settings.json` setup |
| `scripts/` | `setup.ps1` (Windows) and `setup.sh` (macOS): database container, config file, build, migrations |

## Build

```sh
cargo build --release
cargo clippy --all-targets    # must be free of warnings
```

## Test

```sh
cargo test
```

- Most tests need the database: the container running, the config file `~/.config/kb/config.toml` written by the setup script, and `KB_ADMIN_URL` set in the shell (admin connection; it is read only from this variable, never from the config file).
- Tests use the database `kanban_test` only. They take the connection URLs from the normal config and replace the database name, and they check it.
- Unit tests that need no database: `cargo test -p kb-tui --lib`.
- Never read or write the database `kanban`: it holds the user's real boards. Use `kanban_test`, or a separate database for manual checks.
- Always pass `--database <name>` to `kb db migrate`; pass `--yes` only for a test database.

## Install and try

```sh
pwsh scripts/setup.ps1                                  # Windows
bash scripts/setup.sh                                   # macOS
claude plugin install kb --marketplace <project folder>
kb tui                                                  # in a second terminal
```

In a Claude Code session, type `/kb:kb` to start the board. `kb tui --snapshot --width 100 --height 30` prints the screen as text.

## Conventions a change must keep

- Cross-platform: everything works on macOS and Windows. Use `std::path`, no hard-coded separators, plain `kb ...` commands in the plugin. Nothing may be Windows-only.
- Lean CLI: no near-duplicate `kb` commands and no option restrictions; put constraints in hooks or rules instead.
- `plugin/hooks/hooks.json` must equal `examples/claude-settings.json`; a test checks it.
- The agent guide text exists once, in `crates/kb/src/guide.rs`.
- Migrations are append-only: never edit an applied file; add the next numbered file.
- Never write secrets to files, logs, activity or output: no passwords, no full database URLs, no API keys. The judge key comes only from `CLAUDE_PLUGIN_OPTION_JUDGE_API_KEY` or `KB_JUDGE_API_KEY`. Judge tests use a local fake HTTP server.
- Text for agents (CLI output, hook context) is English; card titles and lane names stay as written. The TUI text is Traditional Chinese.
- Do not run `cargo fmt` on files you did not change.

The design documents are not in this repository; code comments that cite `docs/...` refer to them.
