# Waylog 后台采集：Mac / Windows 安装与升级

适用范围：本仓库的常驻采集和定时 Git 同步脚本。Mac 使用 launchd 直接启动安装版 Waylog；Windows 保留 PowerShell＋Python 监督入口。本文同时记录安装结果与后续升级步骤。

当前程序版本为 **0.2.11**（2026-10-17）。Codex 会话首次归档优先使用本地会话名称，缺失时回退到第一条用户消息；已归档会话保留原标题和文件名，不执行历史迁移。

本轮两端安装版均已核验为 0.2.11，Mac 与 Windows 原生 Rust 测试各 191 项通过。安装核对分别保留 Mac 629 条、Windows 1,057 条既有归档的标题和路径；两端原采集任务恢复，未手动触发同步。Mac 更换程序后 Chrome 曾返回 `Operation not permitted`；重新确认权限后，23:04:04 的自动采集已成功更新 Chrome 归档，Chrome/Default 检查均为 `ok`，会话采集正常。

两端服务脚本此前已部署并发推送重试、GH001 分类和状态异常降级修复（2026-10-17），本次标题规则升级不替换服务脚本或配置。普通同步仍每小时，状态检查点为北京时间 23:00；连续三轮上传失败暂停的策略保留。最新部署及验证边界见 [轻量状态部署记录](waylog-daily-status.md#2026-10-17-两端修复验收)。旧严格 `--coverage` 与轻量状态不能同时启用。

升级完成后，确认实际安装版本和脚本内容、原任务正常运行、配置及归档保留，并根据本轮是否有待上传数据核对上传结果；验证通过即清理本次升级/修复生成的旧程序、脚本、配置副本及临时安装包。清理前列出确切路径，只删除已完成用途的备份，不删除运行中的配置、状态、日志、归档或 Git 历史。没有新数据时正常跳过上传，无须为清理备份强制产生一次上传。

## 1. 先分清更新的是哪一部分

| 更新内容 | 你需要做什么 |
| --- | --- |
| Waylog 程序 | 停止任务，安装兼容的新程序，检查参数，再启动 |
| 服务脚本 | 使用旧入口和原配置停止任务，再更新脚本、检查并启动 |
| 仓库/程序路径或同步周期 | 使用原配置停止任务，再改配置、检查并启动 |
| 普通重试参数、pull_on_idle | 同步会重新读取配置，但不会清除暂停状态或失败次数 |
| 归档仓库数据 | 正常同步即可，与程序安装分开；升级不用重新克隆 |

**日常启动和停止仍使用同一个服务脚本。** 不需要另开终端手工运行 waylog watch。

当前两端配置目标：

| 项目 | Mac | Windows |
| --- | --- | --- |
| 安装程序 | `/Users/wangct/.cargo/bin/waylog` | `%USERPROFILE%\.cargo\bin\waylog.exe` |
| 配置 | `scripts/waylog-service.macos.json` | `scripts\waylog-service.windows.json` |
| 归档仓库 | `/Users/wangct/workspace/wk-me/i-raw-data` | `D:\wangct\wk-me\i-raw-data-sparse` |
| 本机归档 | `ai/raw/wk-me` | `ai/raw/wk-jzx` |
| 拉取策略 | 每小时检查，即使本机无变化也拉取 | 无本机变化且无积压提交时跳过联网 |
| 采集启动链 | launchd → Waylog | 任务计划程序 → Python → Waylog |
| 浏览记录权限 | 对实际安装的 Waylog 确认 macOS 权限 | 不需要 macOS 的完全磁盘访问授权 |

Windows **不用因为 Mac 这次权限改造而修改启动架构**。它需要使用现有脚本和带共享锁功能的 Windows 版 Waylog；此处更新 Windows 使用说明，不改 Windows 入口或本机配置。

## 2. “兼容新版”与源码准备

本次后台功能包含本地定制改动，目前不能假定 Homebrew、Scoop、crates.io 或旧远端分支已经包含它们。安装前必须取得保留这些功能的源码/二进制，安装后检查帮助参数。

| 系统 | 必须包含的 watch 参数 |
| --- | --- |
| Mac | `--archive-lock-file`、`--git-dir`、`--instance-lock-file`、`--service-log-file` |
| Windows | `--archive-lock-file`、`--git-dir` |
| 自定义采集周期 | 还需要 `--interval-seconds` |

目前不同程序都可能显示 `0.2.7`，不能仅按版本号认定兼容。默认周期也是建议使用含 interval-seconds 的当前源码。

**安装命令不会替你更新源码。** `cargo install --path .` 安装的是当前目录里的代码。以后有上游新版时，先把新版与本次改动合并到源码目录，再安装；只重复安装旧源码不会获得新功能。

源码更新前先检查：

```bash
git status --short
```

有本机修改或未跟踪源码时，先保留并整合。不要直接覆盖整个目录、reset 或切换旧发行标签，导致原生启动、共享锁、配置读取或重试逻辑丢失。当前这份目录包含未提交的定制改动，单纯在另一台电脑克隆旧远端版本不能保证获得它们。

如由我协助升级，我会先检查源码状态、整合新版和定制功能，再安装及验收。

## 3. 安装参数与流量

文档统一使用：

- `--path .`：从当前本地源码安装。
- `--root`：明确安装位置，避免自定义 Cargo 环境把程序装到别处。
- `--force`：替换已安装程序；这里不默认备份旧二进制。
- `--locked`：使用已有 Cargo.lock；锁文件缺失或需要改变时停止。
- `--offline`：禁止 Cargo 联网，优先使用本机缓存。

参数依据见 [Cargo 官方安装说明](https://doc.rust-lang.org/cargo/commands/cargo-install.html)。

离线安装失败时先看错误原因。缺少依赖且你允许下载时，才去掉 offline 再运行一次；文档提供的联网命令带 `--config net.retry=0`，关闭 Cargo 的网络重试。它只作用于这一条安装命令，不改全局设置。说明见 [Cargo 网络重试配置](https://doc.rust-lang.org/cargo/reference/config.html#netretry)。

安装依赖的下载与归档上传是两回事。Waylog 服务的失败次数上限不会限制 rustup、Cargo 或 Windows 构建工具的下载安装流量。首次安装 Rust/构建工具需要联网，失败时不要放进循环反复执行。

## 4. Mac 每次升级的具体步骤

### 4.1 停止当前任务

先进入当前源码目录，用原配置停止任务。若使用了自定义 --config，下面所有服务命令继续带同一配置路径。

```bash
cd /Users/wangct/workspace/wk-me/i-ai/waylog-cli
python3 scripts/waylog-service.py stop
python3 scripts/waylog-service.py status
```

预期 watch 与 sync 都显示 not loaded。停止保留归档、未提交数据、本地提交、日志和重试状态。

### 4.2 准备兼容的新源码，再安装

源码准备方法见第 2 节。源码已准备好时执行：

```bash
cargo install --path . --root /Users/wangct/.cargo --force --locked --offline
```

看到安装成功、目标为 `/Users/wangct/.cargo/bin/waylog` 后再继续。编译或安装失败时先处理错误，不能直接启动。若明确允许下载缺少的依赖，手动执行这一条替代命令：

```bash
cargo install --path . --root /Users/wangct/.cargo --force --locked --config net.retry=0
```

只运行 cargo build 会更新 target/release，**不会自动更新当前服务使用的安装版**。

### 4.3 检查实际安装的程序和配置

```bash
/Users/wangct/.cargo/bin/waylog --version
/Users/wangct/.cargo/bin/waylog watch --help
python3 scripts/waylog-service.py config
```

核对第 2 节的 Mac 参数。config 输出中的 waylog 应是 `/Users/wangct/.cargo/bin/waylog`。本机配置对应字段如下：

```jsonc
"waylog": "~/.cargo/bin/waylog" // 固定安装路径
```

同一个安装路径更新时，不需要改这个字段。改到其他路径时，必须同步改配置，并为实际新程序检查权限。

### 4.4 核对系统权限

权限由你在“系统设置 → 隐私与安全性 → 完全磁盘访问权限”确认。对象是实际安装的 `/Users/wangct/.cargo/bin/waylog`，不是开发目录 target/release，也不是 Python。

相同路径更新后，不能假定旧授权永远有效。若后续日志出现 Chrome 的 Operation not permitted，先检查这个安装版的开关与实际路径，再核查系统权限日志。只授权 Waylog；不要通过给通用 Python 补权限来解决。

你完成权限设置后，再继续重启步骤。

### 4.5 运行本地检查，再启动

```bash
python3 scripts/waylog-service.py doctor
python3 scripts/waylog-service.py --dry-run sync
```

两项检查通过后才运行：

```bash
python3 scripts/waylog-service.py start
python3 scripts/waylog-service.py status
python3 scripts/waylog-service.py logs
```

start 会注册任务并检查一次同步，可能实际提交、拉取和推送；仍遵守原有冷却和暂停状态。

### 4.6 如何判断升级成功

- status 中 watch 持续 running；sync 空闲时 not running、last exit code=0 可以是正常状态，不能要求它一直运行。
- Mac watch 的后台命令直接指向安装版 Waylog，采集链没有 Python 监督进程。
- logs 中继续出现 Watch sync complete，会话采集正常。
- 有新增浏览历史时，应出现 browser groups updated，indexes/browser-history.jsonl 随之更新；没有新浏览记录时 0 不等于故障。
- 没有新的权限拒绝、启动失败或遗留 Git 操作错误。
- 下一允许周期同步成功。push 表示实际上传成功；pull-only 表示拉取成功但不需要上传；cooldown 表示还在等待原有限流。

配置为每小时整点，但更新时的即时同步会受最小联网间隔约束。即使检查通过，也不要删状态文件或反复 sync 来强行验收推送。

Mac 日常日志在 `~/Library/Application Support/waylog-service/logs`。正常采集日志是 watch-output.log，早期启动错误看 watch-launch.log，同步看 sync.log。

## 5. Windows 第一次安装新版 Waylog

Windows 本机已完成本轮程序安装、Rust 测试和原采集任务恢复验证。本节保留首次准备步骤；SSH/LFS 上传结果仍应以实际自动同步记录为准。

### 5.1 准备安装环境和源码

1. 安装 Python 3.9+、Git、Git LFS、Windows OpenSSH。
2. 从 [Rust 官方安装页面](https://rust-lang.org/tools/install/) 下载 Windows 安装器，安装 Rust/Cargo；本源码要求 Rust 1.89+。
3. 使用默认 MSVC 工具链时，按提示安装 Visual Studio C++ 构建工具及 Windows SDK。依据见 [rustup 的 MSVC 先决条件](https://rust-lang.github.io/rustup/installation/windows-msvc.html)。
4. 将第 2 节的兼容源码与服务脚本放到 Windows；Mac 编译的程序不能直接当作 Windows 的 waylog.exe 使用。

在包含 Cargo.toml 和 scripts 文件夹的 **Waylog 源码根目录** 打开普通 PowerShell。这里不是 i-raw-data 的归档目录。检查：

```powershell
rustc --version
cargo --version
python --version
git --version
git lfs version
```

构建工具安装可能需要管理员权限；日常安装到用户目录、采集和任务使用你自己的普通用户。

### 5.2 如果有旧后台任务，先停止

只有旧任务存在时才执行。使用旧入口及其原配置停止，完成后再替换脚本或 exe：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 stop
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 status
```

若是首次准备、从未注册服务任务，可以跳过 stop。旧 iBuddy 或独立 watch 也应先停止，避免同时采集/上传。

### 5.3 安装到配置默认的固定位置

在兼容源码根目录执行：

```powershell
cargo install --path . --root "$env:USERPROFILE\.cargo" --force --locked --offline
if ($LASTEXITCODE -ne 0) { throw '离线安装失败；先检查错误，不继续启动任务。' }
```

首次 Windows 安装往往还没有完整依赖缓存。若确认允许下载，手动使用：

```powershell
cargo install --path . --root "$env:USERPROFILE\.cargo" --force --locked --config net.retry=0
if ($LASTEXITCODE -ne 0) { throw '安装失败；先检查网络或编译环境，不循环重试。' }
```

安装成功后检查实际 exe，而不是 PATH 中另一个同名程序：

```powershell
& "$env:USERPROFILE\.cargo\bin\waylog.exe" --version
& "$env:USERPROFILE\.cargo\bin\waylog.exe" watch --help
```

Windows 帮助须有 archive-lock-file、git-dir；自定义周期还须有 interval-seconds。Mac 专用的 instance-lock-file 和 service-log-file 在 Windows 不要求出现。

### 5.4 核对 Windows 配置

本机配置缺失时才复制模板，不覆盖已有配置：

```powershell
if (!(Test-Path .\scripts\waylog-service.windows.json)) {
    Copy-Item .\scripts\waylog-service.windows.example.json .\scripts\waylog-service.windows.json
}
notepad .\scripts\waylog-service.windows.json
```

核对这些字段；只修改已有字段，不把这段片段当作完整配置覆盖原文件：

```jsonc
"waylog": "%USERPROFILE%\\.cargo\\bin\\waylog.exe",
"git_repo": "D:\\wangct\\wk-me\\i-raw-data-sparse",
"repo_path": "ai/raw/wk-jzx",
"repo_mode": "sparse",
"pull_on_idle": false
```

如果配置之前指向 target/release/waylog.exe，切换固定安装版时更新 waylog 字段。以后同路径安装新版本不用再改。

带 // 注释的 JSON 需要当前 Python 核心与 PowerShell 入口一起部署，旧入口可能不能读取。Windows 仍使用现有任务计划程序＋Python 监督方式，不需要套用 Mac 的启动修改。

### 5.5 准备 SSH 和稀疏仓库

先按 [Windows 使用说明](waylog-service-windows.md#3-只读检查与-ssh) 让系统 ssh-agent 加载你的已有私钥，并核对 GitHub 连接。密码不写入 JSON。

新稀疏仓库尚未准备时，再执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 init-repo
```

init-repo 会下载仓库并创建稀疏副本，不启动 watch 或推送。目标是 `D:\wangct\wk-me\i-raw-data-sparse`，只展开本机归档范围；会保留旧完整仓库。旧目录里尚未上传的数据须先保留并妥善迁入，新副本不会自动搬迁它们。

已有符合配置的稀疏副本就跳过初始化。程序升级不需要重新克隆。初始化失败保留的临时目录和失败状态要先检查，不要连续重新下载。

### 5.6 检查和首次启动

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 config
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 doctor
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 sync -DryRun
```

确认全部通过，再启动：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 start
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 status
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 logs
```

关闭终端后任务继续运行，当前用户注销时结束。start 可能立即实际同步；仓库初始化产生的冷却也可能让首次联网同步延后。

## 6. Windows 以后每次升级

同一安装路径下，每次按下列顺序：

1. 用旧入口与原配置 stop，确认旧 Waylog 不再占用 exe。
2. 准备保留共享锁等功能的新版源码；若更新服务脚本，在停止后一起替换。
3. 使用第 5.3 节的 cargo install 命令安装到固定路径。
4. 用固定 exe 路径检查 version 和 watch --help。
5. 若路径没变，保留 JSON；若路径或配置结构有变，按新说明修改。
6. 运行 doctor 和 sync -DryRun，通过后 start。
7. 检查任务、日志、索引与下一允许周期同步结果。

**不需要重新安装 Python/Git，不需要重新 init-repo，不需要重填 SSH 私钥口令到脚本。** 若确实升级了 Python、Git 或改变了它们的路径，则另外核对配置和任务启动路径。

Windows 状态目录是 `%LOCALAPPDATA%\waylog-service`。logs 中主要看 watch-output.log、watch-worker.log、sync-worker.log，具体以入口输出为准。

## 7. 升级失败与同步异常怎么处理

| 情况 | 处理 |
| --- | --- |
| 构建或安装失败 | 停下来检查错误；确认配置指向的程序是否仍可用，不假定已升级 |
| 缺少 watch 参数 | 安装来源不兼容，保留定制功能后重新构建；不要启动旧程序 |
| Windows exe 无法替换 | 先核实旧任务/独立 watch 已退出，再安装 |
| Mac 浏览记录权限失败 | 核对安装版 Waylog 的路径、开关及权限日志，不授权通用 Python |
| doctor 或 dry-run 拒绝人工提交、暂存、冲突等 | 按报错处理仓库现场；升级程序不会解决这些问题 |
| 同步处于 cooldown | 等待下一允许周期，不删除重试状态或立即反复同步 |
| 达到失败上限或存在 BLOCKED | 先解决原因，再按服务文档分别使用 resume-sync / unblock |

更新全过程保留配置、归档仓库、state_dir、未推送提交和暂停状态。不要为了升级去清理 i-raw-data、重新初始化 Git，或重置失败计数。

恢复与 GH001 的详细规则见 [Mac / 通用服务文档](waylog-service.md#异常与重试) 和 [Windows 服务文档](waylog-service-windows.md#6-恢复与流量控制)。

## 8. 日常最短操作顺序

Mac：**stop → 准备兼容源码 → cargo install → 检查参数/权限 → doctor → dry-run → start → status/logs。**

Windows：**旧任务存在时 stop → 准备兼容源码 → cargo install 得到 exe → 核对配置 → doctor/DryRun → start → status/logs。** 第一次启动前另外准备 SSH 与稀疏仓库。

安装命令负责替换程序；start/stop 负责后台任务；同步负责归档仓库。三者分开操作。
