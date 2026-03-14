# cq (claude-queue)

[![CI](https://github.com/wiggzz/claude-queue/actions/workflows/ci.yml/badge.svg)](https://github.com/wiggzz/claude-queue/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Orchestrate multiple Claude Code sessions from a single place.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/wiggzz/claude-queue/main/install.sh | sh
```

Or build from source:

```sh
cargo install --path .
```

## Usage

```
# Start named sessions
cq start "fix the auth bug" --name auth --cwd ~/myproject
cq start "add user tests" --name tests --cwd ~/myproject

# Check what needs approval
cq pending

# Approve or deny tool calls
cq approve all --session auth         # scoped to one session (recommended)
cq approve "Edit src/main.rs"         # approve by summary text match
cq approve all --tool Bash            # approve all Bash calls
cq approve all --match "cargo (build|test)"
cq approve 5                          # approve by ID
cq deny 5 --reason "don't touch that file"

# Check status and results
cq list
cq result auth
cq result tests

# Wait for a session to finish
cq wait auth

# Continue a conversation
cq resume auth "now add a test for the fix"

# Live dashboard
cq watch

# Audit log — see every tool call decision
cq audit
cq audit --follow   # real-time tail
cq audit --json     # machine-readable
```

## How it works

Sub-agents run as `claude -p` processes with a PreToolUse hook that intercepts every tool call. Read-only tools (Read, Glob, Grep) are auto-approved by default. Everything else blocks until you approve or deny via `cq approve`/`cq deny`.

## Policies

Policies are configured in `~/.cq/config.json` (user) and `.cq/config.json` (project). Project policies take priority. First match wins.

```json
{
  "policies": [
    {"tool": "Read", "action": "allow"},
    {"tool": "Bash", "action": "allow", "pattern": "^(cargo |git (status|diff|log))"},
    {"tool": "Bash", "action": "deny", "pattern": "rm -rf"},
    {"tool": "*", "action": "ask"}
  ]
}
```

To keep session state in the project instead of `~/.cq/cq.db`, set:

```json
{
  "db": {
    "location": "project_local"
  }
}
```

With `project_local`, cq stores the SQLite DB at `.cq/cq.db` in the resolved project root. `CQ_DB` still overrides this for tests or one-off runs.

## Supervisor

Enable an LLM supervisor to auto-approve/deny/escalate tool calls based on natural language rules:

```json
{
  "supervisor": {
    "enabled": true,
    "model": "haiku",
    "rules": [
      "Approve build and test commands",
      "Deny network requests and system modifications",
      "Escalate anything ambiguous"
    ]
  }
}
```

The supervisor runs after static policies. If it escalates, the call falls through to human approval.

## Orchestration patterns

### Non-blocking approval loop

When orchestrating from a parent Claude Code session, **never block the main loop** waiting for approvals. Instead:

```
# GOOD: check and approve in one shot, then move on
cq pending
cq approve all --session myagent

# GOOD: use audit --follow in a background task to monitor
cq audit --follow

# BAD: don't block on --wait in your main loop
# cq pending --wait   ← blocks until a call arrives, freezing your session
```

`cq pending --wait` is designed for **background tasks and scripts only** — it blocks until a pending call appears, which defeats the purpose of an orchestrator or supervisor session that needs to do other work. Use it in a background process or external script, never as the main loop of an interactive session.

### Transparent approvals

A human watching the orchestrator should always see **what** is being approved. `cq approve all` prints each approved call's details to stderr:

```
  ✓ [auth] Bash — $ cargo test
  ✓ [auth] Edit — [src/lib.rs] pub fn authenticate...
Approved 2 pending tool call(s) for session auth.
```

**Recommended approval patterns for orchestrators** (most to least specific):

1. **By summary text** — `cq approve "Edit src/main.rs"` matches the supervisor's summary. Best for targeted approvals where the orchestrator knows exactly what to expect.
2. **By regex** — `cq approve all --match "cargo (build|test)"` approves calls matching a pattern.
3. **By session + tool** — `cq approve all --session auth --tool Bash` scoped to one session and tool type.
4. **By session** — `cq approve all --session auth` approves everything for one agent.
5. **Global** — `cq approve all` approves everything (prints a warning recommending `--session`).

### Parallel agents with worktrees

```
git worktree add -b feature-a ../project-feature-a HEAD
git worktree add -b feature-b ../project-feature-b HEAD

cq start "implement feature A" --name feature-a --cwd ../project-feature-a
cq start "implement feature B" --name feature-b --cwd ../project-feature-b

# Monitor with audit log
cq audit --follow
```

**Note:** If you use `.cq/config.json` for project-level policies, copy it to each worktree — worktrees don't share untracked files.

Run `cq --help` for full details.
