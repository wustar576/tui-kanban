---
name: kb
description: Start the kanban board for this session. The user runs /kb after discussing the work with you; you then build the board from that discussion. If the kb program is missing, offers to install it.
disable-model-invocation: true
argument-hint: "[title or goal hint]"
allowed-tools: Bash(kb board *) Bash(kb epic *) Bash(kb lane *) Bash(kb card *) Bash(kb --version) Bash(echo KB_NOT_INSTALLED) PowerShell(kb board *) PowerShell(kb epic *) PowerShell(kb lane *) PowerShell(kb card *) PowerShell(kb --version) PowerShell(echo KB_NOT_INSTALLED) PowerShell(Write-Output KB_NOT_INSTALLED)
---

The user just started the kanban board for this session. `kb board start` printed this:

!`kb board start --session ${CLAUDE_SESSION_ID} || kb --version || echo KB_NOT_INSTALLED`

If the last line above is `KB_NOT_INSTALLED`, the `kb` program is not installed: skip "What to do now" and follow "Installing kb" below. If the output instead is an error followed by a `kb` version line, `kb` is installed but `kb board start` failed: show the user the error (if the database is unreachable, Docker Desktop is probably not running) and stop.

What to do now:

1. If the output above lists earlier boards ("Recent boards that may belong to this conversation"), no board exists yet. Show those boards to the user and ask whether to continue one of them (`kb board adopt <board id> --session ${CLAUDE_SESSION_ID}`) or to start a new one (`kb board start --new --session ${CLAUDE_SESSION_ID}`). Run the command the user chooses, then go on with step 2.
2. Build the board from the discussion you had with the user so far, as the guide above says: `kb board init` with a title, a goal, lanes and epics; cards with `--agreed` for what the user already agreed; cards with `--discuss` for topics still under discussion. Do not invent work the user did not agree to.
3. The user's hint for the board's title or goal (empty means no hint): $ARGUMENTS
4. Then continue the work, and keep the board current as the guide says.

Every `kb` command finds this session by itself. If one reports `no session`, add `--session ${CLAUDE_SESSION_ID}`.

Installing kb:

Tell the user, in the user's language, that `kb` is not installed, and that installing it means: cloning https://github.com/wustar576/tui-kanban, starting a PostgreSQL container in Docker on 127.0.0.1:55432 (with generated passwords), and a Rust build of a few minutes that puts `kb` in `~/.cargo/bin`. Ask whether to install it, and do nothing until the user says yes. After a yes:

1. Check the prerequisites: `git --version`; `cargo --version` (Rust 1.94 or later); `docker info` (Docker Desktop running); on Windows also `pwsh --version` (PowerShell 7) and Git Bash. If one is missing, tell the user what to install and stop. Do not install them yourself.
2. If the current folder is a tui-kanban checkout (it has `crates/kb` and `scripts/setup.sh`), use it. Otherwise ask where to clone it, suggesting `~/src/tui-kanban`, then run `git clone https://github.com/wustar576/tui-kanban.git <folder>`.
3. In that folder, run the setup script: `bash scripts/setup.sh` on macOS, `pwsh scripts/setup.ps1` on Windows. It builds for several minutes, so give it a long timeout. If it stops with an error, show the user its last lines and stop; do not rerun it with `--yes` or `-Yes`.
4. In the same folder, run `cargo install --path crates/kb --locked`.
5. Run `kb --version`. If `kb` is not found, tell the user to put `~/.cargo/bin` on `PATH` and restart Claude Code, and stop.
6. Run `kb board start --session ${CLAUDE_SESSION_ID}` and continue with "What to do now" above, using its output.
7. Tell the user that no restart is needed: the hooks find `kb` in `~/.cargo/bin` from now on. If a later session still starts with the notice that `kb` was not found, restart Claude Code from a new terminal.

Never read `.env` or `~/.config/kb/config.toml`, and never print the passwords.
