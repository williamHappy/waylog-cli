# WayLog CLI

[![GitHub license](https://img.shields.io/github/license/shayne-snap/waylog-cli?style=flat-square)](https://github.com/shayne-snap/waylog-cli/blob/main/LICENSE)
![Rust](https://img.shields.io/badge/built_with-Rust-dca282.svg?style=flat-square)

**Seamlessly sync, preserve, and version-control your AI coding conversations locally.**

WayLog CLI is a lightweight tool written in Rust that automatically saves your AI coding sessions (Claude Code, Gemini CLI, OpenAI Codex CLI) into clean, searchable local Markdown files. Stop losing your context to session timeouts—WayLog CLI helps you own your AI history locally.

[中文文档](README_zh.md) | [English](README.md)

Platform quickstart:
[macOS / Windows Source-Build Quickstart (Chinese)](docs/quickstart-macos-windows.md)

---

## ✨ Features

- **🔄 Auto-Sync**: Real-time synchronization of chat history to `.waylog/history/` as you type.
- **🗂 Unified Archive**: Export flattened `Markdown + raw session file` outputs plus centralized `indexes/` metadata into a reusable archive directory.
- **👀 App Watch Mode**: Keep archiving sessions created by Claude App, Codex App, or native CLIs without wrapping them in `waylog run`.
- **☁️ GitHub Publish**: Publish the archive directory to GitHub with a token, either interactively or by passing flags.
- **📦 Full History Recovery**: The `pull` command scans your entire machine to recover past sessions into the current project.
- **📝 Markdown Native**: All history is saved as high-quality Markdown files with frontmatter metadata.


## 🚀 Installation

### Using Homebrew

```bash
brew install shayne-snap/tap/waylog
```

### Using Scoop (Windows)

```powershell
scoop bucket add waylog https://github.com/shayne-snap/scoop-bucket
scoop install waylog
```

### Using Cargo

```bash
cargo install waylog
```


## 💡 Usage

### 1. Real-time Logging (`run`)

Use `waylog run` instead of calling your AI tool directly. WayLog will launch the agent and record the conversation in real-time.



```bash
# Run Claude Code with auto-sync
waylog run claude

# Run Codex and mirror the session into a unified archive directory
waylog run codex --archive-dir ~/waylog-archive

# Run Gemini CLI
waylog run gemini

# Run Codex CLI
waylog run codex
```

![WayLog Run Demo](demo/run.gif)


### 2. Full Sync / Recover History (`pull`)

Scans your local AI provider storage and "pulls" all relevant sessions into your project's `.waylog` folder.



```bash
# Pull all history for the current project
waylog pull
```
![WayLog Pull Demo](demo/pull.gif)

### 3. Host-wide Unified Archive (`export`)

Scans local provider storage on the current machine and exports sessions into a flat archive view under `sessions/`, with readable filenames plus `.raw.*` companions and centralized indexes under `indexes/`.

WayLog now exports browser history into `browser-history/` by default alongside AI sessions. It currently supports Chrome and ChatGPT Atlas on macOS, plus Chrome on Windows. Browser history collection copies locked History databases to temp files before reading them and captures visit-level history records only. It does not capture page-level clickstream or tab focus behavior.

If your browser history is already synced across devices and you only want one machine to archive it, use `--no-browser` on other machines to export AI sessions without browser history.

Before writing to the archive, WayLog skips a default set of low-value sessions:

- trivial greetings like `hi` / `hello`
- shell probes like `exit` or `whoami`
- empty `Untitled Session` records
- boilerplate-only setup prompts such as `agents-instructions` / `context-from-my-ide-setup`
- dedicated git commit message generator chats

```bash
# Export all default sources to the default archive dir
waylog export

# Export only Codex AI sessions plus default browser history to a specific archive dir
waylog export --provider codex --archive-dir ~/waylog-archive

# Export AI sessions only and skip browser history
waylog export --no-browser --archive-dir ~/waylog-archive

# Export Chrome browser history explicitly
waylog export --browser chrome --archive-dir ~/waylog-archive

# Export Atlas browser history explicitly
waylog export --browser atlas --archive-dir ~/waylog-archive

# Export both Codex sessions and Atlas browser history together
waylog export --provider codex --browser atlas --archive-dir ~/waylog-archive
```

### 4. Publish Archive To GitHub (`publish`)

Uploads the archive directory to a GitHub repository using a token passed directly or loaded from an environment variable. This uses the GitHub API directly, so you do not need a local `git add/commit/push` workflow.

```bash
# Start interactive publishing and answer the prompts
waylog publish

# Or provide the repo directly and keep the token in an env var
export GITHUB_TOKEN=ghp_xxx
waylog publish --repo yourname/your-knowledge-repo

# Or pass the token directly on the command line
waylog publish --repo yourname/your-knowledge-repo --token ghp_xxx

# Publish a specific archive directory into a custom path in the repo
waylog publish \
  --archive-dir ~/waylog-archive \
  --repo yourname/your-knowledge-repo \
  --repo-path inbox/waylog \
  --branch main
```

### 5. Watch App Sessions (`watch`)

Use `watch` when you want WayLog to keep archiving sessions created outside `waylog run`, such as Codex App or Claude App.

`watch` uses the same archive filter as `export`, so these noisy setup/test sessions are skipped before they enter your knowledge-base archive.

`watch` also polls supported browser history every 30 seconds by default and appends new visit records into the same archive root. `--browser chrome` and `--browser atlas` remain available as explicit forms.

If you want a machine to keep syncing AI sessions but never archive browser history, run `watch --no-browser`.

```bash
# Watch all default sources and keep the archive updated
waylog watch --archive-dir ~/waylog-archive

# Watch AI sessions only and skip browser history
waylog watch --no-browser --archive-dir ~/waylog-archive

# Watch only Codex App / Codex local sessions
waylog watch --provider codex --archive-dir ~/waylog-archive

# Watch only Claude App / Claude local sessions
waylog watch --provider claude --archive-dir ~/waylog-archive

# Watch Chrome browser history only
waylog watch --browser chrome --archive-dir ~/waylog-archive

# Watch Atlas browser history only
waylog watch --browser atlas --archive-dir ~/waylog-archive

# Watch both Codex sessions and Atlas browser history
waylog watch --provider codex --browser atlas --archive-dir ~/waylog-archive
```

### 6. Scheduled Publish

`waylog publish` is designed to be called by external schedulers.

macOS / Linux (`cron`):

```bash
0 2 * * * cd /path/to/project && /Users/you/.cargo/bin/waylog publish --archive-dir /Users/you/waylog-archive --repo yourname/your-knowledge-repo --repo-path waylog
```

Windows (Task Scheduler, `powershell.exe` arguments):

```powershell
-NoProfile -Command "Set-Location C:\path\to\project; $env:GITHUB_TOKEN='ghp_xxx'; waylog publish --archive-dir C:\Users\you\waylog-archive --repo yourname/your-knowledge-repo --repo-path waylog"
```

On Windows, WayLog also honors `CLAUDE_CONFIG_DIR` for Claude local data discovery if you keep Claude's session data outside the default `~/.claude` location.

## 📂 Supported Providers

| Provider | Status | Description |
|----------|--------|-------------|
| **Claude Code** | 🚧 Beta | Supports `claude` CLI tool from Anthropic. |
| **Gemini CLI** | 🚧 Beta | Supports Google's Gemini CLI tools. |
| **Codex** | 🚧 Beta | Supports OpenAI Codex CLI. |

### Dev build

```bash
git clone https://github.com/shayne-snap/waylog-cli.git
cd waylog-cli
./scripts/install.sh
```


## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## 📄 License

Distributed under the Apache License 2.0. See `LICENSE` for more information.
