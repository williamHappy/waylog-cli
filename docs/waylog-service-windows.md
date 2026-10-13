# Windows：部分克隆、稀疏检出与常驻 Waylog watch

入口是 `scripts/waylog-service.windows.ps1`，共用 Python 核心和 JSON 默认值。Windows 默认 `repo_mode=sparse`、`pull_on_idle=false`，只展开 `ai/raw/wk-jzx`；每小时检查本机变化，有需要时提交、拉取和推送，无变化且无待推送提交时跳过联网。Mac 默认 full/true，按小时获取各设备数据供日报读取，见 [通用配置与 Mac 使用说明](waylog-service.md)。

本轮只修改脚本、配置和文档，做临时本地仓库测试，没有运行真实 watch、计划任务、GitHub clone/fetch/push，也没有删除或转换已有归档仓库。

## 1. 配置与程序准备

需要 Windows 10/11、PowerShell 5.1+、Python 3.9+、Git、Git LFS、Windows OpenSSH，以及包含本次共享锁参数的新版 Waylog。Git 必须支持 blob:none 部分克隆和 cone 稀疏检出；init-repo 会调用这些功能，版本不支持时直接报错。

在 Waylog 源码目录执行以下复制命令时，不要覆盖已经编辑的本机配置：

```powershell
Copy-Item .\scripts\waylog-service.windows.example.json .\scripts\waylog-service.windows.json
notepad .\scripts\waylog-service.windows.json
```

模板的关键字段：

```json
{
  "git_repo": "D:\\wangct\\wk-me\\i-raw-data-sparse",
  "archive_dir": null,
  "repo_path": "ai/raw/wk-jzx",
  "repo_mode": "sparse",
  "pull_on_idle": false,
  "interval": 3600,
  "watch_interval": 30,
  "timeout": 300,
  "retry_max_failures": 3,
  "retry_delays": [3600, 7200],
  "minimum_network_interval": 3600
}
```

缺失字段由核心补默认值，非法类型或未知字段报错。archive_dir 为 null 时，从 git_repo 和 repo_path 自动组合，所以换仓库根路径只需改 git_repo。若填 archive_dir，必须恰好等于这个组合。更多字段见通用文档。

本机配置和模板的每项旁都有 // 中文注释，用途和默认值写在注释中，没有额外说明对象。文件仍用 .json 名称，由本次入口去除字符串外的行注释后读取；URL 和 Windows 路径保留。更新配置时同时更新 Python 核心和 PowerShell 入口。普通无注释 JSON 也兼容，不支持尾逗号。

路径支持 `%LOCALAPPDATA%`、`%USERPROFILE%`、`~` 和相对目录，后者以 JSON 所在目录为基准。python 字段用于 PowerShell 选择真实 Python 可执行文件，不能填 `py -3`；推荐使用 PATH 的 `python` 或绝对路径。git、ssh 也可设为稳定的完整路径。私钥口令不放 JSON。

Waylog 必须支持 `watch --archive-lock-file` 和 `--git-dir`。若 watch_interval 非 30，还需 `--interval-seconds`。只有版本号不足以判断；用包含本次修改的源码构建（Rust 1.89+）：

```powershell
# 依赖已经缓存时，使用 --offline；否则普通构建可能下载依赖
cargo install --path . --root "$env:USERPROFILE\.cargo" --force --locked --offline
& "$env:USERPROFILE\.cargo\bin\waylog.exe" watch --help
```

推荐安装到 `%USERPROFILE%\.cargo\bin\waylog.exe`，与 JSON 默认路径一致。首次离线安装缺依赖时，先检查错误，再明确选择联网安装。完整步骤见 [安装与升级文档](waylog-upgrade.md#5-windows-第一次安装新版-waylog)。本次没有实际更新 Windows 程序或启动任务。

## 2. 明确准备稀疏副本

部分克隆用 `--filter=blob:none` 推迟下载文件内容；稀疏检出只展开本机目录。仍保存 main 的提交、树等元数据，cone 模式还会展开仓库根目录及本机目录各级祖先中的直属文件，例如 README、.gitattributes。它不会得到“只有一个目录的独立分支”，提交/推送仍更新同一 main。

Windows 默认目标已改为新的 `D:\wangct\wk-me\i-raw-data-sparse`。已有完整 clone 不能靠打开稀疏检出收回已经下载的历史内容，因此脚本保留原来的 `D:\wangct\wk-me\i-raw-data`，不转换、清理或删除它。对应配置为：

```json
{
  "git_repo": "D:/wangct/wk-me/i-raw-data-sparse",
  "archive_dir": null,
  "repo_path": "ai/raw/wk-jzx",
  "repo_mode": "sparse",
  "pull_on_idle": false
}
```

新采集目录随之变为 `D:\wangct\wk-me\i-raw-data-sparse\ai\raw\wk-jzx`。老仓库及其数据继续保留；切换前先停止旧采集/上传，核对是否有未提交文件或待推送提交。远端已有归档会由新副本检出；仅存在老工作区、尚未上传的数据需要你先保留并迁入相同本机子目录，脚本不自动丢弃或搬迁它们。索引中的原机器绝对路径可以通过限定的归档后缀验证，无需为迁移强制重建全部历史索引。

以下命令会联网下载、创建本地仓库，但不启动 watch/后台任务，不创建归档提交，不推送：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 init-repo
```

init-repo 的步骤：

1. 读取配置、获取同步锁、检查持久化冷却和暂停状态。
2. 在目标旁的 `.仓库名.waylog-initializing` 临时目录，单分支克隆 main：`--no-tags --no-checkout --filter=blob:none --sparse`，首次克隆跳过 LFS smudge。
3. 安装该新仓库的本地 LFS hook，设置 `lfs.fetchinclude=ai/raw/wk-jzx/**`，再用 cone sparse-checkout 选择该目录并 checkout main。
4. 检查成功后移动临时目录到配置目标。远端不支持过滤则报错；失败或中断保留临时目录和失败状态，不自动重新下载。
5. 目标已存在时只做本地检查；若是完整 clone、选错稀疏目录或 LFS 下载范围错误，会拒绝并保留原样。

start/sync 不会自动调用 init-repo。稀疏副本后续 fetch 保留过滤设置；其他设备更新主要接收提交和树元数据，不常规下载它们的文件内容。手工查看稀疏范围外文件、扩大检出范围或处理相关冲突时，Git 仍可能按需下载缺少的对象。现有本机归档和祖先文件不是零下载；这不是流量字节上限。

## 3. 只读检查与 SSH

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 config
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 doctor
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 sync -DryRun
```

config 只解析和输出默认值合并后的配置；doctor 检查本地依赖、仓库布局和待推送提交；DryRun 不联网、不暂存、不提交、不创建状态目录、不注册任务。检查不验证真实 GitHub/后台 SSH 认证。稀疏模式的 doctor 会验证 promisor/blob:none、唯一稀疏路径和 LFS 下载范围。

SSH 密钥有口令时，使用系统 OpenSSH 及同一 Windows 用户的 ssh-agent。需要启用系统 agent 服务时，由你自行在管理员 PowerShell 执行：

```powershell
Get-Service ssh-agent | Set-Service -StartupType Automatic
Start-Service ssh-agent
```

回到将运行任务的用户终端，使用实际私钥路径：

```powershell
& "$env:WINDIR\System32\OpenSSH\ssh-add.exe" "$env:USERPROFILE\.ssh\id_ed25519"
& "$env:WINDIR\System32\OpenSSH\ssh-add.exe" -l
& "$env:WINDIR\System32\OpenSSH\ssh.exe" -T git@github.com
```

首次连接核对 GitHub 主机指纹。成功认证提示的退出码不一定是 0，还需确认仓库写权限。后台使用 BatchMode=yes、严格指纹检查，无法解锁时直接失败，不弹口令输入框。脚本不会启用 agent、修改密钥或代理配置。

## 4. 启动后业务流程

以下命令才会注册任务并立即运行，可能提交/联网/推送：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 start
```

入口创建两个当前用户任务，名称包含 SID：

- watch：运行一个常驻原生 `waylog watch`，每 watch_interval 秒采集本机会话/浏览记录、维护 indexes。监督进程在 Waylog 退出后等待 30 秒再启动本地采集；监督进程自身失败时计划任务最多重启三次，间隔一分钟，不触发上传。
- sync：默认每小时整点检查，登录和显式 start 时也检查。没有一分钟失败重试；相同任务同时只运行一个。

任务只在当前用户登录期间运行；关闭终端、锁屏不影响，注销会结束。睡眠不运行，恢复后由 Task Scheduler 补调度，不主动唤醒电脑，默认允许电池供电运行。企业策略禁止注册时会报错，不自动提升权限。

每轮同步：

1. 读取配置，检查网络冷却/暂停，获取同步锁和共享归档锁，等待完整采集周期结束。
2. 检查 main/origin、稀疏与 LFS 范围、人工暂存内容、其他目录改动、遗留 Git 操作和待推送提交来源。
3. 校验本机归档和索引；只暂存本机目录的 sessions、indexes、browser-history、旧版 manifest.json，有变化才 commit。
4. 无变化、无本脚本积压提交：结束，不 fetch、不 LFS push、不 Git push。空的本机归档也按此策略跳过。
5. 有新提交或以前失败保留的提交：保存联网尝试状态，fetch main，再 rebase。来自 Mac 的其他目录继续留在 Git 树中，正常不展开到 Windows 工作区。
6. 仍有本机提交时先 LFS push，再 Git push；若远端已有相同提交，无积压就结束。不会把其他目录从远端删除或重新上传。
7. 成功清零失败计数；网络失败保留提交，按冷却等后续调度；rebase 冲突建立 BLOCKED，等待人工处理。

采集和同步使用同一 `archive-access.lock`。同步期间 watch 跳过取不到锁的周期，释放后继续；无需每小时重启 watch。遗留 rebase/merge/cherry-pick 时 watch 暂停写入，处理完后自动恢复。不要同时运行未带同一锁的采集、iBuddy 上传或手工 Git 操作。

indexes 由 watch 的归档写入器正常更新，随后作为本机归档的一部分提交。稀疏目录包括整个本机目录，没有排除 indexes。同步脚本不负责重新生成索引。

## 5. 配置生效与日常操作

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 status
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 logs
# 实际同步，可能提交/联网/推送
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 sync
# 停止并注销两项任务，保留归档、日志、提交和重试状态
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 stop
```

使用另一份 JSON 时所有命令传入相同 `-ConfigPath D:\config\waylog.json`。同一用户仅支持一套任务。任务描述记录脚本和配置路径，不同配置不能覆盖或删除别人的同名任务；旧版已注册任务应先用旧版入口和原配置 stop，再升级脚本。

任务固定启动时的路径、工具、分支和周期；修改这些设置须先用原配置 stop，再修改/start。pull_on_idle、重试设置和 timeout 每次 sync 重新读取；它们的修改不清除暂停状态或旧计数。no_browser 在 watch 下次启动时生效。stop 先协作取消，三秒后仍运行则由任务调度终止；Job Object 管理子进程，Git 超时会终止 Git/SSH/LFS 进程树。

## 6. 恢复与流量控制

默认每次联网尝试至少间隔一小时：第一次失败后等一小时，第二次后等两小时，第三次连续失败/中断后暂停。次数在联网前保存，重启、登录、唤醒、手动 sync 都不能绕过。delay 与 minimum_network_interval/interval 取较大值；成功轮次整点触发若只差不到两秒才过冷却，会等待结束再继续，仍不提前联网。首次启动不在整点时下一整点可能因冷却跳过。

每轮只有一次 fetch/rebase/LFS push/Git push 流程，失败、远端抢先更新或超时都不立刻补试。LFS 的三项命令级重试参数限制为 1，可能仍有一次对象内部重试；不能将 maxretries=0 当作禁用。上传体积限制按你的决定暂不增加，其他程序的流量不受此脚本约束。

脚本推送若收到 GitHub GH001，自动解析本机大文件路径，先将本脚本未推送提交备份到 state_dir/lfs-backups，再局部迁移为 LFS 并生成规则。远端缓存历史被排除，其他引用保持原值，原始文件从本地 LFS 缓存恢复，迁移不下载其他设备文件。本轮不重新推送，下个允许周期再上传；仍受失败次数和冷却限制，不强推。无法安全修复时暂停并保留备份/阻塞信息。已有人工/iBuddy 历史和 IDE 手动 push 不由此恢复流程管理，详见通用文档。

暂存区已有内容、其他目录脏文件、人工待推送提交、归档已跟踪删除或未完成 Git 操作，会停止本轮；处理后下一轮自动检查，不是永久关闭计划任务。这些本地检查失败不增加联网失败次数。冲突会尝试 rebase --abort 并建立 BLOCKED，保留本机提交，不强推；若超时留下遗留操作，先手动完成/取消。

```powershell
# 解决 Git 问题后仅解除 BLOCKED，不解决冲突或推送
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 unblock
# 网络/凭据问题解决后仅解除联网暂停，仍遵守最小间隔
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\waylog-service.windows.ps1 resume-sync
```

随后等计划任务，或冷却结束后手动 sync。不要通过删 network-state.json 绕过限流。status 会显示 last_pull、sync_action（skipped/pull-only/push）、失败原因及 next_retry_at；origin/main 是本地缓存，不能代表远端实时状态。

## 验证边界

离线测试使用临时本地 Git/LFS 远端，确认 blob:none 克隆、稀疏范围外文件内容保持未下载、跨设备提交整合、本机 LFS 下载/上传、空闲跳过、配置默认值与覆盖、索引迁移及重试暂停；Mac 原有空闲拉取测试也保留。

Rust watch 的锁及采集周期测试、Python 整套测试和离线构建在 macOS 验证。本次修改的 Rust 文件格式检查通过。全仓库已有 `archive/browser.rs`、`commands/export.rs`、`error.rs` 格式问题；严格 Clippy 另有原有 should_rewrite 未使用、handle_export/handle_publish 参数数目问题，本次不修改无关代码。

没有在 Windows 执行 PowerShell、注册真实 Task Scheduler、调用原生 Job Object 或验证后台 SSH/GitHub/LFS。因此首次启用仍需在 Windows 用新 exe 完成本地 doctor/DryRun，并实际验收；不能把 macOS 临时本地仓库测试当作 Windows 端到端通过。

## 后续更新 Waylog

完整的首次安装与后续升级步骤见 [Waylog 安装与升级](waylog-upgrade.md)。

Windows 保留任务计划程序＋Python 监督入口，不需要照搬 Mac 的原生 launchd 改造。使用带共享锁参数的 Windows exe，并与现有 Python 核心、PowerShell 入口配合。

更新前用旧入口与原配置 stop；准备兼容新版源码，用 cargo install 安装到固定用户路径；检查实际 exe 的 watch --help，运行 doctor 和 sync -DryRun，通过后 start。相同程序路径更新不改 JSON，路径变化时再更新 waylog。升级不需要重新 init-repo，也不清理原稀疏仓库和重试状态。
