# Waylog 设备采集状态 v1 与服务调度 v2（离线开发）

本轮不部署、不安装、不注册任务、不读取真实会话/浏览器/归档、不联网同步、不提交。旧 `waylog-service.py` 和 Windows 入口保持不变；新的入口是 `scripts/waylog-service-v2.py`，需要显式 `--config`。修改中的 Rust 二进制未安装。原始归档内容变化检测、文件命名和迁移规则不在本轮范围。

## 字段契约与读取边界

每设备只有 `ai/raw/<设备>/indexes/coverage.json` 一份文件。`schemaVersion=1`；正式结构见 `coverage-v1.schema.json`，虚构多日样例见 `examples/coverage-v1/`。这不是 iHermes 旧 `coverage-receipts.md` 协议，消费者必须单独适配；不能直接据此开启正式日报生成。

- `deviceId`：本机设备标识。文件与设备不匹配时拒绝合并，不写其他设备。
- `timezone`：覆盖日期固定为 `Asia/Shanghai`；与同步 cron 的时区独立。
- `asOfDate`：保留窗口的本地日期锚点，只在日期推进时变化，不是逐轮心跳。缺失时消费者不能推断裁剪截止日。窗口内应有但缺失的日期为未知，窗口外为超保留期，不能一律当作零记录。
- `sources`：本设备来源名单。会话按 provider，浏览器按 root 和发现的 profile 登记；失败、不可读、禁用和暂时未发现的来源不从名单删除。`enabled` 表示配置，`discovery` 表示发现结果，不能从是否有产出推导。
- `days[YYYY-MM-DD]`：该日记录。`dataVersion` 是生成这些状态时的归档资产快照摘要，无法计算时为 null；null 不能证明成功覆盖。旧日版本不会自动变成当前版本，也不等于 GitHub 最新版本。
- `attemptedRanges`：实际尝试的范围；`successfulRanges`：已确认读取成功的子范围。半开区间 `[start,endExclusive)`。完整状态必须有明确的全日成功范围及归档检查，不能由最新消息时间、manifest 更新时间或空增量推断。
- `recordCount`：本次可确认的目标日数量；单位由 `countUnit` 指定。null 表示不能确认；partial 下的数量不是“所有来源完整日总量”。会话计解析并允许归档的消息，浏览器计已有归档索引的访问记录。没有消息的会话和被过滤的内部会话不构成当日聊天。
- `archiveStatus`：归档写入/核对结果，独立于来源读取。
- `lastSuccessfulRanges` / `lastFailureReason`：同一数据版本下保留最近已知成功范围和最近失败原因；不是当前失败轮次的成功证明。数据版本变化后不沿用旧成功范围。
- `reasonCode`：稳定原因码，不写异常正文、源绝对路径、会话内容、敏感 URL 或凭据。
- `evidenceScope`：范围是本机保留的源日志快照，不能证明日志保留前的活动、远端电脑活动或外部程序尚未落盘的内容。

状态的意义：`records`=全日读取与归档成功且有记录；`zero`=同样有全日证明且数量为零；`partial`=只能确认部分范围/枚举结果；`pending`=需补查；`unknown`=无法判断；`failed`=明确读取/归档失败；`disabled`=配置禁用；`stopped`=显式停止。本轮不会把一次无新增扫描标为 zero。

**现有来源的证明限制**：provider 枚举器和 parser 会跳过部分错误，浏览器是增量查询；所以原生 watch 当前保守产生 partial/pending/unknown/failed，不输出全日 records/zero。完整状态仅由明确提交全日证明的受检生产端接口接受，离线样例/测试使用虚构完整扫描证据。若需要原生采集器实际证明昨日零记录，后续必须增加严格的来源范围/解析审计；不能用当前枚举器补造证明。

## 保留、合并与写入

保留区间为 `(去年同月同日周年边界, 今天]`，不是自然年度 1 月 1 日，也不是固定 365 天。2 月 29 日的上一年无该日时，下界取 2 月 28 日；2024-02-29 的下界是 2023-02-28，2025-02-28 的下界是 2024-02-28。只裁剪 coverage 的旧条目，不删除归档、索引或源日志。未来日期拒绝写入。

调用方持有既有归档锁后，读取旧文件、校验设备/schema、合并来源和日期，写同目录临时文件并 fsync，再原子替换。内容一致时不写；解析损坏文件时拒绝覆盖。新版本未重新核对的来源降为 pending。当前 watch 正常轮次同时登记今天和昨天；没有固定次日任务。

覆盖文件不含逐轮心跳、Git 状态或上传成功字段。`collection-state.json` 写在归档锁所在的本机状态目录，记录真实轮次时间和 running/stopped；缺失/陈旧心跳只能解释为未知，不等于显式停采。停止时持锁更新本日 stopped；若归档锁正忙，先写本机 stopped，覆盖文件不能作为最新停采状态依据。

资产版本算法：对排序后的相对路径，拼接 UTF-8 路径 + NUL + 该文件的 SHA256 十六进制 + LF，再对整体 SHA256。范围为 `sessions/**`、`browser-history/**`、`indexes/sessions.jsonl`、`indexes/browser-history.jsonl`；排除 coverage、自身临时文件和 manifest 时间。消费者需要相同的已实体化字节（含 LFS 内容）才能核对；Git blob/tree ID 与此摘要不是同一种标识。换行不同导致不匹配时应保持候选/未知，不能静默当作相同版本。

## 同步结果仅保留本机

v2 扩展本机 `status.json`：`pending_data_version`、`current_data_version`、`upload_attempt_data_version`、`upload_result`、`last_uploaded_data_version`、`last_uploaded_commit`、`last_uploaded_archive_tree`。版本在实际 push 之前、仍持有归档锁时确定；A 成功不能确认随后 B。传输中断保留 unknown 和原退避预约，不能马上重传；保存尝试的 commit/tree，下一轮 fetch 确认该 commit 已在远端时只确认该次尝试版本，不确认后来变化的 B。拉取失败、上传成功/失败均不改 coverage。文件变化不触发任何同步。

## 配置与原生任务规格

旧 `interval` 继续兼容；也可用 `sync_schedule={"mode":"interval","seconds":3600}`。新对象和旧 interval 不能同时出现。cron 示例：

```json
{"sync_schedule":{"mode":"cron","expression":"0 9,12,18 * * 1-5","timezone":"Asia/Shanghai"},"coverage_enabled":true,"collection_sources":["codex","claude","chrome"],"watch_interval":30}
```

五段 cron 支持数字、`*`、列表、范围和步长；星期日 0/7，受限日与受限星期按 OR。拒绝别名、秒段、L/W/#、非法值和未知时区。时区使用 IANA zoneinfo；Windows 环境要事先具有时区数据库，缺少时显式报错，不联网安装。DST 跳过不存在分钟，重复本地分钟按不同 UTC 时刻执行。

interval 以 UTC epoch 槽对齐，避免调度器几秒延迟压掉下一轮。cron 原生执行器每分钟检查，业务只执行符合表达式的槽；延迟唤醒合并成最多一次执行，不回放全部错过的轮次。成功按模式等待下个触发点；cron 不受旧 interval=3600 的成功等待压制。失败仍使用 minimum_network_interval、retry_delays、retry_max_failures，不因短 cron 绕过；分钟轮询只读门控，不是分钟上传重试。

`native-spec` 输出纯规格，不注册任务：Mac 同步使用独立 v2 dispatcher，原生 watch 直接执行安装版 Waylog；Windows dispatcher/watch supervisor 使用 pythonw，保留单实例、日志和停止标记。旧 Mac 每小时、Windows 稀疏/空闲不拉取策略不变。`coverage_enabled` 默认 false；空 collection_sources 表示显式禁用所有来源，null/省略保留旧默认发现。

## 尚未执行的部署与平台验收

之后需另行授权：构建并安装新版原生 Waylog；给 Mac 安装路径重新确认权限；选定 v2 配置并注册所生成原生规格；Windows 时区库、pythonw、原子替换、进程退出/登录恢复的本机验收；检查真实采集/同步结果。当前仅代码编译和隔离验证，无真实启用。

旧入口仍活跃的机器继续运行旧逻辑。不要直接替换旧任务引用路径或把示例复制成真实配置。原生完整日零记录的严格扫描审计、iHermes P4.3c 消费适配及正式资格均未完成；上传回执不是全日采集证明。

## 本轮离线验证结果

- Rust coverage：16 项，含多日样例解析、零记录的全日证据约束、设备隔离、年度/闰年裁剪、错误更新不局部修改、损坏索引、原子保存和重复轮次、显式停采和本机心跳隔离。
- Rust watch 协调：12 项，使用临时锁/仓库验证单实例、跨进程锁、忙锁跳过和未完成 Git 操作保护。
- 浏览器虚构 SQLite：1 项，成功和失败 profile 同时保留名单，失败不会变成零记录。
- Python v2：24 项，含兼容/非法配置、interval/cron、时区与 DST、延迟合并、重叠触发、短 cron 成功等待、失败退避/暂停、未知上传恢复、A/B 版本隔离、无窗口日志、模拟原生执行器和旧二进制能力拒绝。

合计 53 项定向测试通过。使用 `cargo test --offline`，未执行安装、真实 watch、原生任务注册或真实 Git fetch/push。旧服务入口和本机配置文件没有修改。Python 编译检查、指定 Rust 文件格式检查和 Git diff 空白检查通过；完整原生 Windows/安装后 Mac 验收未执行。
