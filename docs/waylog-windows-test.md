# Windows 0.2.8 测试包

此包用于手动测试，不自动安装、不注册任务、不同步真实归档。GitHub Windows runner 验证 Rust 测试、release 编译、命令参数以及 PowerShell 5.1 的带注释配置读取。公司的 Windows 上采集、SSH/LFS、任务计划程序仍需现场验证。

解压后先在包目录运行：

```powershell
.\waylog.exe --version
.\waylog.exe watch --help
.\scripts\waylog-service.windows.ps1 config -ConfigPath .\scripts\waylog-service.windows.example.json
Get-FileHash .\waylog.exe -Algorithm SHA256
```

版本应为 `0.2.8`，哈希与 `build-info.json` 一致。这些命令不采集、不联网、不启动任务。测试包不覆盖 `%USERPROFILE%\.cargo\bin\waylog.exe`。

要手动测试《验收工程服务版本元数据》，先在 Windows 本地 Codex 会话目录找到线程 ID `01a1334b-bef5-7c12-8ea9-10d52850bd2e` 对应的 JSONL。确认该文件后，在包目录导出到一个全新的临时目录，避免写入正式归档：

```powershell
$testArchive = Join-Path $env:TEMP ('waylog-test-' + [guid]::NewGuid().ToString('N'))
.\waylog.exe export --provider codex --no-browser --archive-dir $testArchive
$testArchive
```

此命令只读取本机 Codex 日志，可能导出多条历史会话，不拉取远端任务、不联网、不推送。核对 `indexes\sessions.jsonl` 中该线程的 `session_id`，以及对应 Markdown/原始文件；文件名可能使用首条有效用户消息而非 App 的任务标题，因此应按线程 ID 查找。不要把这个临时目录当作正式同步目录；不要按标题合并不同会话。

后台启用步骤见同包的 `waylog-service-windows.md`。正式启用前再配置 `waylog` 为准备安装的程序路径，运行 doctor 和 dry-run；测试阶段不运行 start、init-repo 或真实 sync。
