param(
    [Parameter(Mandatory)]
    [string] $Destination
)

$ErrorActionPreference = 'Stop'
$Destination = (New-Item -ItemType Directory -Path $Destination -Force).FullName

function Get-WebView2Version {
    $client = 'Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $keys = @(
        "HKLM:\SOFTWARE\WOW6432Node\$client",
        "HKLM:\SOFTWARE\$client",
        "HKCU:\SOFTWARE\WOW6432Node\$client",
        "HKCU:\SOFTWARE\$client"
    )
    foreach ($key in $keys) {
        $value = Get-ItemPropertyValue -LiteralPath $key -Name pv -ErrorAction SilentlyContinue
        if ($value -match '^\d+\.\d+\.\d+\.\d+$' -and $value -ne '0.0.0.0') {
            return [version] $value
        }
    }
    throw 'WebView2 Evergreen is unavailable.'
}

$webView = Get-WebView2Version
$edgeDriver = $env:OMNIAGENT_EDGE_DRIVER
if (!$edgeDriver) {
    $zip = Join-Path $Destination "edgedriver-$webView.zip"
    $download = 'https://msedgedriver.microsoft.com'
    try {
        Invoke-WebRequest -Uri "$download/$webView/edgedriver_win64.zip" -OutFile $zip -UseBasicParsing
    } catch {
        if ([int] $_.Exception.Response.StatusCode -ne 404) { throw }
        $release = Invoke-WebRequest -Uri "$download/LATEST_RELEASE_$($webView.Major)_WINDOWS" -UseBasicParsing
        $versionText = if ($release.Content -is [byte[]]) {
            [Text.Encoding]::UTF8.GetString($release.Content)
        } else {
            $release.Content
        }
        $matchingRelease = [version] ($versionText -replace '^[^\d]+|\s+$', '')
        if ($matchingRelease.ToString(3) -ne $webView.ToString(3)) {
            throw "Microsoft did not offer an EdgeDriver for WebView2 build $($webView.ToString(3))."
        }
        Invoke-WebRequest -Uri "$download/$matchingRelease/edgedriver_win64.zip" -OutFile $zip -UseBasicParsing
    }
    $edgeDirectory = Join-Path $Destination "edge-$webView"
    Expand-Archive -LiteralPath $zip -DestinationPath $edgeDirectory -Force
    $edgeDriver = Join-Path $edgeDirectory 'msedgedriver.exe'
}

$edgeDriver = (Get-Item -LiteralPath $edgeDriver).FullName
$edgeVersionOutput = & $edgeDriver --version
if ($LASTEXITCODE -ne 0 -or $edgeVersionOutput -notmatch '\b(\d+\.\d+\.\d+\.\d+)\b') {
    throw 'EdgeDriver did not report a version.'
}
$edgeVersion = [version] $Matches[1]
if ($edgeVersion.ToString(3) -ne $webView.ToString(3)) {
    throw "EdgeDriver $edgeVersion does not match WebView2 $webView."
}

$tauriDriver = $env:OMNIAGENT_TAURI_DRIVER
if (!$tauriDriver) {
    $tauriDriver = Join-Path $Destination 'bin\tauri-driver.exe'
    if (!(Test-Path -LiteralPath $tauriDriver -PathType Leaf)) {
        cargo install tauri-driver --version 2.1.0 --locked --root $Destination | Out-Host
        if ($LASTEXITCODE -ne 0) { throw 'Could not install tauri-driver.' }
    }
}

[pscustomobject] @{
    TauriDriver = (Get-Item -LiteralPath $tauriDriver).FullName
    EdgeDriver = $edgeDriver
    EdgeDriverVersion = $edgeVersion.ToString()
    WebView2Version = $webView.ToString()
}
