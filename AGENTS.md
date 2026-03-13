# Agents

## Before pushing

Always run all CI checks locally before pushing:

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

Fix any issues before pushing. Do not push code that fails these checks.

## Working style

- Work in a git worktree or on a branch — never commit directly to main
- Keep prompts and PRs focused on one thing
- It's OK to stop and ask for clarification rather than guessing
