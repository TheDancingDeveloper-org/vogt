# Native Windows build + publish for the MyDevEnv2 desktop client.
#
# Runs on the `arbit-win` Woodpecker agent (platform windows/amd64, local
# backend) — the same box that builds Zed, so it already has the Rust MSVC
# toolchain, fxc.exe (DirectX shader compiler), NSIS (makensis), and git.
#
# Why native: gpui only precompiles its HLSL shaders (via fxc) when built ON
# Windows in release mode. A Linux cross-compile cannot produce a runnable
# binary (release needs fxc; debug compiles shaders from source paths that don't
# exist on the user's machine). Building here with `--release` + MSVC fixes both
# the shader and the Common-Controls manifest concerns at their source.
#
# This script is the version-controlled source of truth. Deploy it to the agent
# (e.g. C:\ci\mydevenv2\build-and-publish.ps1) — the workflow invokes it there.
#
# Inputs (environment):
#   CI_COMMIT_TAG    e.g. client-v0.1.3  (empty => dev build, no publish)
#   GIT_AUTH_TOKEN   Forgejo token (git clone + release API)   [required]
# Optional:
#   WORKROOT         build root (default: $env:TEMP\mydevenv2-client-ci)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Always capture a full transcript to a fixed path — this host's Woodpecker
# returns empty step logs over the API, so this file is how we diagnose.
try { New-Item -ItemType Directory -Force 'C:\woodpecker' | Out-Null } catch {}
try { Start-Transcript -Path 'C:\woodpecker\last-build.log' -Force | Out-Null } catch {}

function Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }

if (-not $env:GIT_AUTH_TOKEN) { throw 'GIT_AUTH_TOKEN is required' }
# Running as a service (LocalSystem) has no interactive credential store; keep
# git from trying to persist credentials (token is supplied inline in the URL).
$env:GCM_CREDENTIAL_STORE = 'none'
$env:GIT_TERMINAL_PROMPT = '0'
$TAG = $env:CI_COMMIT_TAG
$WORKROOT = if ($env:WORKROOT) { $env:WORKROOT } else { Join-Path $env:TEMP 'mydevenv2-client-ci' }
$VERSION = if ($TAG) { $TAG -replace '^client-v','' -replace '^v','' } else { '0.0.0' }
if (-not $VERSION) { $VERSION = '0.0.0' }

$base   = 'repo.indexarr.net/indexarr'
$mdeUrl = "https://git:$($env:GIT_AUTH_TOKEN)@$base/MyDevEnv2.git"

# git writes progress ("Cloning into...") to stderr; with ErrorActionPreference
# 'Stop' PowerShell would treat that as fatal. Route git stderr to stdout and
# rely on $LASTEXITCODE (checked by Run-Git) for real failures.
$env:GIT_REDIRECT_STDERR = '2>&1'
# Ensure the per-user Rust toolchain is on PATH regardless of which account the
# Woodpecker agent runs as.
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

function Run-Git {
    param([Parameter(ValueFromRemainingArguments=$true)][string[]]$Args)
    & git @Args
    if ($LASTEXITCODE -ne 0) { throw "git $($Args -join ' ') failed ($LASTEXITCODE)" }
}

# Run a native exe, merging stderr into the success stream so PowerShell 5.1
# does not treat informational stderr (e.g. rustup/cargo progress) as a
# terminating error under ErrorActionPreference='Stop'. Fails on non-zero exit.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments, [switch]$IgnoreExit)
    # PS 5.1 turns ANY native-command stderr into a terminating error under
    # ErrorActionPreference='Stop' (even with 2>&1). Drop to 'Continue' for the
    # call, capture exit code, then restore and decide based on the exit code.
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Exe @Arguments 2>&1 | ForEach-Object { Write-Host $_ }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $old
    }
    if (-not $IgnoreExit -and $code -ne 0) {
        throw "$Exe $($Arguments -join ' ') failed ($code)"
    }
}

Step "Workspace: $WORKROOT (tag=$TAG version=$VERSION)"
New-Item -ItemType Directory -Force -Path $WORKROOT | Out-Null
Set-Location $WORKROOT

# ── Checkout MyDevEnv2 (the client lives in client/) ─────────────────────────
Step 'Checkout MyDevEnv2'
if (-not (Test-Path 'MyDevEnv2/.git')) {
    Run-Git clone --no-checkout $mdeUrl MyDevEnv2
}
Push-Location MyDevEnv2
$mdeRef = if ($TAG) { "refs/tags/$TAG" } else { 'main' }
Run-Git fetch --no-tags --depth 1 origin $mdeRef
Run-Git checkout --detach --force FETCH_HEAD
Pop-Location

# ── Build (native MSVC release: fxc precompiles shaders → runnable binary) ────
Step 'cargo build --release (x86_64-pc-windows-msvc)'
Set-Location (Join-Path $WORKROOT 'MyDevEnv2/client')
$target = 'x86_64-pc-windows-msvc'
Invoke-Native rustup @('target', 'add', $target) -IgnoreExit
Invoke-Native cargo @('build', '--release', '--target', $target)
$exe = Join-Path (Get-Location) "target/$target/release/mydevenv2-client.exe"
if (-not (Test-Path $exe)) { throw "build did not produce $exe" }
Get-Item $exe | ForEach-Object { Write-Host ("exe: {0} ({1:N1} MB)" -f $_.FullName, ($_.Length/1MB)) }

# ── Installer (NSIS) ─────────────────────────────────────────────────────────
Step 'makensis installer'
$makensis = $null
$gc = Get-Command makensis.exe -ErrorAction SilentlyContinue
if ($gc) { $makensis = $gc.Source }
if (-not $makensis) {
    foreach ($p in @("$env:ProgramFiles\NSIS\makensis.exe", "${env:ProgramFiles(x86)}\NSIS\makensis.exe")) {
        if (Test-Path $p) { $makensis = $p; break }
    }
}
if (-not $makensis) { throw 'makensis.exe not found (install NSIS)' }
$out = Join-Path (Get-Location) "MyDevEnv2-Setup-$VERSION.exe"
Invoke-Native $makensis @("/DVERSION=$VERSION", "/DSRCEXE=$exe", "/DOUTFILE=$out", 'installer/mydevenv2-client.nsi')
if (-not (Test-Path $out)) { throw "makensis did not produce $out" }

if (-not $TAG) {
    Step 'No tag — build only, skipping publish.'
    exit 0
}

# ── Stage release artefacts + checksums ──────────────────────────────────────
Step 'Stage artefacts'
$setup = "MyDevEnv2-Client-$TAG-Setup.exe"
$port  = "MyDevEnv2-Client-$TAG-windows-x86_64.exe"
Copy-Item $out $setup -Force
Copy-Item $exe $port -Force
$sumsFile = "SHA256SUMS-$TAG.txt"
$lines = foreach ($f in @($setup, $port)) {
    "{0}  {1}" -f (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower(), $f
}
Set-Content -Path $sumsFile -Value $lines -Encoding ascii
Write-Host ($lines -join "`n")

# ── Publish to the Forgejo release page ──────────────────────────────────────
Step 'Publish Forgejo release'
$api  = 'https://repo.indexarr.net/api/v1'
$repo = 'indexarr/MyDevEnv2'
$hdr  = @{ Authorization = "token $($env:GIT_AUTH_TOKEN)" }

$relId = $null
try {
    $rel = Invoke-RestMethod -Headers $hdr -Uri "$api/repos/$repo/releases/tags/$TAG" -Method Get
    $relId = $rel.id
} catch { }
if (-not $relId) {
    $body = @{
        tag_name   = $TAG
        name       = "MyDevEnv2 client $TAG"
        body       = "## MyDevEnv2 desktop client $TAG`n`n- **Windows installer**: ``$setup```n- **Windows portable**: ``$port```n`nSHA256 checksums in ``$sumsFile``"
        draft      = $false
        prerelease = $true
    } | ConvertTo-Json
    $rel = Invoke-RestMethod -Headers $hdr -Uri "$api/repos/$repo/releases" -Method Post -ContentType 'application/json' -Body $body
    $relId = $rel.id
}
if (-not $relId) { throw 'failed to resolve Forgejo release id' }

# Existing assets on this release (to make re-runs idempotent — delete a
# same-named asset before re-uploading instead of creating duplicates).
$existing = @{}
try {
    foreach ($a in (Invoke-RestMethod -Headers $hdr -Uri "$api/repos/$repo/releases/$relId/assets" -Method Get)) {
        $existing[$a.name] = $a.id
    }
} catch { }

foreach ($f in @($setup, $port, $sumsFile)) {
    if (-not (Test-Path $f)) { continue }
    if ($existing.ContainsKey($f)) {
        try { Invoke-RestMethod -Headers $hdr -Method Delete -Uri "$api/repos/$repo/releases/$relId/assets/$($existing[$f])" | Out-Null } catch { }
    }
    # curl.exe ships with Windows 10+; multipart upload (PS 5.1 lacks -Form).
    Invoke-Native 'curl.exe' @(
        '-sf', '-X', 'POST',
        '-H', "Authorization: token $($env:GIT_AUTH_TOKEN)",
        '-F', "attachment=@$f",
        "$api/repos/$repo/releases/$relId/assets"
    )
    Write-Host "uploaded $f"
}
Step "Done: $TAG published."
