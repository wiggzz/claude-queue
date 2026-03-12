# cq Backlog

## Supervisor: support non-Claude models
**Priority:** Low — future extensibility

Currently the supervisor calls `claude -p`. To support other models/providers, abstract the LLM call behind a trait or config option (e.g. `provider: "anthropic"` vs `provider: "openai"`). Could also support a direct API mode using curl/HTTP client for lower latency.

## Supervisor: audit log
**Priority:** Medium — observability

Log every supervisor decision (approve/deny/escalate) and human decision to a file or DB table. Include timestamp, session, tool name, tool input, decision, reason, and who decided (supervisor vs human). Essential for debugging and trust-building.

## Config: resolve project root from worktrees
**Priority:** High — UX

When running in a git worktree, `.cq/config.json` isn't present because it's untracked. The hook should find the project config by either:
- Walking up the directory tree looking for `.cq/config.json` (simple but could be slow in hook hot path)
- Having `cq start` resolve the project root at launch time and pass it via `CQ_PROJECT_DIR` env var (faster, single resolution)

This would eliminate the need to manually copy `.cq/config.json` into each worktree.

## Session expiration
**Priority:** Medium — hygiene

Auto-expire sessions that have been running longer than the configured timeout (default 24h). Mark them as "expired" in the DB. Could run as part of `cq list` or as a separate `cq gc` command.

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
