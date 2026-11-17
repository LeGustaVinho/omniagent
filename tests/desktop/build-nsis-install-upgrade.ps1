[CmdletBinding()]
param([string]$Revision = 'HEAD', [switch]$BuildOnly, [switch]$RequireInstallationIdentity)

# Run with PowerShell 7. The case builds a separate product with its own app profile.
# OMNIAGENT_TAURI_DRIVER and OMNIAGENT_EDGE_DRIVER override driver lookup on PATH.
# -RequireInstallationIdentity requires an unchanged installation.dpapi across both boots.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$commit = git -C $repo rev-parse --verify "$Revision^{commit}"
if ($LASTEXITCODE -or $commit -notmatch '^[0-9a-f]{40}$') { throw 'Cannot resolve the source commit' }
$tag = [guid]::NewGuid().ToString('N').Substring(0, 10)
$scratch = Join-Path ([IO.Path]::GetTempPath()) "omniagent-phase09-nsis-$tag"
$snapshot = Join-Path $scratch 'source'
$installers = Join-Path $scratch 'installers'
$product = "OmniAgent NSIS Test $tag"
$identifier = "io.github.legustavinho.omniagent.nsis-test-$tag"
New-Item -ItemType Directory -Path $scratch | Out-Null
New-Item -ItemType Directory -Path $snapshot | Out-Null
New-Item -ItemType Directory -Path $installers | Out-Null
$archive = Join-Path $scratch 'source.tar'
git -C $repo archive --format=tar --output=$archive $commit
if ($LASTEXITCODE) { throw 'Cannot archive the verified source' }
tar -xf $archive -C $snapshot
if ($LASTEXITCODE) { throw 'Cannot extract the source snapshot' }
$case = Join-Path $scratch 'case.json'
@{
    sourceCommit = $commit
    product = $product
    identifier = $identifier
    requireInstallationIdentity = $RequireInstallationIdentity.IsPresent
} |
    ConvertTo-Json | Set-Content -LiteralPath $case -Encoding utf8NoBOM
Write-Output "NSIS test artifacts are in $scratch"

$previousSourceCommit = $env:OMNIAGENT_SOURCE_COMMIT
Push-Location -LiteralPath $snapshot
try {
    pnpm install --frozen-lockfile
    if ($LASTEXITCODE) { throw 'Snapshot dependency installation failed' }
    $env:OMNIAGENT_SOURCE_COMMIT = $commit
    foreach ($version in @('0.1.0', '0.1.1')) {
        $config = Join-Path $scratch "package-$version.json"
        @{
            productName = $product
            identifier = $identifier
            version = $version
            bundle = @{ createUpdaterArtifacts = $false }
        } | ConvertTo-Json | Set-Content -LiteralPath $config -Encoding utf8NoBOM
        node node_modules/@tauri-apps/cli/tauri.js build --target x86_64-pc-windows-msvc --bundles nsis --ci --no-sign --config $config -- --locked
        if ($LASTEXITCODE) { throw "NSIS build $version failed" }
        $bundle = Join-Path $snapshot "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/${product}_${version}_x64-setup.exe"
        Copy-Item -LiteralPath $bundle -Destination $installers
    }
    if (-not $BuildOnly) {
        $runner = Join-Path $snapshot 'tests/desktop/nsis-install-upgrade.mjs'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'nsis-install-upgrade.mjs') -Destination $runner -Force
        node $runner $case
        if ($LASTEXITCODE) { throw 'NSIS installation and upgrade acceptance failed' }
    }
} finally {
    $env:OMNIAGENT_SOURCE_COMMIT = $previousSourceCommit
    Pop-Location
}
