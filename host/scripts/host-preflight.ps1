[CmdletBinding()]
param()
$ErrorActionPreference = 'Continue'

function Section([string]$Name) { Write-Host "`n=== $Name ===" }
function Tool([string]$Name, [string[]]$Candidates, [string[]]$VersionArgs = @('--version')) {
    $found = $null
    foreach ($c in $Candidates) {
        if (-not $c) { continue }
        if (Test-Path $c) { $found = (Resolve-Path $c).Path; break }
        $cmd = Get-Command $c -ErrorAction SilentlyContinue
        if ($cmd) { $found = $cmd.Source; break }
    }
    if (-not $found) { Write-Host ("MISS {0}" -f $Name); return }
    $v = ''
    try { $v = (& $found @VersionArgs 2>&1 | Select-Object -First 2) -join ' ' } catch {}
    Write-Host ("PASS {0}: {1}" -f $Name, $found)
    if ($v) { Write-Host ("     {0}" -f $v.Trim()) }
}

Write-Host 'LLB COMMAND RADIO — DEDICATED HOST PREFLIGHT (READ ONLY)'
Write-Host 'No files, services, firewall rules, Docker state, WSL state, or registry values are changed.'

Section 'SYSTEM'
try {
    $os = Get-CimInstance Win32_OperatingSystem
    $cs = Get-CimInstance Win32_ComputerSystem
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    Write-Host ("OS: {0} build {1} / {2}" -f $os.Caption, $os.BuildNumber, $os.OSArchitecture)
    Write-Host ("CPU: {0}" -f $cpu.Name.Trim())
    Write-Host ("RAM: {0:N1} GB" -f ($cs.TotalPhysicalMemory / 1GB))
} catch { Write-Host "INFO system query failed: $($_.Exception.Message)" }
foreach ($drive in 'C','D') {
    try {
        $d = Get-PSDrive $drive -ErrorAction Stop
        Write-Host ("{0}: free {1:N1} GB / used {2:N1} GB" -f $drive, ($d.Free/1GB), ($d.Used/1GB))
    } catch {}
}

Section 'REQUIRED / OPTIONAL TOOLS'
Tool 'Docker' @('docker.exe','C:\Program Files\Docker\Docker\resources\bin\docker.exe') @('version','--format','{{.Client.Version}}')
try {
    $dc = & docker.exe compose version 2>&1
    if ($LASTEXITCODE -eq 0) { Write-Host "PASS Docker Compose: $dc" } else { Write-Host 'MISS Docker Compose plugin' }
} catch { Write-Host 'MISS Docker Compose plugin' }
Tool 'Git (optional for P2 light path)' @('git.exe','D:\Tools\Git\cmd\git.exe')
Tool 'Go (optional; container build carries its own Go)' @('go.exe','D:\Tools\Go\bin\go.exe')
Tool 'cloudflared (needed only for public HTTPS)' @('cloudflared.exe','D:\Tools\cloudflared\cloudflared.exe')

Section 'WSL / VIRTUALIZATION'
try {
    $status = & wsl.exe --status 2>&1
    if ($LASTEXITCODE -eq 0) { Write-Host 'PASS WSL available'; $status | ForEach-Object { Write-Host "     $_" } }
    else { Write-Host 'INFO WSL not ready' }
} catch { Write-Host 'INFO WSL not found' }
try {
    $distros = & wsl.exe -l -v 2>&1
    if ($LASTEXITCODE -eq 0) { $distros | ForEach-Object { Write-Host "     $_" } }
} catch {}

Section 'DOCKER DAEMON'
try {
    $info = & docker.exe info --format 'OS={{.OperatingSystem}} OSType={{.OSType}} CPUs={{.NCPU}} Memory={{.MemTotal}}' 2>&1
    if ($LASTEXITCODE -eq 0) { Write-Host "PASS daemon reachable: $info" }
    else { Write-Host "INFO Docker CLI exists but daemon is unavailable: $info" }
} catch { Write-Host 'INFO Docker daemon unavailable' }

Section 'PORT / NETWORK'
try {
    $listeners = Get-NetTCPConnection -LocalPort 17777 -State Listen -ErrorAction Stop
    if ($listeners) { Write-Host 'INFO TCP 17777 is already listening' }
    else { Write-Host 'PASS TCP 17777 is free' }
} catch { Write-Host 'PASS TCP 17777 appears free' }
foreach ($url in @('https://github.com','https://discord.com','https://developers.cloudflare.com')) {
    try {
        $r = Invoke-WebRequest -Method Head -Uri $url -TimeoutSec 5 -MaximumRedirection 3
        Write-Host ("PASS outbound HTTPS: {0} ({1})" -f $url, $r.StatusCode)
    } catch { Write-Host ("INFO outbound check failed: {0} — {1}" -f $url, $_.Exception.Message) }
}

Section 'CONCLUSION'
Write-Host 'Copy/paste this output back to ChatGPT. It is sufficient to choose the lightest host runtime without guessing.'
