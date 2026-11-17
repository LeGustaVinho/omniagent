[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Application,
    [Parameter(Mandatory)][string]$Title,
    [Parameter(Mandatory)][string]$Directory
)

$ErrorActionPreference = 'Stop'
$applicationPath = (Resolve-Path -LiteralPath $Application).Path
$sandbox = Split-Path -Leaf (Split-Path -Parent $applicationPath)
if ($sandbox -cnotmatch '^OmniAgent NSIS Test [a-f0-9]{10}$' -or
    $applicationPath -ine (Join-Path (Join-Path $env:LOCALAPPDATA $sandbox) 'omniagent.exe')) {
    throw 'Refusing to operate a dialog outside the installed test product'
}
$folder = (Resolve-Path -LiteralPath $Directory).Path
if (-not (Test-Path -LiteralPath $folder -PathType Container)) { throw 'The picker fixture requires an existing directory' }
$processes = @(Get-CimInstance Win32_Process -Filter "Name='omniagent.exe'" | Where-Object { $_.ExecutablePath -ieq $applicationPath })
if ($processes.Count -ne 1) { throw 'Cannot identify exactly one installed test process' }

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$automation = [System.Windows.Automation.AutomationElement]
$scope = [System.Windows.Automation.TreeScope]
$condition = [System.Windows.Automation.PropertyCondition]
$ownProcess = $condition::new($automation::ProcessIdProperty, [int]$processes[0].ProcessId)
$dialogConditions = [System.Windows.Automation.AndCondition]::new(
    $ownProcess,
    $condition::new($automation::NameProperty, $Title),
    $condition::new($automation::ClassNameProperty, '#32770')
)
$deadline = [Diagnostics.Stopwatch]::StartNew()
do {
    $dialog = $automation::RootElement.FindFirst($scope::Children, $dialogConditions)
    if (-not $dialog) {
        # Tauri parents the chooser to its window, so UI Automation can expose it as a descendant.
        foreach ($window in $automation::RootElement.FindAll($scope::Children, $ownProcess)) {
            $dialog = $window.FindFirst($scope::Descendants, $dialogConditions)
            if ($dialog) { break }
        }
    }
    if (-not $dialog) { Start-Sleep -Milliseconds 120 }
} until ($dialog -or $deadline.Elapsed.TotalSeconds -ge 20)
if (-not $dialog) { throw "The test process did not open the requested folder dialog: $Title" }

# The common folder dialog can omit its Win32 edit and button from the UI Automation tree.
# Address their standard dialog control IDs within the already process- and title-scoped HWND.
Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class InstalledFixtureFolderDialog {
    const int FolderEdit = 1152;
    const int DefaultButton = 1;
    const uint SetText = 0x000c;
    const uint ButtonClick = 0x00f5;
    const uint AbortIfHung = 0x0002;

    [DllImport("user32")] public static extern IntPtr GetDlgItem(IntPtr dialog, int controlId);
    [DllImport("user32")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wparam, string text,
        uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool PostMessage(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam);

    public static bool TryChoose(IntPtr dialog, uint expectedProcess, string folder) {
        uint process;
        GetWindowThreadProcessId(dialog, out process);
        if (process != expectedProcess) throw new InvalidOperationException("Folder dialog process changed");
        var edit = GetDlgItem(dialog, FolderEdit);
        var button = GetDlgItem(dialog, DefaultButton);
        if (edit == IntPtr.Zero || button == IntPtr.Zero) return false;
        UIntPtr changed;
        if (SendMessageTimeout(edit, SetText, UIntPtr.Zero, folder, AbortIfHung, 2000, out changed) == IntPtr.Zero
            || changed == UIntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        if (!PostMessage(button, ButtonClick, UIntPtr.Zero, IntPtr.Zero)) {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        return true;
    }
}
"@
$chooser = [IntPtr]$dialog.Current.NativeWindowHandle
do {
    $selected = [InstalledFixtureFolderDialog]::TryChoose($chooser, [uint32]$processes[0].ProcessId, $folder)
    if (-not $selected) { Start-Sleep -Milliseconds 120 }
} until ($selected -or $deadline.Elapsed.TotalSeconds -ge 22)
if (-not $selected) { throw 'The folder dialog did not expose its native confirmation controls' }
