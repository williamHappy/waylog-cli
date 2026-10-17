# WayLog CLI

[![GitHub license](https://img.shields.io/github/license/shayne-snap/waylog-cli?style=flat-square)](https://github.com/shayne-snap/waylog-cli/blob/main/LICENSE)
![Rust](https://img.shields.io/badge/built_with-Rust-dca282.svg?style=flat-square)

**无缝同步、保留并本地化版本控制你的 AI 编程对话历史。**

WayLog CLI 是一个轻量级的工具，自动捕捉并存档你的 AI 编程会话（Claude Code, Gemini CLI, OpenAI Codex CLI），将其导出为整洁、可搜索的本地 Markdown 文件。不要再因为会话过期而丢失上下文——WayLog CLI 帮你实现 AI 历史的本地所有权。

[English](README.md) | [中文文档](README_zh.md)

快速上手文档：
[macOS / Windows 源码版快速使用指南](docs/quickstart-macos-windows.md)

[后台采集服务：安装、升级、重启与检查](docs/waylog-upgrade.md)

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

Codex 会话首次归档时，文件名和索引标题优先使用本地 Codex 会话名称；名称不可用时回退到第一条用户消息的首个非空行。已归档的 Codex 会话保留原标题与文件名，之后在 Codex 中重命名也不会触发归档重命名。标题读取不影响原有会话过滤规则。

现在 WayLog 默认也会把浏览器历史一起导出到 `browser-history/`。当前支持 macOS 上的 Chrome 和 ChatGPT Atlas，以及 Windows 上的 Chrome，会先复制被锁定的 History 数据库再读取，采集的是 visit 级浏览历史，不包含页面内点击流、标签页焦点或停留时长。

如果你的 Chrome 历史已经通过账户在多设备间同步，只希望其中一台机器负责归档浏览器历史，可以在其他机器上使用 `--no-browser`，只归档 AI 会话。

浏览器 URL 只隐藏已识别的敏感参数值，普通参数默认保留，例如
`/product?id=123&page=2&access_token=abc` → `/product?id=123&page=2&access_token=REDACTED`。
fragment 参数同样处理；`#/orders?page=2` 和普通锚点保持原样。
嵌套的 HTTP(S) 地址和带参数的根路径跳转地址递归处理，保留普通值、隐藏敏感值；
嵌套超过四层时整体隐藏深层地址。URL 中的用户名、密码也会删除。
`record_id` 使用同一个脱敏后的 URL，访问时间和 visit ID 继续区分不同访问。

refresh_token、access_token、id_token、ticket、password、api_key、state 等凭据和
认证参数会隐藏。`code` 既可能是授权码，也可能是商品编号，默认隐藏；如确认某个商品
页面的 `code` 是业务编号，可复制修改
[`browser-url-rules.example.json`](browser-url-rules.example.json)，通过环境变量指定例外规则：

```bash
WAYLOG_BROWSER_URL_RULES=/absolute/path/browser-url-rules.json waylog export
# watch 同样支持该环境变量；修改规则后需要重启进程
WAYLOG_BROWSER_URL_RULES=/absolute/path/browser-url-rules.json waylog watch
```

规则按 `origin`（协议、域名、端口）、`path` 和参数名精确匹配；
`fragment_path` 存在时仅作用于对应 fragment 路由。示例域名为虚构，默认没有例外。
已识别的凭据不能通过例外放行；登录/OAuth/回调路径的 `code` 也继续隐藏，
这些页面上的普通分页等参数仍保留。配置缺失或无效时使用默认敏感参数规则，
普通参数不受影响。自定义名字的秘密、未识别格式的嵌套 URL 和无参数名的 fragment
可能无法识别；本功能不保证识别所有秘密。规则不会改写现有归档，
已替换为 `REDACTED` 的普通值只能从原始备份或源数据恢复。

隐私边界：URL 的 path、前端路由 path、页面标题和本机路径仍保留；非法 URL 按兼容规则原样返回。
本功能不是全归档脱敏。AI 会话正文、标题、Gemini 思考内容可能含敏感信息，
而 `.raw.*` 会直接复制源会话文件，包含未进入 Markdown 的原始事件；
`publish` 会上传归档目录内所有文件，不进行敏感信息检查。
浏览器 History 临时副本仍包含原始 URL，正常读取后删除，进程异常退出可能留下副本。
如要重建旧浏览器归档，需先停止旧采集进程，同时清理 `browser-history/` 和
`indexes/browser-history.jsonl` 后重新导出；仅使用 `--force` 会合并旧记录。
重建仅能恢复浏览器数据库仍保留的历史，不会清除 Git 历史。

在写入归档前，WayLog 会默认排除一批低价值会话：

- `hi` / `hello` 这类纯问候
- `exit`、`whoami` 这类探测式命令会话
- 空的 `Untitled Session`
- `agents-instructions` / `context-from-my-ide-setup` 这类只有样板上下文的会话
- 专门用于生成 git commit message 的会话

```bash
# 导出默认所有来源到默认归档目录
waylog export

# 导出 Codex AI 会话，并同时包含默认浏览器历史
waylog export --provider codex --archive-dir ~/waylog-archive

# 只导出 AI 会话，跳过浏览器历史
waylog export --no-browser --archive-dir ~/waylog-archive

# 显式导出 Chrome 浏览历史
waylog export --browser chrome --archive-dir ~/waylog-archive

# 显式导出 Atlas 浏览历史
waylog export --browser atlas --archive-dir ~/waylog-archive

# 同时导出 Codex 会话和 Atlas 浏览历史
waylog export --provider codex --browser atlas --archive-dir ~/waylog-archive
```

### 4. 上传归档到 GitHub (`publish`)

通过命令行直接传入或通过环境变量里的 GitHub token，把统一归档目录直接上传到 GitHub 仓库。这个流程走 GitHub API，不依赖本地 `git add/commit/push`。

```bash
# 直接进入交互式发布流程
waylog publish

# 或者显式传入仓库参数，并通过环境变量提供 token
export GITHUB_TOKEN=ghp_xxx
waylog publish --repo yourname/your-knowledge-repo

# 或者直接在命令行里传入 token
waylog publish --repo yourname/your-knowledge-repo --token ghp_xxx

# 把指定归档目录上传到仓库内的自定义路径
waylog publish \
  --archive-dir ~/waylog-archive \
  --repo yourname/your-knowledge-repo \
  --repo-path inbox/waylog \
  --branch main
```

### 5. 监听 App 会话 (`watch`)

如果你不是通过 `waylog run` 启动，而是直接使用 Codex App、Claude App 或原生命令行，可以用 `watch` 持续监听本地会话目录并归档。

这里会复用和 `export` 相同的默认归档过滤规则，所以这些明显的测试/样板会话不会进入你的知识库目录。

`watch` 默认也会每 30 秒轮询一次已支持的浏览器历史，并把新增浏览记录追加到同一个 archive 根目录里。`--browser chrome` 和 `--browser atlas` 仍然可以作为显式写法保留。

如果你希望某台机器只同步 AI 会话、不归档浏览器历史，可以使用 `watch --no-browser`。

```bash
# 监听默认所有来源
waylog watch --archive-dir ~/waylog-archive

# 只监听 AI 会话，跳过浏览器历史
waylog watch --no-browser --archive-dir ~/waylog-archive

# 仅监听 Codex App / Codex 本地会话
waylog watch --provider codex --archive-dir ~/waylog-archive

# 仅监听 Claude App / Claude 本地会话
waylog watch --provider claude --archive-dir ~/waylog-archive

# 仅监听 Chrome 浏览历史
waylog watch --browser chrome --archive-dir ~/waylog-archive

# 仅监听 Atlas 浏览历史
waylog watch --browser atlas --archive-dir ~/waylog-archive

# 同时监听 Codex 会话和 Atlas 浏览历史
waylog watch --provider codex --browser atlas --archive-dir ~/waylog-archive
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
