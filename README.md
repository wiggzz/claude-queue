# cq (claude-queue)

[![CI](https://github.com/wiggzz/claude-queue/actions/workflows/ci.yml/badge.svg)](https://github.com/wiggzz/claude-queue/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Orchestrate multiple Claude Code sessions from a single place.

## Install

```
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
cq approve all
cq approve all --tool Bash
cq approve all --match "cargo (build|test)"
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
cq approve all --tool Bash

# GOOD: use audit --follow in a background task to monitor
cq audit --follow

# BAD: don't block on --wait in your main loop
# cq pending --wait   ← blocks until a call arrives, freezing your session
```

`cq pending --wait` is designed for **background tasks and scripts only** — it blocks until a pending call appears, which defeats the purpose of an orchestrator or supervisor session that needs to do other work. Use it in a background process or external script, never as the main loop of an interactive session.

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
