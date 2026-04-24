# Archive Session Noise Filter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Exclude low-value archive sessions such as greetings, boilerplate setup prompts, and commit-message generator chats from exported knowledge-base data.

**Architecture:** Add one shared archive filter module that inspects parsed `ChatSession` data before archive writes. Reuse that decision in `export`, `watch`, and `run --archive-dir`, while leaving provider parsing and project-local `.waylog/history` behavior intact.

**Tech Stack:** Rust, tokio, existing provider/session/archive pipeline

---

### Task 1: Add the shared archive filter

**Files:**
- Create: `src/session_filter.rs`
- Test: `src/session_filter.rs`

- [ ] Define archive skip reasons for greeting sessions, shell probes, boilerplate setup prompts, empty untitled chats, and commit-message generators.
- [ ] Add a shared `session_title()` helper so archive naming and filtering use the same first-user-message logic.
- [ ] Add focused unit tests that prove noisy sessions are skipped while meaningful sessions are kept.

### Task 2: Wire the filter into archive writes

**Files:**
- Modify: `src/archive/mod.rs`
- Modify: `src/commands/export.rs`
- Modify: `src/commands/watch.rs`
- Modify: `src/synchronizer.rs`

- [ ] Make archive export return whether a session was written, unchanged, or filtered.
- [ ] Skip filtered sessions before creating archive files or index rows.
- [ ] Surface filtered counts/reasons in export/watch logging without treating them as hard failures.
- [ ] Keep project-local sync behavior intact when `run --archive-dir` filters archive output.

### Task 3: Document and verify the behavior

**Files:**
- Modify: `README.md`
- Modify: `README_zh.md`
- Modify: `overview.md`

- [ ] Document the default noise categories that are excluded from archive output.
- [ ] Explain that the filter applies to `export`, `watch`, and `run --archive-dir`.
- [ ] Run `cargo test` and `cargo clippy --all-targets --all-features -- -D warnings`.
