# cq (claude-queue)

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
cq deny 5 --reason "don't touch that file"

# Check status and results
cq list
cq result auth
cq result tests

# Continue a conversation
cq resume auth "now add a test for the fix"

# Live dashboard
cq watch
```

## How it works

Sub-agents run as `claude -p` processes with a PreToolUse hook that intercepts every tool call. Read-only tools (Read, Glob, Grep) are auto-approved by default. Everything else blocks until you approve or deny via `cq approve`/`cq deny`.

Policies are configured in `~/.cq/config.json` (user) and `.cq/config.json` (project).

Run `cq --help` for full details.
