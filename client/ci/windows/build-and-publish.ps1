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
#   FLUENTGUI_REF    fluent-gpui commit/ref (default: the pinned 0.2.5 commit)
#   WORKROOT         build root (default: $env:TEMP\mydevenv2-client-ci)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }

if (-not $env:GIT_AUTH_TOKEN) { throw 'GIT_AUTH_TOKEN is required' }
$TAG = $env:CI_COMMIT_TAG
$FLUENTGUI_REF = if ($env:FLUENTGUI_REF) { $env:FLUENTGUI_REF } else { 'f601e54b4e58e416bc7495a75468b82af9a10545' }
$WORKROOT = if ($env:WORKROOT) { $env:WORKROOT } else { Join-Path $env:TEMP 'mydevenv2-client-ci' }
$VERSION = if ($TAG) { $TAG -replace '^client-v','' -replace '^v','' } else { '0.0.0' }
if (-not $VERSION) { $VERSION = '0.0.0' }

$base   = 'repo.indexarr.net/indexarr'
$mdeUrl = "https://git:$($env:GIT_AUTH_TOKEN)@$base/MyDevEnv2.git"
$fgUrl  = "https://git:$($env:GIT_AUTH_TOKEN)@$base/fluent-gpui.git"

Step "Workspace: $WORKROOT (tag=$TAG version=$VERSION)"
New-Item -ItemType Directory -Force -Path $WORKROOT | Out-Null
Set-Location $WORKROOT

# ── Checkout MyDevEnv2 (the client lives in client/) ─────────────────────────
Step 'Checkout MyDevEnv2'
if (-not (Test-Path 'MyDevEnv2/.git')) {
    git clone --no-checkout $mdeUrl MyDevEnv2
}
Push-Location MyDevEnv2
git fetch --no-tags --depth 1 origin (if ($TAG) { "refs/tags/$TAG" } else { 'main' })
git checkout --detach FETCH_HEAD
Pop-Location

# ── Checkout the gpui fork as a sibling so client's ../../FluentGUI resolves ──
Step "Checkout fluent-gpui @ $FLUENTGUI_REF"
if (-not (Test-Path 'FluentGUI/.git')) {
    git clone --no-checkout $fgUrl FluentGUI
}
Push-Location FluentGUI
git fetch --depth 1 origin $FLUENTGUI_REF
git checkout --detach FETCH_HEAD
Pop-Location

# ── Build (native MSVC release: fxc precompiles shaders → runnable binary) ────
Step 'cargo build --release (x86_64-pc-windows-msvc)'
Set-Location (Join-Path $WORKROOT 'MyDevEnv2/client')
$target = 'x86_64-pc-windows-msvc'
rustup target add $target 2>$null | Out-Null
cargo build --release --target $target
$exe = Join-Path (Get-Location) "target/$target/release/mydevenv2-client.exe"
if (-not (Test-Path $exe)) { throw "build did not produce $exe" }
Get-Item $exe | ForEach-Object { Write-Host ("exe: {0} ({1:N1} MB)" -f $_.FullName, ($_.Length/1MB)) }

# ── Installer (NSIS) ─────────────────────────────────────────────────────────
Step 'makensis installer'
$out = Join-Path (Get-Location) "MyDevEnv2-Setup-$VERSION.exe"
makensis "/DVERSION=$VERSION" "/DSRCEXE=$exe" "/DOUTFILE=$out" installer/mydevenv2-client.nsi
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
Remove-Item $sumsFile -ErrorAction SilentlyContinue
foreach ($f in @($setup, $port)) {
    $h = (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower()
    "$h  $f" | Tee-Object -FilePath $sumsFile -Append
}

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

foreach ($f in @($setup, $port, $sumsFile)) {
    if (-not (Test-Path $f)) { continue }
    # curl.exe ships with Windows 10+; multipart upload to the assets endpoint.
    curl.exe -sf -X POST -H "Authorization: token $($env:GIT_AUTH_TOKEN)" `
        -F "attachment=@$f" "$api/repos/$repo/releases/$relId/assets" | Out-Null
    Write-Host "uploaded $f"
}
Step "Done: $TAG published."
