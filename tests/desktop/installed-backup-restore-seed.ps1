[CmdletBinding()]
param([Parameter(Mandatory)][string]$Case, [switch]$BuildOnly)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $Case).Path
$match = [regex]::Match((Split-Path -Leaf $root), '^omniagent-phase09-nsis-([a-f0-9]{10})$')
if (-not $match.Success) { throw 'The fixture must use an NSIS test case directory' }
$metadata = Get-Content -LiteralPath (Join-Path $root 'case.json') -Raw | ConvertFrom-Json
$tag = $match.Groups[1].Value
if ($metadata.identifier -cne "io.github.legustavinho.omniagent.nsis-test-$tag" -or
    $metadata.product -cne "OmniAgent NSIS Test $tag" -or $metadata.sourceCommit -cnotmatch '^[a-f0-9]{40}$') {
    throw 'The NSIS case identity or source revision is invalid'
}

$source = Join-Path $root 'backup-restore-seed-source'
$module = 'src-tauri/src/workflows/git_integration/installed_backup_restore.rs'
if (-not (Test-Path -LiteralPath $source)) {
    $repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
    $archive = Join-Path $root 'backup-restore-seed-source.tar'
    git -C $repo archive --format=tar --output=$archive $metadata.sourceCommit
    if ($LASTEXITCODE) { throw 'Cannot archive the installed source revision' }
    New-Item -ItemType Directory -Path $source | Out-Null
    tar -xf $archive -C $source
    if ($LASTEXITCODE) { throw 'Cannot extract the fixture source' }
    Add-Content -LiteralPath (Join-Path $source 'src-tauri/src/workflows/git_integration.rs') -Value "`nmod installed_backup_restore;"
} elseif (-not (Test-Path -LiteralPath (Join-Path $source $module))) {
    throw 'Previous fixture compilation did not finish preparing this case'
}
$fixture = Join-Path $PSScriptRoot 'installed-backup-restore-fixture/seed.rs'
$binary = Join-Path $root 'backup-restore-seed.exe'
$stamp = Join-Path $root 'backup-restore-seed-build.txt'
$buildRevision = "$($metadata.sourceCommit):$((Get-FileHash -LiteralPath $fixture).Hash)"

$previousCase = $env:OMNIAGENT_RESTORE_TEST_CASE
Push-Location -LiteralPath (Join-Path $source 'src-tauri')
try {
    $env:OMNIAGENT_RESTORE_TEST_CASE = $root
    if (-not (Test-Path -LiteralPath $binary) -or -not (Test-Path -LiteralPath $stamp) -or
        (Get-Content -LiteralPath $stamp -Raw).Trim() -cne $buildRevision) {
        Copy-Item -LiteralPath $fixture -Destination (Join-Path $source $module) -Force
        $artifacts = cargo test --locked --lib --no-run --message-format=json-render-diagnostics seed_installed_backup_restore |
            ForEach-Object { if ($_ -cmatch '"executable":"') { $_ | ConvertFrom-Json } }
        if ($LASTEXITCODE) { throw 'The installed backup-restore seed did not compile' }
        $compiled = @($artifacts | Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'omniagent' -and $_.profile.test -and $_.executable })
        if ($compiled.Count -ne 1) { throw 'Cargo did not identify the installed restore fixture executable' }
        Copy-Item -LiteralPath $compiled[0].executable -Destination $binary -Force
        Set-Content -LiteralPath $stamp -Value $buildRevision -Encoding ascii
    }
    if (-not $BuildOnly) {
        & $binary --ignored --exact workflows::git_integration::installed_backup_restore::seed_installed_backup_restore
        if ($LASTEXITCODE) { throw 'The installed backup-restore seed failed' }
        if (-not (Test-Path -LiteralPath (Join-Path $root 'backup-restore-seed.json'))) {
            throw 'The installed backup-restore seed did not write a receipt'
        }
    }
} finally {
    Pop-Location
    $env:OMNIAGENT_RESTORE_TEST_CASE = $previousCase
}
