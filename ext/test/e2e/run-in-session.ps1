# Runs run.mjs in a non-elevated desktop session via a one-off scheduled task (session 0 cannot host WebView2).
param(
    [switch]$Inside,
    [string]$Node = '',
    [string]$HarnessArgs = '',
    [int]$TimeoutMinutes = 90
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$session = Join-Path $root 'target\e2e\session'
New-Item -ItemType Directory -Force -Path $session | Out-Null
$context = Join-Path $session 'context.txt'
$outLog = Join-Path $session 'run.out.log'
$errLog = Join-Path $session 'run.err.log'
$exitFile = Join-Path $session 'exit.txt'
$doneFile = Join-Path $session 'done.flag'

if ($Inside) {
    $code = 2
    try {
        $level = (whoami /groups | Select-String 'Mandatory Level').Line
        "session=$((Get-Process -Id $PID).SessionId) user=$env:USERNAME $level" | Set-Content $context
        $harness = @('ext\test\e2e\run.mjs') + @($HarnessArgs -split '\s+' | Where-Object { $_ })
        $run = Start-Process -FilePath $Node -ArgumentList $harness -WorkingDirectory $root -NoNewWindow -Wait -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
        $code = $run.ExitCode
    } catch {
        $_ | Out-String | Set-Content $errLog
    } finally {
        # done.flag is written last, so the waiter never reads a partial exit.txt.
        Set-Content -Path $exitFile -Value $code
        New-Item -ItemType File -Force -Path $doneFile | Out-Null
    }
    return
}

$node = (Get-Command node -ErrorAction Stop).Source
$task = 'ripdl-e2e-session'
Remove-Item $exitFile, $doneFile -ErrorAction SilentlyContinue
$taskArgs = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`" -Inside -Node `"$node`" -HarnessArgs `"$HarnessArgs`""
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $taskArgs -WorkingDirectory $root
$principal = New-ScheduledTaskPrincipal -UserId ([System.Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes $TimeoutMinutes)
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Settings $settings -Force | Out-Null
# A per-call event id keeps events queued by earlier calls in this shell from
# ending the wait early; done.flag is re-checked after every wakeup.
$eventId = 'ripdl-e2e-done-' + [guid]::NewGuid().ToString('N')
$watcher = New-Object System.IO.FileSystemWatcher $session, 'done.flag'
$watcher.EnableRaisingEvents = $true
Register-ObjectEvent -InputObject $watcher -EventName Created -SourceIdentifier $eventId | Out-Null
$deadline = (Get-Date).AddMinutes($TimeoutMinutes)
$finished = $false
try {
    Start-ScheduledTask -TaskName $task
    while (-not $finished) {
        if (Test-Path $doneFile) { $finished = $true; break }
        $left = [int]($deadline - (Get-Date)).TotalSeconds
        if ($left -le 0) { break }
        Wait-Event -SourceIdentifier $eventId -Timeout $left | Out-Null
    }
} finally {
    Unregister-Event -SourceIdentifier $eventId -ErrorAction SilentlyContinue
    Remove-Event -SourceIdentifier $eventId -ErrorAction SilentlyContinue
    $watcher.Dispose()
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
}
if (-not $finished) { throw "no result within $TimeoutMinutes minutes; see $session" }
Get-Content $context -ErrorAction SilentlyContinue
"harness exit code: $((Get-Content $exitFile).Trim())"
Get-Content $errLog -Tail 20 -ErrorAction SilentlyContinue
Get-Content $outLog -Tail 80
