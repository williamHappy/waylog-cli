# Waylog 轻量采集状态

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
8. 上传失败后保留该提交；重试不重新取检查时间、不加入新采集文件、不再次生成提交，也不在重试中 rebase（重写提交顺序）或强推。其他设备抢先推送导致远端拒绝时，保留待上传提交，按原限制退避，最终暂停等待处理。
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
