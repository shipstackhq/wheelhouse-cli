<#
.SYNOPSIS
  Registers the Wheelhouse daemon as a per-user Scheduled Task that starts at logon (Windows).
.EXAMPLE
  .\install.ps1 -Entry C:\path\to\daemon\src\main.mjs -WhatIf
  .\install.ps1 -Entry C:\path\to\daemon\src\main.mjs
#>
[CmdletBinding(SupportsShouldProcess)]
param(
  [Parameter(Mandatory)][string]$Entry,
  [string]$Node = (Get-Command node -ErrorAction SilentlyContinue).Source
)

$ErrorActionPreference = 'Stop'
$TaskName = 'Shipstack Wheelhouse Daemon'
$Template = Join-Path $PSScriptRoot 'wheelhouse-daemon-task.xml.template'

# Refuse to register anything for a program that is not there.
if (-not (Test-Path -LiteralPath $Entry -PathType Leaf)) {
  throw "refusing to install: no daemon entry file at $Entry (the daemon is not built yet)"
}
if (-not $Node -or -not (Test-Path -LiteralPath $Node -PathType Leaf)) {
  throw 'refusing to install: node.exe not found; pass -Node <path>'
}
$EntryFull = (Resolve-Path -LiteralPath $Entry).Path
$WorkDir = Split-Path -Parent $EntryFull
$User = "$env:USERDOMAIN\$env:USERNAME"

# XML-escape every value before it goes into the template.
$esc = { param($s) [System.Security.SecurityElement]::Escape([string]$s) }
$xml = (Get-Content -LiteralPath $Template -Raw -Encoding UTF8) `
  -replace '__USER__', (& $esc $User) `
  -replace '__NODE__', (& $esc $Node) `
  -replace '__ENTRY__', (& $esc $EntryFull) `
  -replace '__WORKDIR__', (& $esc $WorkDir)

if ($PSCmdlet.ShouldProcess($TaskName, "register logon task for $User")) {
  Register-ScheduledTask -TaskName $TaskName -Xml $xml -Force | Out-Null
  Write-Output "installed '$TaskName' for $User"
} else {
  Write-Output "would register '$TaskName' for $User running: `"$Node`" `"$EntryFull`" (working directory $WorkDir)"
}
