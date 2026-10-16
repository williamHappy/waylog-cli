# Waylog 常驻采集与定时 Git 同步

两端共用 `scripts/waylog-service.py` 和 JSON 配置。Mac 保留完整工作区，按小时拉取，供日报读取跨设备数据；Windows 用 `blob:none` 部分克隆和稀疏检出，只展开自己的归档，空闲时跳过联网。Windows 的入口和准备步骤见 [Windows 使用说明](waylog-service-windows.md)。

编写和验证脚本不会启动服务。只有显式执行 `start` 才注册并启动后台任务，启动时可能立即提交、拉取和推送；`sync` 也会实际同步。`init-repo` 是独立的、会联网的仓库准备命令，不启动采集或推送。启用前先完成本地检查和系统权限确认。

## 配置与默认值

默认配置文件位于脚本旁：Mac 使用 `waylog-service.macos.json`，Windows 使用 `waylog-service.windows.json`。模板分别是同名 `.example.json`；本机配置已加入 `.gitignore`。配置优先级是命令行参数 > JSON > 平台默认值。配置缺少某个字段时补默认值；拼错字段、错误类型或非法数值会在执行前报错。

两个平台的本机配置和模板每项旁都有 `// 中文注释`，说明用途、单位及默认值。实际缺省值由程序维护；修改注释不会改变行为。文件扩展名仍为 .json，由脚本按带行注释的 JSON 读取；普通无注释 JSON 也兼容。

```jsonc
{
  "interval": 3600 // 同步间隔，秒；默认 3600，修改后须 stop/start
}
```

部署带注释配置时一起更新 Python 核心和 Windows PowerShell 入口。入口读取前去除字符串外的行注释，保留 URL 中的 // 和 Windows 路径；PowerShell 5.1 不直接把带注释文本交给 ConvertFrom-Json。config 命令输出合并后的纯 JSON。配置不支持尾逗号。

| 字段 | Mac 默认值 | Windows 默认值 / 作用 |
| --- | --- | --- |
| `git_repo` | `/Users/wangct/workspace/wk-me/i-raw-data` | `D:\wangct\wk-me\i-raw-data-sparse` |
| `repo_path` | `ai/raw/wk-me` | `ai/raw/wk-jzx`；每台设备必须不同 |
| `archive_dir` | `null` | 自动取 `git_repo/repo_path`，必须位于对应目录 |
| `repo_mode` | `full` | `sparse`；部分克隆＋稀疏检出 |
| `pull_on_idle` | `true` | `false`；无本机变化、无待推送提交时是否拉取 |
| `remote` / `branch` | `git@github.com:williamHappy/i-raw-data.git` / `main` | 相同 |
| `interval` | `3600` | 同步调度间隔，秒，至少 60 |
| `watch_interval` | `30` | 原生 watch 采集周期，秒，至少 1 |
| `timeout` | `300` | 每条 Git 命令及等待归档锁的超时，秒 |
| `no_browser` | `false` | 设为 true 时只采集会话 |
| `retry_max_failures` | `3` | 连续失败/中断达到此次数后暂停联网 |
| `retry_delays` | `[3600, 7200]` | 第一次、第二次失败后的等待秒数；超出列表沿用最后一项 |
| `minimum_network_interval` | `3600` | 联网间隔下限，与 interval 取较大值 |
| `state_dir` | `~/Library/Application Support/waylog-service` | `%LOCALAPPDATA%\waylog-service`；日志、锁、状态必须在仓库外 |
| `waylog` | `~/.cargo/bin/waylog` | `%USERPROFILE%\.cargo\bin\waylog.exe` |
| `git` | `git` | PATH 中的 Git 或完整路径 |
| `ssh` | `/usr/bin/ssh` | `%WINDIR%\System32\OpenSSH\ssh.exe` |
| `python` | `python3` | `python`；Windows 入口据此选择解释器 |
| `git_name` / `git_email` | `williamHappy` / `williamHappy@users.noreply.github.com` | 自动提交身份 |

路径支持 `~`、`%VAR%`、`$VAR` 和 `${VAR}`；相对路径以 JSON 所在目录为基准。Windows JSON 的反斜杠要写成 `\\`，也可直接写 `/`。SSH 私钥口令由系统 agent/Keychain 处理，不写入配置。

后台任务保存配置文件路径，并固定启动时的仓库、归档、状态目录、分支、工具路径和周期，防止 watch 与 sync 在编辑配置后使用不同目录。`pull_on_idle`、重试规则、`timeout`、提交身份每次定时同步重新读取；明确通过命令行覆盖的字段继续使用覆盖值。`no_browser` 在下次 watch 启动时生效。

修改仓库路径、repo_mode、远端、分支、工具路径、采集或同步周期时，先使用原配置 `stop`，修改后再 `start`。修改重试配置不会清除已有失败计数或暂停状态。Mac 使用运行入口时的 Python 解释器；若要更换它，请用新 Python 启动入口。

## Mac 准备与只读检查

默认使用已经存在的完整 `i-raw-data` 仓库，必须在 main 分支，origin 指向目标仓库并有 `origin/main` 缓存引用。完整模式会拒绝启用了稀疏检出的工作区。脚本不会在 start/sync 中自动克隆仓库。

需要 Python 3.9+、Git、Git LFS 和支持共享锁参数的新版 Waylog。当前源码支持 `watch --archive-lock-file`、`--git-dir`、`--interval-seconds`；Mac 原生后台任务还需要 `--instance-lock-file` 与 `--service-log-file`。版本号不足以判断兼容性，doctor 会检查实际程序参数。

以下命令在 Waylog 源码目录执行：

```bash
# 缺少本机配置时复制；已有配置不要覆盖
cp scripts/waylog-service.macos.example.json scripts/waylog-service.macos.json

# 如果依赖已缓存，可以离线构建；不会安装全局程序
cargo build --release --offline
./target/release/waylog watch --help
```

Mac 本机配置使用已安装的 `~/.cargo/bin/waylog`。必须先安装包含原生后台参数的新构建，不能仅把路径指向旧安装版。离线安装命令是 `cargo install --path . --force --locked --offline`；依赖未缓存时会报错，不自动联网补齐。

```bash
python3 scripts/waylog-service.py config
python3 scripts/waylog-service.py doctor
python3 scripts/waylog-service.py --dry-run sync
```

`config` 只输出合并后的配置，不启动任何子进程；`doctor` 检查本地依赖及仓库，不验证远端认证；`--dry-run sync` 检查保护条件、统计归档，不联网、不创建状态目录、不暂存或提交。预览不会锁住采集，不能代替实际同步的并发检查。

可在所有命令中指定另一份配置：`python3 scripts/waylog-service.py --config /完整路径/waylog.json config`。

## SSH 与启用

后台 Git 使用 `BatchMode=yes`、连接超时 15 秒和严格主机指纹校验，无法解锁私钥时直接失败，不等待密码输入。Mac 使用系统 SSH。先核对已有 `~/.ssh/config` 中 github.com 对应设置，并合并以下配置；IdentityFile 改为实际私钥，不覆盖原有 Host 配置：

```sshconfig
Host github.com
  AddKeysToAgent yes
  UseKeychain yes
  IdentityFile ~/.ssh/id_ed25519
```

由你在同一用户终端加载已有私钥，再测试系统 SSH：

```bash
/usr/bin/ssh-add --apple-use-keychain ~/.ssh/id_ed25519
/usr/bin/ssh-add -l
/usr/bin/ssh -T git@github.com
```

首次连接核对主机指纹；ssh -T 会联网但不拉取或推送归档。Keychain 用法参见 [GitHub 官方 SSH 说明](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/generating-a-new-ssh-key-and-adding-it-to-the-ssh-agent)。脚本不会修改 SSH 配置、添加密钥或测试联网认证。

准备完成后，以下命令才会启动后台任务：

```bash
python3 scripts/waylog-service.py start
python3 scripts/waylog-service.py status
python3 scripts/waylog-service.py logs
python3 scripts/waylog-service.py stop
```

start 创建两个用户级 LaunchAgent：watch 的 ProgramArguments 直接指向安装版 Waylog，不经过 Python 或 shell；sync 继续由 Python 运行，默认每小时整点触发。启动时也检查一次；关闭终端后继续运行，用户注销时结束。睡眠期间不运行，恢复后的执行由 launchd 决定。重复 start 不覆盖已加载任务。

Mac 默认一小时配置下，成功同步占用当前自然小时，下一次正常联网允许时间为下一整点；同一小时内重复调用仍会跳过，不因准备或启动晚了几秒而多跳过一小时。若配置的联网间隔大于一小时或不是一小时调度，则仍按实际联网时间加配置间隔计算。失败、超时、进程中断继续严格从失败结束或预留时间计算等待：首次至少一小时、第二次至少两小时，连续三次失败暂停；成功的整点规则不缩短失败冷却。Windows 的冷却逻辑保持不变。实际执行仍受睡眠、运行中的任务、冲突和本地保护条件影响。

## 采集流程

1. launchd 直接运行原生 `waylog watch`，默认每 30 秒检查本机会话和浏览器历史。Waylog 持有 `state_dir/watch.lock` 作为整个进程的单实例锁。
2. 每轮先尝试获取 `state_dir/archive-access.lock`。获取后完成整轮会话及浏览器采集，再释放锁。
3. 内容写入本机 `sessions/`、`browser-history/`；归档写入器维护 `indexes/sessions.jsonl`、`indexes/browser-history.jsonl`、`indexes/manifest.json`。内容无变化时不会因定时同步额外重建索引。
4. 同步持锁时 watch 跳过该采集周期，随后自动继续。同一个 watch 不会为每小时同步而反复停止、重启。
5. 遗留 rebase/merge/cherry-pick 时，watch 暂停写入，Git 操作处理完后下一轮自动继续。

不要同时运行不遵守相同锁的 export、另一份 watch 或 iBuddy 自动上传，也不要在任务运行时手动切换/编辑同一仓库。脚本只协调自己管理的采集与同步。

Mac 原生采集把输出与诊断写到 `logs/watch-output.log`，程序内部按 5 MiB 轮转并保留三份备份；`logs/watch-launch.log` 接收早期启动错误。launchd 负责异常重启，等待至少 30 秒。停止任务时原生程序处理 SIGTERM，释放单实例锁和采集锁。Windows 仍使用现有 Python 监督入口。

Mac 采集权限由实际启动链决定。新方案应只给安装路径下的 Waylog 授权，然后用 macOS 权限日志及 Chrome 索引确认；不需要为此给通用 Python 授权。完全磁盘访问本身仍是广泛权限，应由用户在系统设置确认。若程序路径或代码身份变化，需复核旧授权是否仍有效。

## 同步流程

1. 获取同步锁，检查持久化冷却/暂停状态，再等待采集周期结束并持有归档锁。
2. 检查仓库根目录、分支、origin、full/sparse 布局、暂存区、未完成 Git 操作及待推送提交来源。
3. 检查归档文件和索引。克隆后索引里的旧机器绝对路径可通过受限的 `sessions/` 或 `browser-history/` 后缀对应到本地文件；不会改写这些索引内容。
4. 只暂存本机目录下受支持且存在的 `sessions/`、`indexes/`、`browser-history/`、旧版根 `manifest.json`；有变化才创建带脚本标记的本机提交。
5. `pull_on_idle=false` 且没有待推送提交：结束本轮，不 fetch、不 push。失败保留的待推送提交也会进入后续同步，无须出现新文件。
6. `pull_on_idle=true` 或有待推送提交：在第一次联网前保存尝试状态，然后 fetch origin/main，再 rebase。Mac 会还原其他设备的新文件；Windows 仅展开稀疏范围内文件。
7. 无待推送提交时结束，仅更新 `last_pull`，不上传；有待推送提交时先 LFS push，再 Git push。成功后清零连续失败次数，保存状态。

日报应读取 Mac 已更新的完整工作区。脚本没有修改日报入口；生成时仍可能落后 Windows 上传和 Mac 拉取两个周期，睡眠、网络失败或保护检查会进一步延迟。

脚本保留已保存的 origin URL，网络命令通过临时 `-c` 使用配置中的 SSH 地址。已有 hooks 和签名设置保留；显式 LFS push 成功后，只对接下来的 Git push 设置 `GIT_LFS_SKIP_PUSH=1`，避免 LFS pre-push 再上传一遍。

## 异常与重试

暂存区已有内容、其他目录脏文件、本机已跟踪文件删除、人工待推送提交或未完成 Git 操作：停止本轮，保留内容。清理对应保护条件后，下个计划点自动再检查；这些本地检查失败不会计为联网失败。普通人工提交即使只涉及本机归档，也需由你先手动处理，因为脚本只允许推送带自身标记的普通待推送提交。合并提交仅在远端父提交已发布、本机归档相对本地父提交完全不变、相对远端父提交仅包含本机归档差异时允许通过；已有合并历史使用 merge 整合远端，不用 rebase 改写它。冲突时中止本次整合并保留原提交，等待人工处理。

同一分支的拉取与推送不是原子操作，两端写不同目录也可能竞争分支更新。普通推送使用 `--porcelain`，仅对明确的 `fetch first`、`non-fast-forward` 或远端分支已更新的 `cannot lock ref ... is at ... but expected ...` 重新 fetch、整合并推送；每轮最多 3 次推送，之间短暂随机等待，共享 `timeout` 时间预算，整轮只计一次联网失败。重试耗尽后沿用原退避规则。真正合并冲突停止，不自动选边或强推。

GH001 必须区分 `remote: warning` 和 `remote: error`；警告只保存在状态中，不触发 LFS 历史迁移或大文件暂停。混合日志中的分支竞争按竞争处理。轻量日报冻结批次的合并、崩溃恢复及大文件保护见 [日报状态说明](waylog-daily-status.md)，不会套用旧路径的自动历史迁移。

rebase 冲突会尝试 abort 并创建 `BLOCKED`；不选边覆盖、不强推。网络/认证失败或超时保留提交，结束本轮，无立即再次 fetch/push。若超时留下未完成 Git 操作，先手动完成或取消该操作。

| 默认结果 | 下次联网规则 |
| --- | --- |
| 成功 | 距尝试开始至少 `max(interval, minimum_network_interval)` 秒 |
| 第一次失败 | 距失败结束至少一小时 |
| 第二次连续失败 | 距失败结束至少两小时 |
| 第三次连续失败/中断 | 暂停，等待显式 resume-sync |
| GitHub GH001 大文件拒绝 | 备份并迁移本脚本未推送的大文件历史；本轮不再次推送 |

计数在联网前原子保存至 `network-state.json`，终止、重启或反复手动 sync 不能绕过。失败延迟实际取配置延迟与联网间隔下限的较大值。状态损坏时拒绝联网。同步调度没有一分钟自动重试；watch 本地采集重启不触发上传。

GH001 是普通 Git 文件超出 GitHub 单文件限制的确定性拒绝，增加 timeout 或重复 push 无法修复。默认 direct 的 full/sparse 模式自动恢复如下：解析 GitHub 报告的文件，限定本机目录；检查全部未推送提交均由本脚本生成；在 state_dir/lfs-backups 保存原提交 bundle 和引用快照；仅迁移未推送的 main 增量，排除缓存的远端历史。迁移会添加对应 LFS 规则、改变未推送提交 SHA，并从本地 LFS 缓存还原原始文件；保留现有 hooks 和其他分支/引用的原值。

整个修复在采集锁内完成。成功后记录 ready-for-next-cycle，仍计一次推送失败并遵守一小时/两小时退避，下个允许周期再正常上传 LFS 和 Git；本轮不立即重新上传，不强推。达到配置失败上限仍暂停，需要显式 resume-sync。迁移过程自身不 fetch 远端引用或下载文件。

没有可解析文件名、文件不属于本机、文件名含无法安全表达的匹配字符时暂停等待人工处理。备份、迁移或内容校验失败时保留备份和 BLOCKED 并暂停。旧 isolated 模式仍按暂停处理。当前已有的人工/iBuddy 待推送提交会被前置保护拦下，不会被这项后台恢复接管。这个处理只作用于脚本自己的推送，IDE 手动 push、旧 iBuddy 等外部上传仍需自行控制。首次拒绝前已经传出的流量不能收回；本次未增加上传体积预检或字节额度。

LFS 命令级 `lfs.transfer.maxretries`、`lfs.ssh.retries`、`lfs.transfer.maxverifies` 均为 1，不改全局配置。这仍可能允许一次对象内部重试；maxretries 设 0 会回退默认值，不能用它禁用重试。本次没有增加上传体积限制。

解决冲突/本地问题后：

```bash
# 仅移除冲突阻塞标记，仍检查仓库；不解决冲突或推送
python3 scripts/waylog-service.py unblock
# 仅重置联网失败计数/暂停；不绕过最小间隔或推送
python3 scripts/waylog-service.py resume-sync
# 冷却结束后实际同步，可能提交/联网/推送
python3 scripts/waylog-service.py sync
```

unblock 和 resume-sync 是不同的恢复步骤，按实际状态使用。冷却/联网暂停时采集通常继续；遗留 Git 操作时采集也暂停。不要删除状态文件来绕过限流。

## 验证范围与兼容模式

离线验证命令：`PYTHONDONTWRITEBYTECODE=1 python3 scripts/test-waylog-service.py`，`cargo test --offline`，`cargo build --offline`。测试使用临时本地 Git/LFS 仓库，覆盖配置、稀疏下载范围、索引、空闲策略、跨设备更新、冲突、超时和重试暂停。没有启动真实采集或后台服务、连接 GitHub或修改实际归档。

Windows 原生 PowerShell、任务调度、Job Object、锁、SSH/LFS 仍需在 Windows 验收；本机开发环境是 macOS。Mac 当前已安装包含原生任务支持的新构建，Windows 程序仍需在对应机器准备。全仓库格式/严格 Clippy 的既有无关问题另见 Windows 文档验证说明。

旧 `--isolated` 模式仍兼容：在 `state_dir/repo` 建立完整专用副本、复制归档再同步。它不支持 sparse，不能用于本次 Windows 节省下载的方案；Mac 日报使用默认 direct/full 工作区。旧状态中的仓库不会自动删除。

## Waylog 程序更新

完整安装、重启、权限检查和 Windows 首次准备步骤见 [Waylog 安装与升级](waylog-upgrade.md)。

Mac 当前使用固定安装路径 `~/.cargo/bin/waylog`；先 stop，准备保留定制功能的新源码，再离线 cargo install，检查原生后台参数和安装版权限，运行 doctor/dry-run，通过后 start。仅 cargo build 不会更新当前服务用的安装版；不默认备份旧二进制。

会话归档质量规则：Codex Guardian 内部审批记录不作为会话导出；实际子任务仍导出，索引的可选 `codex_source` 保存来源及父任务信息。单独注入的 `external_codex_apps_open_page` 页面上下文不用于会话标题，原始日志仍完整保留；真实提问中的 `null` 不过滤。新归档文件名附带稳定会话标识的哈希后缀，避免同时间同标题的不同会话覆盖。已有无冲突且标题正确的路径保持不变；历史冲突或错误标题重新导出后可能产生旧文件，需在停止服务、备份归档后核对索引再清理，不能按标题直接合并不同会话。

Mac 只读取本机日志。Mac 客户端中可见的 Windows 远端任务不会因此成为 Mac 本地源日志；应由 Windows 采集到其独占目录，再由 Mac 拉取供日报使用。

同路径更新不需要修改 JSON；更换路径时须先用原配置 stop，再更新配置和系统授权。更新不清理归档、日志或重试状态，也不绕过冷却。

历史大文件问题是归档仓库内容问题，单独升级 Waylog 程序不会把已经提交的普通 Git blob 自动转成 LFS。未来脚本自己的待推送提交可按上面的 GH001 自动恢复；人工/iBuddy 的未推送历史仍需手动处理。本次没有对真实仓库运行迁移。GitHub 对超过 100 MiB 的普通 Git 文件拒绝接收，增加一次删除/追踪提交不能消除旧历史里的 blob，参见 [GitHub 大文件说明](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github) 和 [Git LFS migrate 官方文档](https://github.com/git-lfs/git-lfs/blob/main/docs/man/git-lfs-migrate.adoc)。
