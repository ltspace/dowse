param(
    [string]$AppPath = "$env:LOCALAPPDATA\dowse\dowse-app.exe"
)

# Exercises the real installed Tauri window. Leaves the app visible on success.
# Run after installing the candidate; does not change configuration or index data.
$ErrorActionPreference = 'Stop'
$AppPath = (Resolve-Path -LiteralPath $AppPath).Path
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class DowseReopenProbe {
    public delegate bool Callback(IntPtr hwnd, IntPtr param);
    [DllImport("user32.dll")] static extern bool EnumWindows(Callback cb, IntPtr param);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, IntPtr wp, IntPtr lp, uint flags, uint timeout, out IntPtr result);
    public static IntPtr FindMain(int processId) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, p) => {
            uint id; GetWindowThreadProcessId(h, out id);
            var title = new StringBuilder(256); GetWindowText(h, title, 256);
            if (id == processId && title.ToString() == "dowse") { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static void Send(IntPtr hwnd, uint message) {
        IntPtr result;
        if (SendMessageTimeout(hwnd, message, IntPtr.Zero, IntPtr.Zero, 2, 3000, out result) == IntPtr.Zero)
            throw new Exception("Window message timed out: " + message);
    }
}
'@

function Show-App {
    # A second launch uses the production single-instance show_window path.
    Start-Process -FilePath $AppPath -WorkingDirectory (Split-Path -Parent $AppPath)
}

function Wait-Visibility([IntPtr]$Handle, [bool]$Expected) {
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        if ([DowseReopenProbe]::IsWindowVisible($Handle) -eq $Expected) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Expected native visibility $Expected but got $([DowseReopenProbe]::IsWindowVisible($Handle))"
}

Show-App
$mainWindow = [IntPtr]::Zero
for ($attempt = 0; $attempt -lt 50; $attempt++) {
    $instances = @(Get-Process -Name dowse-app -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $AppPath })
    if ($instances.Count -eq 1) { $mainWindow = [DowseReopenProbe]::FindMain($instances[0].Id) }
    if ($mainWindow -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 100
}
if ($mainWindow -eq [IntPtr]::Zero) { throw 'Could not locate the candidate main window' }
Start-Sleep -Milliseconds 1200
Show-App
Wait-Visibility $mainWindow $true

for ($cycle = 1; $cycle -le 3; $cycle++) {
    # WM_KILLFOCUS / WM_ENTERSIZEMOVE / WM_EXITSIZEMOVE must not hide.
    foreach ($message in @(0x0008, 0x0231, 0x0232)) {
        [DowseReopenProbe]::Send($mainWindow, $message)
        Wait-Visibility $mainWindow $true
    }
    # Alt+Tab exercises real activation changes, without manually editing flags.
    if ([DowseReopenProbe]::GetForegroundWindow() -ne $mainWindow) {
        throw 'Dowse lost foreground focus before deactivation test'
    }
    $keyboard = New-Object -ComObject WScript.Shell
    $keyboard.SendKeys('%{TAB}')
    Wait-Visibility $mainWindow $false
    Show-App
    Wait-Visibility $mainWindow $true
    Write-Host "PASS: cycle $cycle - native gestures, deactivation, framework show"
}

# Exercise the real WebView keyboard path only while Dowse owns foreground focus.
Start-Sleep -Milliseconds 300
if ([DowseReopenProbe]::GetForegroundWindow() -ne $mainWindow) {
    throw 'Dowse lost foreground focus; refusing to send Escape to another app'
}
$keyboard = New-Object -ComObject WScript.Shell
$keyboard.SendKeys('{ESC}')
Wait-Visibility $mainWindow $false
Show-App
Wait-Visibility $mainWindow $true
Write-Host 'PASS: real Escape hides the window, and the next launch shows it again'
