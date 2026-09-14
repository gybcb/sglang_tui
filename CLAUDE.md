# Rust Agent Rules

## Build Strategy

- Never run `cargo test --workspace` unless explicitly requested.
- Never use `--all-features` for routine development.
- After a change, prefer `cargo check -p <affected-package>`.
- Only test packages affected by the change.
- Do not run full workspace checks for single-crate changes.

## Verification

Preferred order:

1. cargo fmt --check
2. cargo check -p <affected-package>
3. targeted cargo test
4. cargo clippy -p <affected-package> --all-targets -- -D warnings

Run workspace-wide checks only before final delivery.

## Editing

- Make small, incremental changes.
- Do not refactor unrelated code.
- Do not change public APIs unless required.
- Do not introduce abstractions before they are necessary.

## Search Tools

Prefer modern fast search tools:

- Use `rg` instead of `grep -R` or recursive grep.
- Use `fd` instead of `find` when searching for files.
- Use `rg -g` to restrict searches by file type.
- Avoid searching `target/`, `.git/`, node_modules, generated files, and other build artifacts.
- Never recursively search the entire repository unless necessary.

## Rust

- Never search `target/` with rg.
- Prefer `rg -g '*.rs'` for Rust source searches.
- Prefer `cargo check -p <package>` over workspace-wide checks.

## Command Output

- Avoid commands that produce huge output.
- Prefer targeted tests and package-scoped checks.
- Do not repeatedly rerun the same expensive command after unrelated edits.

## Context

- Keep the active task focused.
- If the session becomes large, compact the context.
- Do not rediscover the project structure after it has already been established.