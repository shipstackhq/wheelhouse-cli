<#
.SYNOPSIS
  Removes the Wheelhouse daemon's Scheduled Task (Windows). It does not touch the daemon's state folder, so job state and logs are kept.
#>
[CmdletBinding(SupportsShouldProcess)]
param()

$ErrorActionPreference = 'Stop'
$TaskName = 'Shipstack Wheelhouse Daemon'
$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if (-not $task) {
  Write-Output "no task named '$TaskName'"
  return
}
if ($PSCmdlet.ShouldProcess($TaskName, 'unregister')) {
  Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
  Write-Output "removed '$TaskName'"
}
