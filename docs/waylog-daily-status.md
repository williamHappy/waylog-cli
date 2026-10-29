# Waylog 轻量采集状态

## 2026-10-17 两端修复验收

Mac、Windows 安装版均为 0.2.10，四个同步服务脚本内容已核对一致。本次修复只涉及 Python 服务脚本，无需再次构建 Rust 程序。

- 并发推送遇到远端前移时，在同一轮内最多尝试三次普通推送，合并远端更新后再推送，保留总超时限制；真实内容冲突仍停止处理，不强推、不重写归档历史。连续三轮失败暂停策略保留。
- GH001 大文件警告不再误判为拒绝；真实大文件错误仍单独处理。
- 状态说明的读取、日期校验、合并或写入异常记录为警告，不阻断正常数据归档上传。设备归属、路径、快照和待上传队列等必要数据检查保留。
- Mac 已安全恢复并核对积压数据：1982 个归档文件、9 个 LFS 指针及原有 3 个待上传提交均已验证；后台后续上传成功，失败次数为零。
- Windows 完成原生离线测试 90 项（v2 26、按日状态 44、并发推送 20），同步任务已启用且正常退出，采集进程继续运行。更新后的检查没有新增数据，正常跳过上传，不能视为修复后的真实推送验收。
- 每日 23:00 自动检查点及 Mac Chrome 权限恢复仍须以实际后续运行结果为准。下节保留 10 月 16 日的阶段记录，其中暂停、Windows 未操作等描述不代表当前状态。

升级和修复备份在验收通过后及时清理；此前备份路径仅为历史记录，不作为长期恢复入口。

## 2026-10-16 Mac 部署状态

用户授权升级后，本机安装版已从 0.2.9 更新为 **0.2.10**，使用当前本地定制源码离线构建，未拉取或推送源码。Windows 未操作。

- 原生 watch 已重启，仍由 launchd 直接运行 `~/.cargo/bin/waylog`，采集间隔 30 秒；以 `--daily-status-file` 替换严格 `--coverage`。
- 现有 v2 配置启用 `daily_status`，普通同步仍每小时，每日检查点先设为北京时间 23:00；launchd 每分钟只做本地门控，不表示每分钟上传。
- 实测已产生本机 `daily-status.json` 的 `dailyChecks["2026-10-16"]`。Codex、Claude、Gemini 和 Atlas 检查成功；Chrome 返回 `Operation not permitted`，如实记录 `browser_read_failed`。需用户在 macOS 完全磁盘访问权限中核对实际安装路径，不给通用 Python 授权。
- 原 `network-state.json` 的三次失败暂停原样保留，未调用 `resume-sync`、手动 sync 或上传；新状态仅排在本机待上报队列，尚未写入/推送归档的 coverage。没有补造 10 月 15 日检查结果。
- 修复上线实测发现的等待采集锁时间误判：在获取锁后再取当前时间，避免锁等待期间新写的检查被误报为未来时间，包含同日/跨午夜回归测试。修复后 dispatcher 退出码 0，返回 deferred，符合既有网络暂停。
- 离线测试：Rust 187、按日状态 29、虚构样例 13、v2 25、原服务 96 项通过。当前 Python 环境未启用 JSON Schema 包，不能把 consumer 验证计作完整 Schema 验证。

备份及完整本机验收摘要：`~/Library/Application Support/waylog-service/backups/upgrade-0.2.10-20261016-233923/`。原配置、任务、二进制、暂停状态及本地源码均保留。下文“本轮/尚未部署”描述此前离线交付边界，当前 Mac 部署以上段为准；Windows、Chrome 权限恢复、真实上传和每日到点执行仍未验收。

目标仅是让日报知道“哪台设备最近实际检查过、哪些来源出错”，以及本机知道“上传有没有成功”。不做整日完整性证明、不核对全量记录、不扫描历史重复文件，不恢复严格审计。

## 文件及含义

- `src/archive/daily_status.rs`：原生 Waylog 将检查结果写到仓库外的本机 `daily-status.json`。不依赖 Python 读取浏览器。
- `src/archive/coverage.rs`、`docs/coverage-v1.schema.json`：兼容旧 coverage v1，增加轻量字段；旧 `retention/sources/days` 对新最小文件可省略，已有字段保留。
- `src/commands/watch.rs`、`src/cli.rs`、`src/main.rs`：显式 `--daily-status-file` 新入口，必须使用现有归档锁，不能同时使用严格 `--coverage`。
- `src/providers/{codex,claude,gemini}.rs`：已知目录遍历/文件读取错误向上传递；Codex 不再将损坏 JSON 当作正常源文件跳过。不把这种检查当作所有历史事件语义均已核对。
- `scripts/waylog_daily_status.py`：状态合并、明确停用/恢复、每天上报去重、冻结待上传队列。上传结果只写本机。
- `scripts/waylog_light_transport.py`：现有本地 Git、超时、锁、SSH/LFS 和失败退避的适配层，不调用全库 inventory 或 archive_version。
- `scripts/waylog-service-v2.py`：默认关闭的新分支；现有未启用配置继续走原流程。没有改现有任务注册或真实配置。
- `scripts/generate_daily_status_fixtures.py`、`scripts/test-waylog-daily-status.py`：虚构样例生成及隔离测试。

每设备对外文件仍是 `ai/raw/<device>/indexes/coverage.json`：

```json
{
  "schemaVersion": 1,
  "deviceId": "wk-me",
  "timezone": "Asia/Shanghai",
  "collectionEnabled": true,
  "dailyChecks": {
    "2026-10-14": {
      "checkedAt": "2026-10-14T10:00:00+08:00",
      "result": "failed",
      "sources": {
        "session:codex": {
          "result": "failed",
          "stage": "parse",
          "reasonCode": "session_read_or_parse_failed"
        },
        "session:claude": { "result": "ok" }
      }
    }
  }
}
```

- `checkedAt` 是实际完成检查的时间，带时区且对应上海日期。没有检查就没有该日记录，不补昨日成功。无新记录但确实成功读过来源，可以记 `ok`。
- `stage` 只允许 `collection/read/parse/archive`；`reasonCode` 只用 1–96 位英文字母、数字、下划线或短横线。
- 每个来源的未恢复错误保留到该来源真实成功；其他来源成功不清除它。显式不选用的来源可标记 `sources[id].enabled=false`，不计入设备失败。
- 保留最近一个日历年；二月二十九日周年按二月二十八日计算。旧索引/会话不做迁移或清理。新日结果优先，恢复后清理同日旧失败状态，保留旧计数、版本及最后失败原因，不赋予整日证明。
- `collectionEnabled=false` 只由明确停用操作写入；普通故障/离线不自动停用。恢复只表示允许继续采集，不编造一次成功检查。
- 本机 `daily-status.json` 包含 `receipt` 和 `controlRevision`，后者仅用于同步停用/恢复声明，不上传。watch 与控制指令使用同一归档锁，原子更新同一文件。
- 本机 `daily-report-state.json` 保存已完成检查点、待上传提交和上传结果；这些元数据、心跳永远不进 coverage，避免“上传确认又触发一次上传”。

## 同步流程

1. 原生 watch 实际检查会话/浏览器，将结果留本机；不因每轮更新时间就制造 Git 提交。
2. 常规 interval/cron 到时，本地 Git 检查本设备归档是否变化。有数据时，状态与数据同一提交。没有数据时，Mac 可按 `pull_on_idle=true` 拉取，Windows 按 `false` 跳过。
3. 额外独立的每天检查点到时，即使没有新数据也尝试上报一次。本日从未检查时只上传缺少当日结果的真实状态，不添加虚构记录。先前普通数据上传不能代替该检查点；内容完全相同时复用已有提交，不创建空提交。
4. 明确停用/恢复声明也能在无数据、采集已停止时排队同步；仍受网络退避/暂停限制。
5. 每设备每天检查点只有确认上传成功后才标记完成，重复触发去重。离线积压的真实日记录可一并补报，原日期/checkedAt 不变。不会为缺失的过去日期补成功。
6. 读取/解析失败仍可携带其他数据上报错误。归档写入失败时仅提交状态，保留未提交的数据文件供恢复，避免坏文件挡住故障报告。
7. 首次准备先拉取并尝试快进（仅移动到兼容的远端提交）；冲突/分叉不自动改历史、不强推。生成提交后把提交编号写入本机待上传队列，再上传 LFS/Git。
8. 上传失败后保留原始冻结提交；重试不重新取检查时间、不加入新采集文件、不 rebase 或强推。仅在明确的远端分支更新竞争时，重新 fetch 并合并已发布历史，生成原冻结提交的后继提交；本机归档 tree 必须与冻结提交完全一致。队列分别记录原始 `commit` 和实际候选 `uploadCommit`，合并前持久化 `integration` 预约，支持合并后中断恢复。远端改动本机归档、真正合并冲突或未知 HEAD 变化均停止自动处理。
9. 每轮最多 3 次普通推送，重试之间短暂随机等待；LFS、fetch、整合及推送共享 `timeout` 时间预算，整轮只占用一次连续失败计数。耗尽后沿用原退避与三轮失败暂停，不自动解除已有暂停。`remote: warning` 的 GH001 只记警告；明确的 `remote: error: GH001` 才触发大文件拒绝暂停。冻结批次遇到真实大文件拒绝时，先备份尚未发布的提交，再仅将 GitHub 明确报告的本设备文件转换成 LFS 指针，同时更新批次提交及合并候选编号；本轮不立即重推，下一允许周期上传。已发布提交和其他引用保持不变。
10. 日报时间校验使用拿到归档锁后的时钟，避免等待期间采集器写入的更新被误判为未来记录；仍拒绝真正的未来时间、日期与 `checkedAt` 不符等无效记录，错误日志包含日期、检查时间和校验时钟。
9. 沿用已有网络状态：默认失败等待一小时、两小时，累计三次暂停；每个 LFS 对象限制一次内置尝试，继承 Git 命令超时。每分钟检查到退避中，只做本地判断，不联网。人工检查原因后可使用 v2 `resume-sync`，恢复不会绕过已有待上传提交。
10. 进程在提交后、保存编号前崩溃时，可用该队列的专属提交标记和父提交恢复同一提交；无法确认所有权则停止，保留现场。

检查点默认**关闭**，`23:00` 仅是示例，真实启用时间还未确认。配置样例全部使用 `/fictional` 路径，不能原样部署：`docs/waylog-daily-status.example.json`。新功能默认关闭，`coverage_enabled` 应保持 false。

```json
"daily_status": {
  "enabled": false,
  "checkpoint": { "enabled": false, "time": "23:00" }
}
```

`sync_schedule` 继续支持 interval 或五段 cron。启用每日检查点后，`native-spec` 只生成每分钟检查一次的任务描述，从而不受每小时主任务边界限制；仍未注册任何真实任务。Windows 仍使用 pythonw 无窗口描述；Mac watch 描述直接启动 Waylog。

本轮不提供一键安装/启用，避免改变现有工作任务。`collection-disable` / `collection-enable` 仅修改本机控制声明，下一次 dispatch 按退避规则同步；不负责操作系统任务启停。若整个 dispatch 任务也停止，声明须等恢复 dispatch 才能上传。

## 离线复测

```sh
cargo check --offline --bin waylog
cargo test --offline --bin waylog
python3 scripts/test-waylog-daily-status.py
python3 scripts/test-waylog-daily-fixtures.py
python3 scripts/test-waylog-v2.py
python3 scripts/test-waylog-service.py
```

新测试只使用虚构记录、模拟时钟、模拟 LFS 上传和临时本地 bare Git 仓库。外部 GitHub/SSH/LFS 服务没有调用。`jsonschema` 如未安装会明确显示 `schema: False`，不据此宣称完成 Schema 验证。本次使用已有 browser-use Python 环境，显示 `schema: True`。消费端校验仅导入已有 Python 判断函数，输入这批虚构 coverage，不调用真实归档读取。

本次通过的状态功能不能替代真实部署验收：尚未安装本轮二进制，未更改两端任务/配置，没有验证真实浏览器权限、Windows 原生运行、GitHub 上传或实际每日到点执行。没有提交 Git。


## 虚构样例的生成与清理

样例不再长期存放在仓库中。两份测试会自动在系统临时目录生成完整虚构归档，测试结束后清理；完整读取测试仍先复制未经修补的样例，再依次运行日报 read_raw 和 consume_coverage_files。

如需手动查看样例，可执行：

```sh
python3 scripts/generate_daily_status_fixtures.py
```

命令会输出一个新建的系统临时目录路径，手动生成的这一份供查看，不会自动删除。也可用 `--output <新建或空目录>` 指定位置；脚本拒绝覆盖非空目录。

样例包含12种场景：正常、零记录、枚举失败、读取失败、解析失败、归档失败、兄弟来源成功不掩盖错误、同来源恢复、明确停用、明确恢复、缺少检查、跨日积压。每种场景的 `i-raw-data/ai/raw/wk-me` 和 `wk-jzx` 都包含会话、浏览器空目录、索引及 coverage。所有内容均为虚构。

这批轻量功能的验证范围为离线测试，不代表真实 Mac/Windows 后台部署已经完成。阶段路径清单和验证记录已合并到本说明；旧严格审计实验文件已清理。


### 状态说明异常时继续上传正常归档

本地状态说明的 JSON、日期、设备标识或读取异常，以及归档中状态说明的读取、合并或写入异常，只产生本机 `status_warning` 警告。存在正常归档变化时，本轮继续上传归档，排除异常的 `indexes/coverage.json`，保留其本地内容和原已发布版本；不虚构成功检查，不确认本轮跳过的检查点或控制版本。仅有异常状态、没有归档数据时不推送，待状态恢复后再上报。尚未冻结的旧批次同样可以跳过异常说明；已冻结批次仍复用原提交。

归档索引语法、引用文件存在性、路径和符号链接保护、提交来源、冻结批次完整性及真实合并冲突检查继续有效。持久化队列及 Git 状态异常不属于可跳过的状态说明；连续三轮失败后的暂停规则保持原样。

## 冻结批次的 GH001 / LFS 恢复

`waylog_frozen_lfs.py` 使用本地 Git 对象操作，只替换缓存远端边界之后、经服务提交来源检查的未发布历史。LFS 对象直接从原提交的 blob 构造并保存到本地 LFS 缓存，文件原字节、提交消息、作者信息及所有已发布父提交均保留。不会把后来采集的工作区内容混入冻结批次，也不会 checkout/reset 工作区或强推。

每次转换先保存可验证的原始 Git bundle、原队列信息和旧新提交映射。在改变分支或索引前，将恢复预约写入队列；先完成分支与索引、LFS 规则，再更新队列编号。进程中断后，下一次调度在联网门控之前校验预约、内容及提交映射，完成同一项本地事务，再沿用原冷却周期。只有本次 GH001 的保护性暂停可以自动解除；达到重试次数上限的暂停不解除。

转换只接受明确的 `remote: error: GH001` 和对应文件路径。警告不触发转换；其他设备路径、带通配符/目录穿越的路径、带保护标签的待上传历史、签名提交、未知 HEAD/索引变化或备份失败等情形保留暂停，转人工检查。共享 `.gitattributes` 的合并只允许本设备合法 LFS 规则，其他设备规则不得被改写。

离线回归覆盖本地拒收钩子到实际本地 LFS 对象上传、并发合并、完整及 sparse 工作区、转换前/分支更新后/索引更新后的中断恢复、后续采集内容保留和安全拒绝。仅使用临时本地 bare 仓库，不访问 GitHub，不操作真实归档或后台任务。
