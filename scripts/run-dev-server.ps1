[CmdletBinding()]
param(
    [string]$Addr = '127.0.0.1:17777',
    [string]$Guild = 'dev-guild',
    [string]$User = 'dev-user'
)
$ErrorActionPreference = 'Stop'
$Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'bin\radio-dev-server.exe'
& $Exe -addr $Addr -guild $Guild -user $User
