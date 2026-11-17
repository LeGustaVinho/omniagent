param([string]$Mode, [string]$Case)
$ErrorActionPreference = 'Stop'
$config = Get-Content -Raw src-tauri/tauri.conf.json | ConvertFrom-Json
$version = $config.version
$cargoVersion = Select-String -Path src-tauri/Cargo.toml -Pattern '^version = "(.*)"$' | Select-Object -First 1
if ($version -ne $cargoVersion.Matches[0].Groups[1].Value) { throw 'Fixture versions disagree' }
if ($Mode -eq 'check') { exit 0 }
if ($Mode -ne 'package') { throw 'Unknown fixture build action' }
$source = (git rev-parse HEAD).Trim()
if ($LASTEXITCODE -or $source -notmatch '^[a-f0-9]{40}$') { throw 'Missing verified fixture commit' }
$artifact = Join-Path $Case "artifacts/$source"
$manifest = Get-Content -LiteralPath (Join-Path $artifact 'built.json') -Raw | ConvertFrom-Json
$installer = Join-Path $artifact 'installer.exe'
$algorithm = [System.Security.Cryptography.SHA256]::Create()
$stream = [IO.File]::OpenRead($installer)
try { $digest = [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '') }
finally { $stream.Dispose(); $algorithm.Dispose() }
if ($manifest.source -ne $source -or $manifest.version -ne $version -or
    $digest -ne $manifest.sha256) {
    throw 'The prebuilt artifact differs from the known build of this exact fixture source'
}
Copy-Item -LiteralPath $installer -Destination 'accepted-installer.exe'
[IO.File]::WriteAllText((Join-Path $PWD 'accepted-version.txt'), $version)
