# cq Backlog

## Bug: pending --wait ignores already-pending calls
**Priority:** High — usability bug

`cq pending --wait` only watches for *new* pending calls. If there are already pending calls when `--wait` starts, they're silently ignored. It should print existing pending calls immediately and only enter the poll loop if the queue is empty. Discovered while using `--wait` to monitor a cq agent — a pending Bash call sat unnoticed.

## Supervisor agent for approval loop
**Priority:** High — key UX unlock

Instead of the user manually approving every tool call, run a supervisor agent that watches pending calls and makes approval decisions with judgment.

Design:
- Supervisor is itself a Claude session (launched via `cq start` or inline)
- Watches `cq pending --wait` in a loop (depends on that feature)
- For each pending call, evaluates: is this within scope? Is it destructive? Does it touch files outside the expected repo?
- Has context about what the worker agent is supposed to be doing (task description, target repo, expected file paths)
- Auto-approves routine dev work: reads, edits in target repo, build/test commands
- Escalates to user only for genuinely risky actions: git push, touching other repos, rm -rf, anything off-task
- Prompt template: "You are reviewing tool calls for an agent working on [feature X] in [repo Y]. Approve normal development activity. Escalate anything destructive, out-of-scope, or suspicious."

This collapses the manual approve-poll loop into something ~95% autonomous. The user only gets pulled in for real decisions.

Depends on: `pending --wait`, `pending show <id>` (supervisor needs full tool call details) — both now implemented.

## Batch approve by tool type
**Priority:** High — needed for orchestrator workflow

`cq approve all --tool Bash` or `cq approve all --tool "Bash,Agent"` to approve all pending calls matching specific tool types. When managing 8 parallel agents, most pending calls are `cargo build` or `git diff` — approving by tool type is faster than reviewing each one individually.

Could combine with `--session` for even more control: `cq approve all --session auth-fix --tool Bash`.

## Pending --wait: emit JSON mode
**Priority:** Medium — automation

`cq pending --wait --json` to output new pending calls as JSON instead of table format. Makes it easier for scripts and supervisor agents to parse tool call details programmatically.

## Session status notification
**Priority:** Medium — UX

`cq wait <name-or-id>` blocks until a session completes, then prints its status and result. Useful for orchestrators that need to know when an agent finishes before proceeding (e.g., waiting for dependencies before starting the supervisor agent).

## Approve with regex filter
**Priority:** Medium — power user

`cq approve all --match "cargo (build|test)"` to approve all pending Bash calls whose command matches a regex. More surgical than `--tool` filtering and doesn't require a persistent policy.

## Policy add with pattern from CLI
**Priority:** Low — convenience

`cq policy add Bash allow --pattern "^(ls|tree|cargo build|cargo test)"` to add conditional policies directly from the CLI instead of editing config.json. The `pattern` field is already supported in config but there's no CLI flag for it yet.

---

## Done

- ~~Pending --wait: long-polling for approvals~~
- ~~Pending show: full tool call detail view~~
- ~~List: filter by session name~~
- ~~Pending: show which file is being written~~
- ~~Approve all scoped to session~~
- ~~Resume is broken~~
- ~~Result/output broken for resumed sessions~~
- ~~Policy: conditional Bash approval~~
