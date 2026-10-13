# Requires Windows PowerShell 5.1+ and Python 3.9+.
# No service is installed until the explicit start command.
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('start', 'stop', 'status', 'sync', 'doctor', 'logs', 'unblock', 'resume-sync', 'init-repo', 'config')]
    [string]$Command = 'status',
    [string]$ConfigPath = "$PSScriptRoot\waylog-service.windows.json",
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This entry point requires Windows.' }
if ($DryRun -and $Command -ne 'sync') { throw '-DryRun is only supported for sync.' }
if (-not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
    throw 'Copy waylog-service.windows.example.json to waylog-service.windows.json and check git_repo/repo_path first.'
}
$ConfigPath = (Resolve-Path -LiteralPath $ConfigPath).Path
$configSource = Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8
# Preserve quoted URLs, escaped quotes and Windows paths while removing // comments.
$configSource = [regex]::Replace($configSource, '"(?:\\.|[^"\\])*"|//[^\r\n]*', {
    param($match)
    if ($match.Value.StartsWith('"')) { return $match.Value }
    return ' ' * $match.Value.Length
})
$bootstrapConfig = $configSource | ConvertFrom-Json
$pythonSetting = 'python'
$pythonProperty = $bootstrapConfig.PSObject.Properties['python']
if ($null -ne $pythonProperty) { $pythonSetting = [string]$pythonProperty.Value }
function Resolve-Executable([string]$Value) {
    $expanded = [Environment]::ExpandEnvironmentVariables($Value)
    $expanded = [regex]::Replace($expanded, '\$(?:\{([^}]+)\}|([A-Za-z_][A-Za-z0-9_]*))', {
        param($match)
        $name = if ($match.Groups[1].Success) { $match.Groups[1].Value } else { $match.Groups[2].Value }
        $replacement = [Environment]::GetEnvironmentVariable($name)
        if ($null -eq $replacement) { return $match.Value }
        return $replacement
    })
    if ($expanded.StartsWith('~')) { $expanded = $env:USERPROFILE + $expanded.Substring(1) }
    if (($expanded.Contains('\') -or $expanded.Contains('/')) -and -not [IO.Path]::IsPathRooted($expanded)) {
        $expanded = Join-Path (Split-Path -Parent $ConfigPath) $expanded
    }
    $application = Get-Command -Name $expanded -CommandType Application -ErrorAction Stop | Select-Object -First 1
    return $application.Source
}
$pythonLauncher = Resolve-Executable $pythonSetting
$python = & $pythonLauncher -c 'import sys; assert sys.version_info >= (3,9), "Python 3.9+ required"; print(sys.executable)'
if ($LASTEXITCODE -ne 0) { throw 'Cannot run Python 3.9+; configure python with a real python.exe path.' }
$python = [string]($python | Select-Object -Last 1)
$core = Join-Path $PSScriptRoot 'waylog-service.py'
$coreArgs = @($core, '--config', $ConfigPath)
$configText = & $python @coreArgs config
if ($LASTEXITCODE -ne 0) { throw 'Configuration validation failed.' }
$config = $configText | ConvertFrom-Json
$state = [string]$config.state_dir
$normalizedRepoPath = ([string]$config.repo_path).Replace('\', '/').Trim('/').ToLowerInvariant()
if ($normalizedRepoPath -eq 'ai/raw/wk-me' -or $normalizedRepoPath.StartsWith('ai/raw/wk-me/')) {
    throw 'Choose a Windows-specific repo_path; ai/raw/wk-me belongs to the Mac.'
}

function Invoke-Core([string]$Action, [switch]$Preview) {
    $arguments = @($coreArgs)
    if ($Preview) { $arguments += '--dry-run' }
    $arguments += $Action
    & $python @arguments
    if ($LASTEXITCODE -ne 0) { throw "Waylog command failed: $Action (exit $LASTEXITCODE)" }
}

# Use the current user's SID so accounts do not overwrite each other's tasks.
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$taskPrefix = 'Waylog-Service-' + $identity.User.Value + '-'
$ownerDescription = 'Waylog managed service: ' + [IO.Path]::GetFullPath($core) + ' | config=' + $ConfigPath

function Get-OwnedTask([string]$Kind) {
    $task = Get-ScheduledTask -TaskName ($taskPrefix + $Kind) -TaskPath '\' -ErrorAction SilentlyContinue
    if ($task -and $task.Description -ne $ownerDescription) {
        throw "Task name is occupied by a different script/configuration: $($task.TaskName). Use the original configuration to stop it first."
    }
    return $task
}

if ($Command -eq 'status') {
    Invoke-Core 'status'
    foreach ($kind in @('watch', 'sync')) {
        $task = Get-OwnedTask $kind
        if ($task) {
            $info = Get-ScheduledTaskInfo -TaskName $task.TaskName -TaskPath '\'
            [pscustomobject]@{ Task = $task.TaskName; State = $task.State; LastRun = $info.LastRunTime;
                              LastResult = $info.LastTaskResult; NextRun = $info.NextRunTime } | Format-List
        } else { Write-Output "$kind task is not registered." }
    }
    exit 0
}

if ($Command -eq 'stop') {
    # Request cooperative cancellation before falling back to Task Scheduler termination.
    $owned = @()
    foreach ($kind in @('watch', 'sync')) {
        $task = Get-OwnedTask $kind
        if ($task) {
            $owned += $task
            New-Item -ItemType Directory -Path $state -Force | Out-Null
            [IO.File]::WriteAllText((Join-Path $state "$kind.stop"), 'stop')
        }
    }
    if ($owned.Count -gt 0) { Start-Sleep -Seconds 3 }
    foreach ($task in $owned) {
        $current = Get-ScheduledTask -TaskName $task.TaskName -TaskPath '\'
        if ($current.State -eq 'Running') { Stop-ScheduledTask -TaskName $task.TaskName -TaskPath '\' }
        Unregister-ScheduledTask -TaskName $task.TaskName -TaskPath '\' -Confirm:$false
        Write-Output "Stopped $($task.TaskName); archive, logs and pending commits were preserved."
    }
    exit 0
}

if ($Command -ne 'start') {
    Invoke-Core $Command -Preview:$DryRun
    exit 0
}

# The only branch that registers or starts background tasks.
Invoke-Core 'doctor'
$existingWatch = Get-OwnedTask 'watch'
$existingSync = Get-OwnedTask 'sync'
if ($existingWatch -or $existingSync) {
    throw 'Tasks are already registered. Use status, or stop then start to change configuration.'
}
$specArgs = @($coreArgs) + 'task-spec'
$specText = & $python @specArgs
if ($LASTEXITCODE -ne 0) { throw 'Cannot generate task action arguments.' }
$spec = $specText | ConvertFrom-Json
New-Item -ItemType Directory -Path $state -Force | Out-Null
$principal = New-ScheduledTaskPrincipal -UserId $identity.User.Value -LogonType Interactive -RunLevel Limited
$registered = @()
try {
    foreach ($kind in @('watch', 'sync')) {
        $action = New-ScheduledTaskAction -Execute $spec.executable -Argument $spec.$kind -WorkingDirectory $state
        $logon = New-ScheduledTaskTrigger -AtLogOn -User $identity.User.Value
        if ($kind -eq 'watch') {
            $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) `
                -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable `
                -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
            $triggers = @($logon)
        } else {
            $now = Get-Date
            $first = $now.Date.AddHours($now.Hour + 1)
            $hourly = New-ScheduledTaskTrigger -Once -At $first -RepetitionInterval (New-TimeSpan -Seconds ([int]$config.interval))
            $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 30) `
                -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
            $triggers = @($logon, $hourly)
        }
        Register-ScheduledTask -TaskName ($taskPrefix + $kind) -TaskPath '\' -Action $action `
            -Trigger $triggers -Settings $settings -Principal $principal -Description $ownerDescription | Out-Null
        $registered += ($taskPrefix + $kind)
        # Only remove this script's cancellation markers after successful registration.
        $marker = Join-Path $state "$kind.stop"
        if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker }
    }
} catch {
    foreach ($name in $registered) {
        Unregister-ScheduledTask -TaskName $name -TaskPath '\' -Confirm:$false -ErrorAction SilentlyContinue
    }
    throw
}
foreach ($name in $registered) { Start-ScheduledTask -TaskName $name -TaskPath '\' }
Write-Output 'Started watch and hourly Git sync. Initial sync may immediately commit and push.'
