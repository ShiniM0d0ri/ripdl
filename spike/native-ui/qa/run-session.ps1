# Start the native UI spike in the interactive desktop session with the
# egui inspection port open, or stop it with -Stop.
param([switch]$Stop, [string]$Screen = 'main')

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$wrapper = Join-Path $PSScriptRoot 'run-app.cmd'
$task = 'ripdl-native-ui-spike'

if ($Stop) {
    Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
    'stopped'
    return
}

if (-not (Test-Path $wrapper)) { throw "missing $wrapper" }
Set-Content -Path (Join-Path $PSScriptRoot 'screen.txt') -Value $Screen -NoNewline
Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
$action = New-ScheduledTaskAction -Execute $wrapper -WorkingDirectory $root
$principal = New-ScheduledTaskPrincipal -UserId ([System.Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Hours 2)
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName $task

$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $client.Connect('127.0.0.1', 5719)
        'inspection port 5719 is open'
        return
    } catch {
    } finally {
        $client.Close()
    }
    Start-Sleep -Milliseconds 500
}
throw 'inspection port did not open; task may have failed to start'
