# Unified Archive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a cross-platform unified archive that supports host-wide export plus real-time archive updates during `waylog run`.

**Architecture:** Keep the existing project `.waylog/history` flow, and add a parallel archive target that writes flattened triplets (`.md`, `.raw.*`, `.meta.json`) plus machine-friendly indexes. Extend provider scanning so Claude and Codex can enumerate both project-scoped sessions and host-wide sessions without breaking existing `pull` behavior.

**Tech Stack:** Rust, clap, tokio, serde/serde_json, chrono, walkdir, tempfile

---

### Task 1: Archive layout and naming

**Files:**
- Create: `src/archive/mod.rs`
- Create: `src/archive/layout.rs`
- Create: `src/archive/meta.rs`
- Test: `src/archive/layout.rs`

- [ ] Step 1: Write failing tests for readable base naming, collision suffixing, and archive paths
- [ ] Step 2: Run `cargo test archive:: -- --nocapture` and verify the new tests fail for missing module/symbols
- [ ] Step 3: Implement archive layout helpers and metadata model with minimal logic
- [ ] Step 4: Re-run `cargo test archive:: -- --nocapture` and verify the tests pass

### Task 2: Export command and host scan support

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `src/commands/mod.rs`
- Create: `src/commands/export.rs`
- Modify: `src/providers/base.rs`
- Modify: `src/providers/claude.rs`
- Modify: `src/providers/codex.rs`
- Modify: `src/providers/mod.rs`
- Test: `src/archive/mod.rs`

- [ ] Step 1: Write failing tests for archive export writing markdown/raw/meta and incremental overwrite behavior
- [ ] Step 2: Run targeted `cargo test archive::export -- --nocapture` and confirm failure
- [ ] Step 3: Implement `export` command, scan scope support, and archive writer
- [ ] Step 4: Re-run targeted tests and confirm pass

### Task 3: Real-time archive updates during run

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/commands/run/mod.rs`
- Modify: `src/commands/run/cleanup.rs`
- Modify: `src/synchronizer.rs`
- Modify: `src/watcher/file_watcher.rs`
- Test: `src/synchronizer.rs`

- [ ] Step 1: Write failing tests for synchronizer dual-write to project markdown and archive outputs
- [ ] Step 2: Run targeted `cargo test synchronizer:: -- --nocapture` and confirm failure
- [ ] Step 3: Implement optional archive target plumbing for watcher/final sync
- [ ] Step 4: Re-run targeted tests and confirm pass

### Task 4: Verification and docs

**Files:**
- Modify: `README.md`
- Modify: `README_zh.md`

- [ ] Step 1: Run `cargo fmt`
- [ ] Step 2: Run `cargo test`
- [ ] Step 3: Run `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] Step 4: Update docs for `export` and `run --archive-dir`
