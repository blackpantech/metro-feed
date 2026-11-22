---
name: validations-suite
description: Run the metro-feed validation suite (the "Apply-change finishing sequence" from CLAUDE.md) — cargo fmt, clippy with -D warnings, workspace tests, release tests with test-utils, and clippy pedantic — fixing every issue before moving to the next command. Use after finishing a commit or feature, before committing, or when the user asks to "validate", "run the checks", "run the validation suite", or "run the finishing sequence".
allowed-tools: Bash(cargo fmt:*), Bash(cargo clippy:*), Bash(cargo test:*), Read, Edit, Grep, Glob
---

# Validations Suite

Run the project's finishing sequence (CLAUDE.md → "Apply-change finishing sequence", mirrored in `openspec/config.yaml` → `operations.apply.guidance`) from the repository root.

## Sequence

Run these commands **in order, one at a time**:

| # | Command | Purpose |
|---|---------|---------|
| 1 | `cargo fmt` | Apply formatting |
| 2 | `cargo clippy --workspace --all-targets --features test-utils -- -D warnings` | Lint gate (CI hard-fails on warnings) |
| 3 | `cargo test --workspace` | Unit tests (test-utils-gated modules run zero tests here) |
| 4 | `cargo test --workspace --release --features test-utils` | Full suite incl. DB-backed tests, release mode |
| 5 | `cargo clippy --workspace --all-targets --features test-utils -- -W clippy::pedantic` | Pedantic lint pass |

## Rules

1. **Fix before advancing.** If a command reports errors or warnings, fix them, then re-run *that same command* until it is clean. Only then move to the next one. Never batch fixes across steps.
2. **Restart when fixes are invasive.** If a fix in a later step changes code meaningfully (not just a lint tweak), re-run from step 1 so earlier gates still hold.
3. **Fixes must respect CLAUDE.md conventions:** no `.unwrap()` in production code, avoid `.clone()`, no `crate::`/`super::` in production code (import explicitly at the top), keep docs up to date, pick `MetroError` variants by their exit-code contract.
4. **Do not silence lints lazily.** Prefer real fixes. Only use `#[expect(clippy::..., reason = "...")]` when the lint is genuinely inapplicable, always with a concrete `reason`. Not-yet-used code gets `#[cfg_attr(not(test), expect(dead_code, reason = "..."))]` per the apply guidance.
5. **Pedantic step (5):** fix pedantic findings in code touched by the current change. If findings are in untouched, pre-existing code, report them rather than refactoring unrelated modules, unless the user asks otherwise.
6. **Never weaken tests** (deleting, `#[ignore]`-ing, or loosening assertions) to make a step pass. If a failure looks like a real bug or is pre-existing and out of scope, stop and report it.
7. **Network:** tests must not hit the network. If a failure is caused by the environment (no network, sandbox, missing toolchain), classify it as an environment limitation rather than "fixing" code.

## Reporting

When done, report a short summary per step:

- ✅ passed (first try) / 🔧 passed after fixes (list what was fixed, with `file:line`) / ❌ failed (with the relevant output)
- Distinguish **success**, **pre-existing failure**, and **environment limitation**.
- Never report a step as passing if it was not run or its output was not observed.
