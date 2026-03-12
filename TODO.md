# cq Backlog

## Approval TUI / UI
**Priority:** High — UX

Interactive approval interface that follows pending tool calls and lets the user approve/deny inline.

Options (in order of complexity):
1. **CLI TUI** — `cq approve --interactive` or `cq tui`. Streams pending calls as they arrive, shows the supervisor summary, user hits enter to approve or `d` to deny. Minimal deps (crossterm or similar).
2. **Native macOS UI** — SwiftUI menu bar app or floating window. Shows pending calls with approve/deny buttons, notifications on escalation. Much nicer UX than terminal — could show rich diffs, file context, etc.

Either way the core loop is: poll `cq pending --json`, present, write back `cq approve`/`cq deny`.

## Live agent activity stream
**Priority:** High — observability

Real-time unified stream of what all agents are doing — reasoning, tool calls, outputs — across all active sessions. Think `cq watch` but with full activity detail, not just status.

Possible approaches:
- Tap into Claude Code's streaming output per session (the raw JSONL logs in `~/.claude/projects/`)
- `cq stream [--session NAME]` that tails all (or one) agent's activity
- Unified feed: interleave events from all agents with session labels, color-coded
- Could feed into the TUI as a split pane (approvals on one side, activity on the other)

## PR checks integration
**Priority:** Medium — workflow

Automatically run checks on PRs using cq-managed agents. E.g. `cq pr-check 123` spins up agents to review code, run tests, check for regressions, and post results as PR comments. Could integrate with GitHub Actions or run locally. The orchestrator pattern is already there — this is just a pre-built workflow on top of it.

## Multi-agent backend support
**Priority:** Medium — extensibility

Extract "Claude Code" as one agent backend behind a trait/interface. Support other coding agents:
- **Codex** (OpenAI) — if it exposes a hook or approval mechanism
- **opencode** — similar, need to investigate their extension points
- **Direct API mode** — bypass CLI, call Claude API directly for lower latency supervisor calls

The abstraction: an agent backend needs to support `start(prompt, cwd) -> session`, `resume(session, prompt)`, `kill(session)`, and a hook mechanism for intercepting tool calls. Each backend implements this differently.

## Supervisor: direct API mode
**Priority:** Low — performance

Currently the supervisor calls `claude -p` which has CLI startup overhead on every tool call. Call the Claude API directly via HTTP for lower latency. Could use the Anthropic SDK or raw curl.

## Derive policies from Claude Code permissions
**Priority:** High — zero-config UX

cq should work well OOTB without any `.cq/config.json`. Instead of maintaining a separate policy config, read the user's existing Claude Code permission settings (`.claude/settings.json`, `~/.claude/settings.json`) to derive tool-call policies automatically. If Claude Code already trusts `Edit` and `Bash`, cq should too. This eliminates the need for users to configure permissions twice and makes cq a drop-in addition to any Claude Code workflow.

Fallback: if no Claude Code settings are found, use the current default policies (read-only tools auto-allowed, everything else goes to supervisor).

## Config: resolve project root from worktrees
**Priority:** High — UX

When running in a git worktree, `.cq/config.json` isn't present because it's untracked. The hook should find the project config by either:
- Walking up the directory tree looking for `.cq/config.json` (simple but could be slow in hook hot path)
- Having `cq start` resolve the project root at launch time and pass it via `CQ_PROJECT_DIR` env var (faster, single resolution)

This would eliminate the need to manually copy `.cq/config.json` into each worktree.

## Session expiration
**Priority:** Medium — hygiene

Auto-expire sessions that have been running longer than the configured timeout (default 24h). Mark them as "expired" in the DB. Could run as part of `cq list` or as a separate `cq gc` command.

## End-to-end tests for hook and supervisor
**Priority:** Medium — correctness

Add integration tests that exercise the full hook → policy → supervisor → pending → approve/deny flow. Key cases:
- Policy allow: tool call auto-approved, never hits supervisor
- Policy deny: tool call blocked immediately
- Supervisor escalate: tool call lands in pending, can be approved/denied by orchestrator
- Supervisor approve: tool call auto-approved
- Hook output format: verify Claude Code actually honors deny (regression test for the `permissionDecisionReason` fix)
- Approve by summary: `cq approve "summary text"` matches and approves the right call
- Timeout: pending call times out and returns deny to the agent

These would ideally run in CI using a mock or lightweight supervisor (no real LLM calls).

## DB cleanup
**Priority:** Medium — hygiene

`cq gc` (or `cq cleanup`) to prune old sessions and tool calls from the DB. Options: `--older-than 7d` to remove sessions older than N days, `--status completed` to only remove finished sessions. Keeps the DB small and fast over time.

---

## Done

- ~~Pending --wait: long-polling for approvals~~
- ~~Pending --wait: fix ignoring already-pending calls~~
- ~~Pending show: full tool call detail view~~
- ~~List: filter by session name~~
- ~~Pending: show which file is being written~~
- ~~Approve all scoped to session~~
- ~~Approve all by tool type (`--tool` flag)~~
- ~~Resume is broken~~
- ~~Result/output broken for resumed sessions~~
- ~~Policy: conditional Bash approval~~
- ~~Policy add with pattern from CLI~~
- ~~Supervisor LLM for auto-approving/denying tool calls~~
- ~~Pending --wait: emit JSON mode~~
- ~~Session status notification (`cq wait`)~~
- ~~Approve with regex filter (`--match`)~~
- ~~Supervisor audit log~~
