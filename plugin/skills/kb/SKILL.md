---
name: kb
description: Start the kanban board for this session. The user runs /kb after discussing the work with you; you then build the board from that discussion.
disable-model-invocation: true
argument-hint: "[title or goal hint]"
allowed-tools: Bash(kb board *) Bash(kb epic *) Bash(kb lane *) Bash(kb card *) PowerShell(kb board *) PowerShell(kb epic *) PowerShell(kb lane *) PowerShell(kb card *)
---

The user just started the kanban board for this session. `kb board start` printed this:

!`kb board start --session ${CLAUDE_SESSION_ID}`

What to do now:

1. If the output above lists earlier boards ("Recent boards that may belong to this conversation"), no board exists yet. Show those boards to the user and ask whether to continue one of them (`kb board adopt <board id> --session ${CLAUDE_SESSION_ID}`) or to start a new one (`kb board start --new --session ${CLAUDE_SESSION_ID}`). Run the command the user chooses, then go on with step 2.
2. Build the board from the discussion you had with the user so far, as the guide above says: `kb board init` with a title, a goal, lanes and epics; cards with `--agreed` for what the user already agreed; cards with `--discuss` for topics still under discussion. Do not invent work the user did not agree to.
3. The user's hint for the board's title or goal (empty means no hint): $ARGUMENTS
4. Then continue the work, and keep the board current as the guide says.

Every `kb` command finds this session by itself. If one reports `no session`, add `--session ${CLAUDE_SESSION_ID}`.
