param(
    [ValidateSet('hold', 'probe')][string]$Mode,
    [string]$Case,
    [string]$Executable,
    [string]$Journal
)
$ErrorActionPreference = 'Stop'
if ((Split-Path $Case -Parent).TrimEnd('\') -ne $env:TEMP.TrimEnd('\') -or
    (Split-Path $Case -Leaf) -notmatch '^omniagent-signed-update-([a-f0-9]{10})$') {
    throw 'Unexpected disposable queue fixture path'
}
$tag = $Matches[1]
if ($Executable -ne (Join-Path $env:LOCALAPPDATA "OmniAgent Signed Update Test $tag/omniagent.exe") -or
    $Journal -ne (Join-Path $env:LOCALAPPDATA "io.github.legustavinho.omniagent.nsis-update-test-$tag/fixture-queue-journal.json")) {
    throw 'Unexpected disposable executable or journal path'
}
$null = [Console]::In.ReadToEnd()
if ($Mode -eq 'hold') {
    [IO.File]::WriteAllText((Join-Path $Case 'queue-hold-started'), 'active')
    $limit = [DateTime]::UtcNow.AddSeconds(55)
    while (-not (Test-Path -LiteralPath (Join-Path $Case 'queue-release-hold') -PathType Leaf)) {
        if ([DateTime]::UtcNow -gt $limit) { throw 'The test did not release the active Command' }
        Start-Sleep -Milliseconds 100
    }
    [Console]::Write('{"held":true}')
} else {
    if (Test-Path -LiteralPath $Journal) { throw 'The verification probe already ran' }
    & $Executable '--read-signed-fixture-journal' $Journal | Out-Null
    $entry = Get-Content -LiteralPath $Journal -Raw | ConvertFrom-Json
    if ($entry.outcome.status -ne 'verified' -or $entry.update.version -ne '0.1.1') {
        throw 'The queued Command ran before the signed upgrade was verified'
    }
    $marker = @{ activationId = $entry.id; version = $entry.update.version; sha256 = $entry.update.sha256 } |
        ConvertTo-Json -Compress
    [IO.File]::WriteAllText((Join-Path $Case 'queue-ran.json'), $marker)
    [Console]::Write($marker)
}
