# WayLog macOS / Windows 源码版快速使用指南

这份文档默认面向 **二次开发版本 / 最新源码版本** 的使用方式。  
也就是说，这里优先讲的是：

- 如何安装你当前仓库里的最新代码
- 如何从指定远程分支直接安装
- 如何更新到你最新的源码版本
- 如何基于这份最新代码使用和卸载

如果你要的是 Homebrew / Scoop 这种官方发行版安装方式，请回到 [README.md](../README.md) 或 [README_zh.md](../README_zh.md) 查看。

## 1. 最短路径

### macOS

如果你本机已经有这份仓库：

```bash
cd /path/to/waylog-cli
cargo install --path . --force
rehash
waylog --help
```

如果你想从远程仓库某个分支直接安装：

```bash
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
rehash
waylog --help
```

### Windows

如果你想直接从远程仓库某个分支安装，不手动下载源码：

```powershell
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
waylog --help
```

如果你本机已经有这份仓库：

```powershell
cd C:\path\to\waylog-cli
cargo install --path . --force
waylog --help
```

## 2. 安装

## 2.1 先决条件

源码版安装默认需要：

- Rust / Cargo
- macOS 或 Windows
- 如果是 `--git` 安装，最好本机已装 Git

检查：

```bash
cargo --version
git --version
```

Windows PowerShell 也是直接执行同样的命令。

## 2.2 macOS 安装

### 方式一：安装当前本地仓库代码

适合你正在开发或测试本地改动。

```bash
cd /path/to/waylog-cli
cargo install --path . --force
rehash
```

### 方式二：直接安装远程分支代码

适合你想要远程最新分支版本，但不想手动 `git clone`。

```bash
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
rehash
```

### 方式三：先 clone 再安装

适合你后面还会继续改代码。

```bash
git clone -b feature/20260424-upload-custom https://github.com/williamHappy/waylog-cli.git
cd waylog-cli
cargo install --path . --force
rehash
```

## 2.3 Windows 安装

### 方式一：直接安装远程分支代码

这是 Windows 下最省事的源码版方式。

```powershell
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
```

### 方式二：安装当前本地仓库代码

如果你已经把仓库放到本地：

```powershell
cd C:\path\to\waylog-cli
cargo install --path . --force
```

### 方式三：先 clone 再安装

```powershell
git clone -b feature/20260424-upload-custom https://github.com/williamHappy/waylog-cli.git
cd waylog-cli
cargo install --path . --force
```

## 2.4 安装后检查

建议先检查这些命令是否都在：

```bash
waylog --help
waylog export --help
waylog watch --help
waylog publish --help
```

如果你刚装的是包含新能力的开发版本，至少要能看到：

- `export`
- `watch`
- `publish`

## 3. 更新

源码版更新的核心原则是：  
**你怎么装的，就用对应方式覆盖安装。**

## 3.1 macOS 更新

### 更新当前本地仓库版本

```bash
cd /path/to/waylog-cli
cargo install --path . --force
rehash
```

### 更新远程分支版本

```bash
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
rehash
```

## 3.2 Windows 更新

### 更新当前本地仓库版本

```powershell
cd C:\path\to\waylog-cli
cargo install --path . --force
```

### 更新远程分支版本

```powershell
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
```

## 3.3 更新后检查

```bash
waylog --help
waylog export --help
waylog watch --help
waylog publish --help
```

如果命令不对，优先检查是不是命中了旧版二进制。

macOS：

```bash
which -a waylog
```

Windows：

```powershell
Get-Command waylog -All
```

## 4. 日常使用

当前源码版最常用的是这几条命令：

- `run`
- `pull`
- `export`
- `watch`
- `publish`

## 4.1 在项目里实时记录

```bash
waylog run claude
waylog run codex
waylog run gemini
```

如果你还想同步进统一归档目录：

```bash
waylog run codex --archive-dir ~/waylog-archive
```

Windows：

```powershell
waylog run codex --archive-dir C:\Users\you\waylog-archive
```

## 4.2 导出统一归档

导出全部 provider：

```bash
waylog export --archive-dir ~/waylog-archive
```

只导出 Claude：

```bash
waylog export --provider claude --archive-dir ~/waylog-archive
```

只导出 Codex：

```bash
waylog export --provider codex --archive-dir ~/waylog-archive
```

Windows：

```powershell
waylog export --archive-dir C:\Users\you\waylog-archive
```

归档目录默认长这样：

```text
waylog-archive/
  sessions/
    *.md
    *.raw.jsonl
  indexes/
    sessions.jsonl
    manifest.json
```

## 4.3 后台监听 Claude App / Codex App

如果你不是通过 `waylog run` 启动，而是直接在 App 里使用，就用：

```bash
waylog watch --archive-dir ~/waylog-archive
```

只监听 Claude：

```bash
waylog watch --provider claude --archive-dir ~/waylog-archive
```

只监听 Codex：

```bash
waylog watch --provider codex --archive-dir ~/waylog-archive
```

Windows：

```powershell
waylog watch --archive-dir C:\Users\you\waylog-archive
```

## 4.4 发布到 GitHub

最简单是交互式：

```bash
waylog publish
```

也可以显式传参：

```bash
export GITHUB_TOKEN=ghp_xxx
waylog publish \
  --archive-dir ~/waylog-archive \
  --repo yourname/your-knowledge-repo \
  --repo-path waylog \
  --branch main
```

Windows：

```powershell
$env:GITHUB_TOKEN="ghp_xxx"
waylog publish --archive-dir C:\Users\you\waylog-archive --repo yourname/your-knowledge-repo --repo-path waylog --branch main
```

## 5. 卸载

源码版安装本质上还是 Cargo 安装，所以卸载方式统一：

### macOS

```bash
cargo uninstall waylog
```

### Windows

```powershell
cargo uninstall waylog
```

如果你只是想切回另一份源码版，通常不用先卸载，直接再执行一次：

```bash
cargo install --path . --force
```

或者：

```bash
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
```

## 6. 常用排错

## 6.1 命中了旧版本

macOS：

```bash
which -a waylog
```

Windows：

```powershell
Get-Command waylog -All
```

## 6.2 更新后没有新命令

先确认你装的是不是最新源码版：

```bash
waylog --help
waylog export --help
```

如果看不到新命令，就重新覆盖安装。

## 6.3 归档里有旧的测试会话

当前版本会默认过滤掉一些低价值会话，例如：

- `hi` / `hello`
- `agents-instructions`
- `context-from-my-ide-setup`
- `git commit message` 生成类会话

注意：

- 新版会阻止这些会话继续写入归档
- 但旧归档目录里已经存在的文件不会自动删除

如果你想得到最干净的结果，建议：

1. 清理旧归档目录
2. 重新运行 `waylog export`

## 7. 推荐起步命令

### macOS

```bash
cd /path/to/waylog-cli
cargo install --path . --force
rehash
waylog export --archive-dir ~/waylog-archive
waylog watch --archive-dir ~/waylog-archive
```

### Windows

```powershell
cargo install --git https://github.com/williamHappy/waylog-cli.git --branch feature/20260424-upload-custom waylog --force
waylog export --archive-dir C:\Users\you\waylog-archive
waylog watch --archive-dir C:\Users\you\waylog-archive
```
