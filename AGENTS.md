# Agents

## Working style

- Use `cq` (this CLI tool) to dispatch sub-agents and manage them (check `cq --help` to see best how to use this tool).

## Testing

Always use a red-green loop to write tests. A test which does not fail for the right reason before turning green is useless - it may not work, it may pass trivially. Ensure that tests fail for the right reason before committing them. If you need to, comment out or delete relevant code and ensure the tests fail for the expected reason before writing or bringing back the code and checking that they pass.

## Before pushing

Always run all CI checks locally before pushing:

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

Fix any issues before pushing.

Commit using conventional commits.
