#requires -Version 5.1
<#
.SYNOPSIS
Opens the pool dashboard (`rampire top`) on the KVM host over SSH.
.DESCRIPTION
Runs on the Windows workstation. Over `ssh -t` it runs the host's installed
`rampire top` through sudo, so it can read the controller's configuration
and journal. No checkout or build is needed on the host. The dashboard is
read-only. Ctrl+C stops it and leaves the last view on screen to copy.

The host is a site value. Pass it, or set it once in the environment or in
site/pool.env (untracked; copy deploy/examples/pool.env.example):
  RAMPIRE_TOP_HOST  SSH destination of the KVM host
.EXAMPLE
.\Watch-Pool.ps1
.EXAMPLE
.\Watch-Pool.ps1 -HostName kvm-host -NewWindow
#>
[CmdletBinding()]
param(
    [string] $HostName,
    [ValidateRange(0.1, 3600)]
    [double] $IntervalSeconds = 1,
    [ValidateRange(1, 30)]
    [int] $Events = 8,
    # Open the dashboard in a separate, suitably sized console window.
    [switch] $NewWindow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# An explicit parameter wins, then the environment, then site/pool.env.
if (-not $HostName) { $HostName = [Environment]::GetEnvironmentVariable('RAMPIRE_TOP_HOST') }
$siteFile = Join-Path $PSScriptRoot '..\..\site\pool.env'
if (-not $HostName -and (Test-Path -LiteralPath $siteFile)) {
    foreach ($line in Get-Content -LiteralPath $siteFile) {
        if ($line -match '^\s*RAMPIRE_TOP_HOST\s*=\s*(.*?)\s*$') { $HostName = $Matches[1] }
    }
}
if (-not $HostName) {
    throw 'Name the KVM host: -HostName, RAMPIRE_TOP_HOST, or site/pool.env ' +
        '(see deploy/examples/pool.env.example).'
}
if ($HostName -match '[\s''"`$\\;&|<>]') {
    throw "Refusing a host name with spaces or shell characters: $HostName"
}
$interval = $IntervalSeconds.ToString([Globalization.CultureInfo]::InvariantCulture)

if ($NewWindow) {
    $arguments = @(
        '-NoExit', '-ExecutionPolicy', 'Bypass', '-Command',
        ("mode con cols=140 lines=52; & '{0}' -HostName '{1}' -IntervalSeconds {2} -Events {3}" -f
            $PSCommandPath, $HostName, $interval, $Events)
    )
    Start-Process powershell -ArgumentList $arguments
    return
}

& ssh -t $HostName "sudo rampire top --interval-seconds $interval --events $Events"
exit $LASTEXITCODE
