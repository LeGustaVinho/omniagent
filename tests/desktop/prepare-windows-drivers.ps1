param(
    [Parameter(Mandatory)]
    [string] $Destination,
    [switch] $UseFixedWebView2
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

function Get-FixedWebView2 {
    # This signed Microsoft Update Catalog package stays in the CI fixture folder.
    $version = [version] '149.0.4022.98'
    $source = 'https://catalog.s.download.windowsupdate.com/d/msdownload/update/software/updt/2026/06/microsoftedgestandaloneinstallerx64_d8a24888cdb1f99c8aa0715dc12918e2e2cbf9fc.exe'
    $expected = '36668652F8453F3C80D0D8890CA200FF1E05FE5E934CD89A29DF14025E2E8036'
    $root = (New-Item -ItemType Directory -Path (Join-Path $Destination "webview2-$version") -Force).FullName
    $installer = Join-Path $root 'runtime-package.exe'
    Invoke-WebRequest -Uri $source -OutFile $installer -UseBasicParsing
    if ((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash -ne $expected) {
        throw 'The Microsoft WebView2 fixture package digest changed.'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $installer
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Microsoft Corporation') {
        throw 'The WebView2 fixture package signature is invalid.'
    }
    $extractor = (Get-Command 7z.exe -ErrorAction SilentlyContinue).Source
    if (!$extractor) { $extractor = Join-Path $env:ProgramFiles '7-Zip/7z.exe' }
    & $extractor e -y "-o$root" $installer 'MicrosoftEdge_X64_*' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot open the WebView2 fixture package.' }
    $inner = @(Get-ChildItem -LiteralPath $root -File -Filter 'MicrosoftEdge_X64_*')
    if ($inner.Count -ne 1) { throw 'Cannot find the WebView2 fixture payload.' }
    & $extractor e -y "-o$root" $inner[0].FullName 'MSEDGE.7z' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot open the WebView2 fixture payload.' }
    & $extractor x -y "-o$root" (Join-Path $root 'MSEDGE.7z') | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract the WebView2 fixture runtime.' }
    $folder = Join-Path $root "Chrome-bin/$version"
    $runtime = Get-Item -LiteralPath (Join-Path $folder 'msedgewebview2.exe')
    if ($runtime.VersionInfo.ProductName -ne 'Microsoft Edge WebView2' -or
        [version]$runtime.VersionInfo.ProductVersion -ne $version) {
        throw 'The extracted WebView2 fixture is a different runtime.'
    }
    [pscustomobject] @{ Version = $version; Folder = $folder }
}

$fixed = if ($UseFixedWebView2) { Get-FixedWebView2 }
$webView = if ($fixed) { $fixed.Version } else { Get-WebView2Version }
$edgeDriver = $env:OMNIAGENT_EDGE_DRIVER
if (!$edgeDriver) {
    $zip = Join-Path $Destination "edgedriver-$webView.zip"
    $download = 'https://msedgedriver.microsoft.com'
    try {
        Invoke-WebRequest -Uri "$download/$webView/edgedriver_win64.zip" -OutFile $zip -UseBasicParsing
    } catch {
        if ($fixed -or [int] $_.Exception.Response.StatusCode -ne 404) { throw }
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
    if ($fixed -and (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash -ne '31D432396713390AC14852832DCCCC1C456B38926E1C0E04D9B54A426D4CCF03') {
        throw 'The matching EdgeDriver fixture package digest changed.'
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
    WebView2Folder = $fixed.Folder
}
