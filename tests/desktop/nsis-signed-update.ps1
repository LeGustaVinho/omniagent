[CmdletBinding()]
param([Parameter(Mandatory)][string]$Revision, [switch]$BuildOnly, [string]$ReuseTargetCase,
    [switch]$Automatic)

# Windows PowerShell 7, existing pinned pnpm/Cargo caches and WebView2 drivers.
# Builds and signs packages offline. BuildOnly keeps the signed case out of HKCU.
# For automatic acceptance on Windows 10 or 11 x64, use -Revision e469b1c -Automatic.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$inputCommit = git -C $repo rev-parse --verify "$Revision^{commit}"
if ($LASTEXITCODE -or $inputCommit -notmatch '^[a-f0-9]{40}$') { throw 'Cannot pin the source commit' }
$tag = [guid]::NewGuid().ToString('N').Substring(0, 10)
$identifier = "io.github.legustavinho.omniagent.nsis-update-test-$tag"
$product = "OmniAgent Signed Update Test $tag"
$case = Join-Path ([IO.Path]::GetTempPath()) "omniagent-signed-update-$tag"
$snapshot = Join-Path $case 'source'
$fixture = Join-Path $PSScriptRoot 'nsis-signed-update-fixture'
New-Item -ItemType Directory -Path $snapshot | Out-Null
Write-Host "Signed NSIS fixture case $case"

function Assert-Native([string]$Action) {
    if ($LASTEXITCODE) { throw "$Action failed (exit $LASTEXITCODE)" }
}

function Edit-Once([string]$Path, [string]$Match, [string]$Replacement) {
    $contents = [IO.File]::ReadAllText($Path)
    if ([regex]::Matches($contents, [regex]::Escape($Match)).Count -ne 1) {
        throw "The pinned source has an unexpected fixture seam in $Path"
    }
    [IO.File]::WriteAllText($Path, $contents.Replace($Match, $Replacement))
}

$inputTar = Join-Path $case 'input.tar'
git -C $repo archive --format=tar --output=$inputTar $inputCommit
Assert-Native 'Archive of pinned source'
tar.exe -xf $inputTar -C $snapshot
Assert-Native 'Extract pinned source'
Copy-Item -LiteralPath (Join-Path $fixture 'desktop.rs') -Destination (Join-Path $snapshot 'src-tauri/src/signed_update_desktop_fixture.rs')
Copy-Item -LiteralPath (Join-Path $fixture 'offline.rs') -Destination (Join-Path $snapshot 'src-tauri/src/maintenance_update/signed_desktop_fixture.rs')
Copy-Item -LiteralPath (Join-Path $fixture 'seed.rs') -Destination (Join-Path $snapshot 'src-tauri/src/workflows/git_integration/signed_update_fixture.rs')
Copy-Item -LiteralPath (Join-Path $fixture 'queue.rs') -Destination (Join-Path $snapshot 'src-tauri/src/workflows/git_integration/signed_update_fixture_queue.rs')
Copy-Item -LiteralPath (Join-Path $fixture 'queue.ps1') -Destination (Join-Path $case 'queue.ps1')
Copy-Item -LiteralPath (Join-Path $fixture 'build.ps1') -Destination (Join-Path $snapshot 'fixture-build.ps1')
Copy-Item -LiteralPath (Join-Path $fixture 'migrate.sql') -Destination (Join-Path $snapshot 'src-tauri/migrations/fixture-nsis-update.sql')
$hooks = (Get-Content -Raw -LiteralPath (Join-Path $fixture 'installer.nsh')).Replace('@IDENTIFIER@', $identifier)
[IO.File]::WriteAllText((Join-Path $snapshot 'src-tauri/fixture-installer.nsh'), $hooks)
$main = Join-Path $snapshot 'src-tauri/src/main.rs'
Edit-Once $main 'mod startup_update;' "mod startup_update;`nmod signed_update_desktop_fixture;"
Edit-Once $main '    let context = tauri::generate_context!();' @'
    let context = tauri::generate_context!();
    if signed_update_desktop_fixture::read_journal_from_args(&context.config().identifier)
        .expect("signed fixture journal")
    { return; }
'@
Edit-Once $main '                let store = Store::open(installation.layout.database_path())?;' @'
                let store = Store::open(installation.layout.database_path())?;
                signed_update_desktop_fixture::after_sqlite(&app.config().identifier, &root, &store)?;
'@
Edit-Once $main '            workflow_ipc::maintenance::activate_signed_update,' @'
            workflow_ipc::maintenance::activate_signed_update,
            signed_update_desktop_fixture::signed_fixture_migration_state,
'@
$update = Join-Path $snapshot 'src-tauri/src/maintenance_update.rs'
Add-Content -LiteralPath $update -Value "`nmod signed_desktop_fixture;`npub use signed_desktop_fixture::{dirty_signed_fixture_migration, read_signed_fixture_migration};"
if ($Automatic) {
    Copy-Item -LiteralPath (Join-Path $fixture 'automatic_feed.rs') -Destination (Join-Path $snapshot 'src-tauri/src/maintenance_update/signed_desktop_fixture_feed.rs')
    Add-Content -LiteralPath $update -Value "`nmod signed_desktop_fixture_feed;"
    $network = @'
        let Some(advertised) = updater
            .check()
            .await
            .context("Cannot check the configured signed update feed")?
        else {
            return Ok(None);
        };
'@
    $localFeed = @'
        let Some(raw) = signed_desktop_fixture_feed::read_fixture_feed(app)? else {
            return Ok(None);
        };
'@
    Edit-Once $update $network $localFeed
    Edit-Once $update 'updater.restore_update(advertised.raw_json)' 'updater.restore_update(raw)'
} else {
    Copy-Item -LiteralPath (Join-Path $fixture 'desktop_manual.rs') -Destination (Join-Path $snapshot 'src-tauri/src/signed_update_manual_fixture.rs')
    Copy-Item -LiteralPath (Join-Path $fixture 'offline_manual.rs') -Destination (Join-Path $snapshot 'src-tauri/src/maintenance_update/signed_desktop_fixture_manual.rs')
    Add-Content -LiteralPath $update -Value "`nmod signed_desktop_fixture_manual;"
    Edit-Once $main 'mod signed_update_desktop_fixture;' "mod signed_update_desktop_fixture;`nmod signed_update_manual_fixture;"
    Edit-Once $main '            signed_update_desktop_fixture::signed_fixture_migration_state,' @'
            signed_update_desktop_fixture::signed_fixture_migration_state,
            signed_update_manual_fixture::signed_fixture_activate,
'@
}
Add-Content -LiteralPath (Join-Path $snapshot 'src-tauri/src/workflows/git_integration.rs') -Value "`nmod signed_update_fixture;"
$fixtureTar = Join-Path $case 'fixture-source.tar'
tar.exe -cf $fixtureTar -C $snapshot .
Assert-Native 'Archive fixture source without dependencies'

$envNames = @('OMNIAGENT_SIGNED_FIXTURE_ID','OMNIAGENT_SIGNED_TEST_CASE','OMNIAGENT_SIGNED_TEST_STEP',
    'OMNIAGENT_SIGNED_TEST_AUTOMATIC',
    'OMNIAGENT_SOURCE_COMMIT','OMNIAGENT_UPDATE_FEED','OMNIAGENT_UPDATE_PUBLIC_KEY','CARGO_TARGET_DIR','CARGO_NET_OFFLINE',
    'CARGO_PROFILE_RELEASE_OPT_LEVEL','CARGO_PROFILE_RELEASE_INCREMENTAL')
$previous = @{}
foreach ($name in $envNames) { $previous[$name] = [Environment]::GetEnvironmentVariable($name) }

$env:OMNIAGENT_SIGNED_FIXTURE_ID = $identifier
$env:OMNIAGENT_SIGNED_TEST_CASE = $case
$env:OMNIAGENT_SIGNED_TEST_AUTOMATIC = if ($Automatic) { '1' } else { $null }
$env:CARGO_TARGET_DIR = Join-Path $case 'target'
if ($ReuseTargetCase) {
    $cachedCase = (Resolve-Path -LiteralPath $ReuseTargetCase).Path
    if ((Split-Path $cachedCase) -ne (Split-Path $case) -or (Split-Path $cachedCase -Leaf) -notmatch '^omniagent-signed-update-[a-f0-9]{10}$') {
        throw 'The reuse path must name another disposable signed updater case in the same temporary directory'
    }
    $cachedTarget = Get-Item -LiteralPath (Join-Path $cachedCase 'target')
    if (-not $cachedTarget.PSIsContainer -or $cachedTarget.LinkType) { throw 'The cached target must be an ordinary directory' }
    $env:CARGO_TARGET_DIR = $cachedTarget.FullName
}
$env:CARGO_NET_OFFLINE = 'true'
$env:CARGO_PROFILE_RELEASE_OPT_LEVEL = '0'
$env:CARGO_PROFILE_RELEASE_INCREMENTAL = 'true'
$env:OMNIAGENT_UPDATE_FEED = $null
$env:OMNIAGENT_UPDATE_PUBLIC_KEY = $null
$seedTest = 'workflows::git_integration::signed_update_fixture::seed_signed_update_case'

Push-Location -LiteralPath $snapshot
try {
    pnpm install --offline --frozen-lockfile
    Assert-Native 'Install pinned dependencies from cache'
    $env:OMNIAGENT_SIGNED_TEST_STEP = 'plan'
    cargo test --offline --locked --manifest-path src-tauri/Cargo.toml --lib $seedTest -- --ignored --exact --nocapture
    Assert-Native 'Accept real Git fixture changes'
    $plan = Get-Content -LiteralPath (Join-Path $case 'plan.json') -Raw | ConvertFrom-Json
    git init --quiet
    Assert-Native 'Initialize isolated source checkout'
    git config core.autocrlf false
    Assert-Native 'Configure isolated source checkout'
    git fetch --no-tags $plan.repository HEAD
    Assert-Native 'Fetch only the local disposable fixture history'
    $env:OMNIAGENT_UPDATE_FEED = 'https://release.invalid/offline-fixture.json'
    $env:OMNIAGENT_UPDATE_PUBLIC_KEY = (Get-Content -LiteralPath (Join-Path $case 'fixture.pub') -Raw).Trim()
    $builds = @(@{ version = '0.1.0'; commit = $plan.baselineCommit }) + @($plan.candidates)
    foreach ($build in $builds) {
        $version = $build.version
        $source = $build.commit
        $cache = Join-Path $case "artifacts/$source"
        New-Item -ItemType Directory -Path $cache | Out-Null
        git checkout --quiet --force --detach $source
        Assert-Native "Open accepted fixture $version"
        if ((git rev-parse HEAD) -ne $source -or (git --no-optional-locks status --porcelain --untracked-files=normal)) {
            throw "Build $version must use a clean checkout of the accepted source"
        }
        $override = Join-Path $case "package-$version.json"
        $windows = @{ allowDowngrades = $true }
        if ($version -eq '0.1.2') { $windows.nsis = @{ installerHooks = './fixture-installer.nsh' } }
        @{ productName = $product; identifier = $identifier; version = $version;
           plugins = @{ updater = @{ windows = @{ installMode = 'quiet' } } };
           bundle = @{ createUpdaterArtifacts = $false; windows = $windows } } |
           ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $override -Encoding utf8NoBOM
        $env:OMNIAGENT_SOURCE_COMMIT = $source
        node node_modules/@tauri-apps/cli/tauri.js build --target x86_64-pc-windows-msvc --bundles nsis --ci --no-sign --config $override -- --locked --offline
        Assert-Native "Build accepted fixture $version"
        $bundle = Join-Path $env:CARGO_TARGET_DIR "x86_64-pc-windows-msvc/release/bundle/nsis/${product}_${version}_x64-setup.exe"
        $installer = Join-Path $cache 'installer.exe'
        Copy-Item -LiteralPath $bundle -Destination $installer
        @{ source = $source; version = $version; sha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $cache 'built.json') -Encoding utf8NoBOM
    }

    git checkout --quiet --force --detach $plan.baselineCommit
    Assert-Native 'Restore the source of the fixture seeder'
    $env:OMNIAGENT_SOURCE_COMMIT = $previous['OMNIAGENT_SOURCE_COMMIT']
    $env:OMNIAGENT_UPDATE_FEED = $null
    $env:OMNIAGENT_UPDATE_PUBLIC_KEY = $null
    $env:OMNIAGENT_SIGNED_TEST_STEP = 'sign'
    cargo test --offline --locked --manifest-path src-tauri/Cargo.toml --lib $seedTest -- --ignored --exact --nocapture
    Assert-Native 'Sign accepted installer bytes with the ephemeral fixture key'

    $caseFile = Join-Path $case 'case.json'
    @{ inputCommit = $inputCommit; tag = $tag; identifier = $identifier; product = $product; automatic = $Automatic.IsPresent } |
        ConvertTo-Json | Set-Content -LiteralPath $caseFile -Encoding utf8NoBOM
    if (-not $BuildOnly) {
        $runner = Join-Path $snapshot 'tests/desktop/nsis-signed-update.mjs'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'nsis-signed-update.mjs') -Destination $runner
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'nsis-signed-update-automatic.mjs') -Destination (Split-Path $runner)
        node $runner $caseFile
        Assert-Native 'Exercise the disposable installed updater'
    }
} finally {
    Pop-Location
    foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name, $previous[$name]) }
}
