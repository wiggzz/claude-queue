# cq Backlog

## Pending --wait: long-polling for approvals
**Priority:** High — needed for orchestrator use case

`cq pending --wait` blocks until a new pending tool call appears, prints it, exits.
- Scoped variant: `cq pending --wait --session <id>`
- Could use fs watching on the DB or internal poll
- Enables event-driven orchestrator loop instead of shell polling

## Pending show: full tool call detail view
**Priority:** High — safety gap

`cq pending show <id>` displays complete tool call input (untruncated JSON).
- Current table view truncates input, making it hard to safely review Bash commands
- Alternative: `cq pending --full` to show all pending with full input

## List: filter by session name
**Priority:** Medium — convenience

`cq list --session <name>` to filter the list view. Tried `cq list --session cq-discover` and got an unexpected argument error. Should support filtering by name or ID prefix.

## Pending: show which file is being written
**Priority:** Medium — safety context

For Write/Edit tool calls, `cq pending` should show the `file_path` prominently (not buried in truncated JSON). Knowing *what file* is being written is often enough to approve without needing the full content.

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

Depends on: `pending --wait`, `pending show <id>` (supervisor needs full tool call details)

## Approve all scoped to session
**Priority:** Medium — usability

`cq approve all --session <name-or-id>` to bulk-approve all *currently pending* calls for a specific session only. Current `cq approve all` is too broad — it approves across all sessions.

## Resume is broken
**Priority:** High — bug

`cq resume <name>` fails with "No conversation found with session ID: <original-id>". The resumed `claude --resume` process can't find the original session transcript. Likely a mismatch between how cq tracks session IDs and where Claude Code stores JSONL transcripts.

## Result/output broken for resumed sessions
**Priority:** High — bug

`cq result <name>` after a resume looks up the original session ID, not the new one. Also `cq output` returns nothing for failed sessions. Need better error surfacing.

## Policy: conditional Bash approval
**Priority:** Low — stretch goal

Currently Bash is all-or-nothing in policies. Would be useful to have patterns, e.g. auto-approve Bash commands matching `ls`, `tree`, `find`, `cat` but gate `rm`, `git push`, `cargo install`, etc.
