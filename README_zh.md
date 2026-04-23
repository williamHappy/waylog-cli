# WayLog CLI

[![GitHub license](https://img.shields.io/github/license/shayne-snap/waylog-cli?style=flat-square)](https://github.com/shayne-snap/waylog-cli/blob/main/LICENSE)
![Rust](https://img.shields.io/badge/built_with-Rust-dca282.svg?style=flat-square)

**无缝同步、保留并本地化版本控制你的 AI 编程对话历史。**

WayLog CLI 是一个轻量级的工具，自动捕捉并存档你的 AI 编程会话（Claude Code, Gemini CLI, OpenAI Codex CLI），将其导出为整洁、可搜索的本地 Markdown 文件。不要再因为会话过期而丢失上下文——WayLog CLI 帮你实现 AI 历史的本地所有权。

[English](README.md) | [中文文档](README_zh.md)

---

## ✨ 特性

- **🔄 自动同步**：实时同步聊天历史至 `.waylog/history/`，边聊边记。
- **🗂 统一归档**：支持把 `Markdown + 原始会话文件` 平铺导出到长期复用的统一目录，并把元数据集中写入 `indexes/`。
- **👀 App 监听模式**：即使不通过 `waylog run` 启动，也能持续归档 Claude App、Codex App 或原生命令行会话。
- **☁️ GitHub 发布**：支持通过 token 交互式或命令式发布归档目录到 GitHub。
- **📦 全量历史恢复**：使用 `pull` 命令扫描全机，将过去或丢失的会话恢复到当前项目中。
- **📝 Markdown 原生**：所有历史记录均保存为带 Frontmatter 元数据的高质量 Markdown 文件。

## 🚀 安装

### 使用 Homebrew (推荐)

```bash
brew install shayne-snap/tap/waylog
```

### 使用 Cargo

```bash
cargo install waylog
```


## 💡 使用方法

### 1. 实时记录 (`run`)

使用 `waylog run` 代替直接调用 AI 工具。WayLog 将启动代理并实时记录对话。



```bash
# 启动 Claude Code 并同步
waylog run claude

# 启动 Codex CLI，并实时同步到统一归档目录
waylog run codex --archive-dir ~/waylog-archive

# 启动 Gemini CLI
waylog run gemini

# 启动 Codex CLI
waylog run codex
```

![WayLog Run Demo](demo/run.gif)

### 2. 全量同步 / 恢复历史 (`pull`)

扫描本地 AI 供应商的存储，并将所有相关的会话“拉取”到项目的 `.waylog` 文件夹中。



```bash
# 拉取当前项目的所有历史记录
waylog pull
```
![WayLog Pull Demo](demo/pull.gif)

### 3. 全机统一归档 (`export`)

扫描当前主机上的本地 AI 会话目录，并把会话平铺导出到统一归档目录。每条会话会生成可读文件名的 `.md`、`.raw.*`，会话级元数据集中写入 `indexes/`。

```bash
# 导出全部支持供应商的会话到默认归档目录
waylog export

# 仅导出 Codex 会话到指定归档目录
waylog export --provider codex --archive-dir ~/waylog-archive
```

### 4. 上传归档到 GitHub (`publish`)

通过环境变量里的 GitHub token，把统一归档目录直接上传到 GitHub 仓库。这个流程走 GitHub API，不依赖本地 `git add/commit/push`。

```bash
# 直接进入交互式发布流程
waylog publish

# 或者显式传入仓库参数，并通过环境变量提供 token
export GITHUB_TOKEN=ghp_xxx
waylog publish --repo yourname/your-knowledge-repo

# 把指定归档目录上传到仓库内的自定义路径
waylog publish \
  --archive-dir ~/waylog-archive \
  --repo yourname/your-knowledge-repo \
  --repo-path inbox/waylog \
  --branch main
```

### 5. 监听 App 会话 (`watch`)

如果你不是通过 `waylog run` 启动，而是直接使用 Codex App、Claude App 或原生命令行，可以用 `watch` 持续监听本地会话目录并归档。

```bash
# 监听所有支持的供应商
waylog watch --archive-dir ~/waylog-archive

# 仅监听 Codex App / Codex 本地会话
waylog watch --provider codex --archive-dir ~/waylog-archive

# 仅监听 Claude App / Claude 本地会话
waylog watch --provider claude --archive-dir ~/waylog-archive
```

### 6. 定时发布

`waylog publish` 可以直接被外部调度器调用。

macOS / Linux（`cron`）：

```bash
0 2 * * * cd /path/to/project && /Users/you/.cargo/bin/waylog publish --archive-dir /Users/you/waylog-archive --repo yourname/your-knowledge-repo --repo-path waylog
```

Windows（任务计划程序，`powershell.exe` 参数）：

```powershell
-NoProfile -Command "Set-Location C:\path\to\project; $env:GITHUB_TOKEN='ghp_xxx'; waylog publish --archive-dir C:\Users\you\waylog-archive --repo yourname/your-knowledge-repo --repo-path waylog"
```

另外，Windows 下如果 Claude 的本地数据目录不在默认的 `~/.claude`，WayLog 现在也会识别 `CLAUDE_CONFIG_DIR`。

## 📂 支持的供应商

| 供应商 | 状态 | 描述 |
|----------|--------|-------------|
| **Claude Code** | 🚧 Beta | 支持 Anthropic 的 `claude` 命令行工具。 |
| **Gemini CLI** | 🚧 Beta | 支持 Google 的 Gemini 命令行工具。 |
| **Codex** | 🚧 Beta | 支持 OpenAI Codex CLI。 |


### 源码安装

```bash
git clone https://github.com/shayne-snap/waylog-cli.git
cd waylog-cli
./scripts/install.sh
```

## 🤝 贡献

欢迎贡献！请随时提交 Pull Request。

## 📄 许可证

基于 Apache License 2.0 许可证分发。详见 `LICENSE` 文件。
