# cq Backlog

## Best practices: guide orchestrators on prompt style
**Priority:** Medium — UX / docs

Add guidance in `cq --help` (and README) that orchestrators should pass intent to sub-agents concisely — close to how a human would phrase it — rather than over-specifying step-by-step instructions. Sub-agents have full codebase access and can read project context (CLAUDE.md, AGENTS.md) themselves.

When a sub-agent gets stuck or needs clarification, it should finish with a question in its output, which the orchestrator surfaces via `cq tail` and can follow up with `cq resume`. The workflow is conversational, not fire-and-forget-with-perfect-instructions.

Anti-pattern: stuffing 20 lines of step-by-step instructions into the prompt.
Good pattern: "rebase studio#640 onto #637 and update the PR base branch" + let the agent figure out the details.

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

## Bug: session marked failed after long-running command
**Priority:** High — correctness

**Symptom:** a session ran `corepack pnpm lint` (via `turbo run lint`) for ~25 minutes and the command completed, producing only a Node deprecation warning. The agent then emitted a thinking entry, produced no further output for ~24 minutes, and the session was marked as failed. No explicit error was reported.

Status update:
- added a deterministic reproducer in `tests/backend_e2e.rs` where the backend emits normal output, stays quiet, then exits non-zero
- narrowed likely root cause to backend/CLI exit semantics after successful transcript output rather than cq's own inactivity timeout logic
- `cq` now classifies such runs from transcript/stderr artifacts instead of trusting a lone non-zero exit code

Still worth checking with a real Claude repro whether the backend is exiting non-zero after idle/thinking periods, and if so why.

## Bug: Claude Code permissions not mapping correctly to cq policy
**Priority:** High — correctness

Resolved in this branch.

**Root cause:** Claude Code permission entries like `Edit(/../**)` and `Write(/../**)` were parsed into regexes that expected paths starting with `/../...`, but canonical `Edit`/`Write` tool inputs use parent-relative paths like `../...` (no leading slash). That mismatch caused policy lookup to miss and fall through to supervisor escalation.

**Fix:** Normalize Claude Code parent-relative path permissions during policy derivation so `/../**` is matched against canonical tool inputs as `../**`. Added regression tests around permission parsing plus `Edit`/`Write` policy matching.

## Bug: resumed session spins at 100% CPU with no output
**Priority:** High — correctness

**Repro:** `cq resume studio-stack "question"` on a completed session. The claude process launched (`claude -p --session-id d0e1ba77...`) pegged CPU at 100% indefinitely. No log file was created for the new session ID (`9814a395`), and the original session log already had `"last-prompt"` as its final entry. The process had to be killed manually.

Possible cause: resuming a session that's already at `last-prompt` state may cause Claude Code to spin. Or the session-id + new prompt combination creates an invalid state. Need to investigate how `claude -p --session-id X "new prompt"` behaves when the session is already terminated.


## `cq list` prompt column should truncate at first newline
**Priority:** Low — UX

`cq list` truncates the prompt by character count, but multi-line prompts show partial second lines that are hard to read. Truncate at the first newline instead (then apply character limit to that first line).

## cq watch: hide old completed sessions
**Priority:** Medium — UX

`cq watch` shows all sessions including long-completed ones, filling the screen. Sessions that completed/failed more than 5 minutes ago should be omitted from the watch view. Could add a `--all` flag to show everything if needed.

## `cq start --name` should resume existing sessions, deprecate `cq resume`
**Priority:** High — UX

When `cq start --name X "prompt"` is called and a session named X already exists:
- If **completed/failed**: resume it with the new prompt (preserving context)
- If **running**: queue the prompt for delivery when it finishes (see resume queuing item)
- If **no session exists**: start a new one (current behavior)
- Add `--new` flag to force a fresh session even if one exists by that name

This makes `--name` a stable handle for a work stream. Remove `cq resume` entirely — `cq start --name` covers all cases.

For adopting pre-existing (non-cq) Claude sessions: `cq start --name X --session <claude-uuid> "prompt"` — resumes that Claude session but tracks it under the cq name going forward. The `--session` flag is only needed the first time; after that `--name X` auto-resumes.

In help text and README, emphasize that session reuse is the **default and preferred** pattern — the agent keeps its context, knows what it already did, and picks up where it left off. Only use `--new` when the old context is too large or no longer relevant.

## Reduce unwrap() calls at I/O boundaries
**Priority:** Low — robustness

67 `unwrap()` calls across 5 files. For a CLI tool this is generally acceptable, but converting panics at I/O boundaries to proper error messages (via `anyhow` or `miette`) would improve UX when things go wrong (bad permissions, missing files, etc.).

## Orchestrator approval transparency
**Priority:** High — safety / UX

Orchestrators (parent agents) should surface what they're approving rather than blindly running `cq approve all`. A human watching the orchestrator's output should always see what's being approved, not just "Approved 3 pending tool calls".

Implemented so far:
- `cq approve all` now prints each approved call's details (tool, session, description) to stderr
- `cq approve all` without `--session` prints a warning recommending session-scoped approval
- `cq approve "summary text"` documented in README as the recommended orchestrator pattern

Still to consider:
- `cq pending --json` enrichment: include session name in JSON output so orchestrators can display it without a second lookup
- `--confirm` flag or interactive prompt for `cq approve all` without `--session` (currently just warns)
- Whether `cq approve all` should hard-require `--session` scope (breaking change, needs migration path)

## Session performance metrics / observability
**Priority:** High — observability

Add timing metrics so orchestrators (and humans) can see where time is being spent:
- **Approval wait time**: how long each tool call sat in pending before being approved/denied
- **Work time**: time between approvals (i.e. how long the agent spent working between tool calls)
- **Total session duration** and **active vs waiting** breakdown
- Surface in `cq list` (maybe a `--stats` flag) and `cq result`

This would answer "why is this session taking so long?" — is it blocked on approvals, or doing expensive work (yarn install, cargo build, etc.)? Surface in `cq list`, `cq tail`, and/or a dedicated stats view.

## Supervisor: include known Claude Code tools in system prompt
**Priority:** Medium — correctness

The supervisor doesn't know about Claude Code's built-in tools (ToolSearch, NotebookEdit, Agent, etc.) and flags them as "non-existent" when escalating. Add a list of known Claude Code tool names to the supervisor system prompt so it can make informed decisions instead of treating them as suspicious.

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
- ~~Bug: completed sessions stuck as "running"~~
- ~~Approve with regex filter (`--match`)~~
- ~~Supervisor audit log~~
- ~~PR checks: branch protection requiring CI~~
- ~~Hook output format fix (permissionDecisionReason)~~
- ~~Supervisor escalate-only (no deny)~~
- ~~Supervisor enabled by default with sensible system prompt~~
- ~~Approve by summary text~~
- ~~Self-update command (cq update)~~
- ~~Audit log: show session names instead of IDs~~
- ~~Audit log: show bash command contents~~
- ~~Audit log: show tool call details inline (`--verbose` flag)~~
- ~~Derive policies from Claude Code permissions~~
- ~~Config: resolve project root from worktrees~~
- ~~Supervisor: omit agent prompt from context by default~~
- ~~README: prerequisites and contributing~~
- ~~Bug: `cq result` returns resume response instead of original session output~~
- ~~`cq tail` includes final stdout output for completed sessions~~
- ~~Bug: `cq tail` sanitizes control characters in rendered output~~
- ~~Bug: `cq pending` shows session IDs instead of names~~
- ~~CLI: show effective config (`cq config show`)~~
- ~~`cq list` prompt column should truncate at first newline~~
- ~~cq watch: hide old completed sessions~~
- ~~Session expiration (`cq gc` resolves stale running sessions)~~
- ~~DB cleanup (`cq gc --older-than` prunes old sessions, tool calls, and log files)~~
- ~~Bug: `cq resume` should support native Claude session IDs correctly~~
