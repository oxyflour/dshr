# Minimal local "computer use" helper for probe verification.
# Actions: shot | winshot | windows | click | keys | focus | raise | lower | list | resize
# winshot 用 PrintWindow(PW_RENDERFULLCONTENT) 直接抓窗口位图，能抓到桌面截图抓不到的
# GPU 合成窗口；进程不是 DPI-aware 时用 -Scale 1.75 抓完整物理窗口。
# Uses only built-in Windows APIs (System.Drawing + user32), no external tools.
param(
  [Parameter(Mandatory = $true)][ValidateSet('shot', 'winshot', 'windows', 'click', 'keys', 'focus', 'raise', 'lower', 'list', 'resize')][string]$Action,
  [int]$Width = 1000,
  [int]$Height = 700,
  [string]$Path = 'C:\Projects\dshr\.tmp\shot.png',
  [int]$X = 0,
  [int]$Y = 0,
  [string]$Text = '',
  [int]$ProcessId = 0,
  [long]$Hwnd = 0,
  [double]$Scale = 1.0,
  [int]$DelayMs = 40
)

Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class NativeInput {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, System.Text.StringBuilder lpClassName, int nMaxCount);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

    public static List<IntPtr> WindowsOf(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p;
            GetWindowThreadProcessId(h, out p);
            if (p == pid && IsWindowVisible(h)) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    public static List<IntPtr> AllWindowsOf(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p;
            GetWindowThreadProcessId(h, out p);
            if (p == pid) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    public static IntPtr LargestWindowOf(uint pid) {
        IntPtr best = IntPtr.Zero;
        long bestArea = 0;
        foreach (var h in WindowsOf(pid)) {
            RECT r;
            if (!GetWindowRect(h, out r)) continue;
            long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (area > bestArea) { bestArea = area; best = h; }
        }
        if (best != IntPtr.Zero) return best;
        // Some GPU-composited windows report IsWindowVisible=false; retry without that filter.
        foreach (var h in AllWindowsOf(pid)) {
            RECT r;
            if (!GetWindowRect(h, out r)) continue;
            long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (area > bestArea) { bestArea = area; best = h; }
        }
        return best;
    }
}
"@

switch ($Action) {
  'shot' {
    Add-Type -AssemblyName System.Windows.Forms
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
    $g.Dispose()
    $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    "shot saved: $Path ($($b.Width)x$($b.Height))"
  }
  'focus' {
    $proc = Get-Process -Id $ProcessId -ErrorAction Stop
    $h = $proc.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { throw "process $ProcessId has no main window" }
    [NativeInput]::ShowWindow($h, 9) | Out-Null   # SW_RESTORE
    [NativeInput]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Milliseconds 300
    $r = New-Object NativeInput+RECT
    [NativeInput]::GetWindowRect($h, [ref]$r) | Out-Null
    [pscustomobject]@{ Left = $r.Left; Top = $r.Top; Right = $r.Right; Bottom = $r.Bottom; Width = $r.Right - $r.Left; Height = $r.Bottom - $r.Top } | ConvertTo-Json -Compress
  }
  'list' {
    foreach ($h in [NativeInput]::WindowsOf([uint32]$ProcessId)) {
      $r = New-Object NativeInput+RECT
      [NativeInput]::GetWindowRect($h, [ref]$r) | Out-Null
      $cls = New-Object System.Text.StringBuilder 256
      [NativeInput]::GetClassName($h, $cls, 256) | Out-Null
      $txt = New-Object System.Text.StringBuilder 256
      [NativeInput]::GetWindowText($h, $txt, 256) | Out-Null
      [pscustomobject]@{ Hwnd = $h.ToString(); Class = $cls.ToString(); Title = $txt.ToString(); Left = $r.Left; Top = $r.Top; Width = $r.Right - $r.Left; Height = $r.Bottom - $r.Top } | ConvertTo-Json -Compress
    }
  }
  'resize' {
    $h = [NativeInput]::LargestWindowOf([uint32]$ProcessId)
    if ($h -eq [IntPtr]::Zero) { throw "process $ProcessId has no visible window" }
    [NativeInput]::SetWindowPos($h, [IntPtr](-1), $X, $Y, $Width, $Height, 0x0040) | Out-Null
    [NativeInput]::SetForegroundWindow($h) | Out-Null
    "resized hwnd=$h to $Width x $Height at $X,$Y"
  }
  'raise' {
    $h = [NativeInput]::LargestWindowOf([uint32]$ProcessId)
    if ($h -eq [IntPtr]::Zero) { throw "process $ProcessId has no visible window" }
    # HWND_TOPMOST = -1, SWP_NOSIZE|SWP_NOMOVE|SWP_SHOWWINDOW
    [NativeInput]::SetWindowPos($h, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0040) | Out-Null
    [NativeInput]::SetForegroundWindow($h) | Out-Null
    $r = New-Object NativeInput+RECT
    [NativeInput]::GetWindowRect($h, [ref]$r) | Out-Null
    [pscustomobject]@{ Hwnd = $h.ToString(); Left = $r.Left; Top = $r.Top; Width = $r.Right - $r.Left; Height = $r.Bottom - $r.Top } | ConvertTo-Json -Compress
  }
  'lower' {
    $h = [NativeInput]::LargestWindowOf([uint32]$ProcessId)
    if ($h -eq [IntPtr]::Zero) { throw "process $ProcessId has no visible window" }
    # HWND_NOTOPMOST = -2, SWP_NOSIZE|SWP_NOMOVE
    [NativeInput]::SetWindowPos($h, [IntPtr](-2), 0, 0, 0, 0, 0x0001 -bor 0x0002) | Out-Null
    "lowered pid=$ProcessId hwnd=$h"
  }
  'windows' {
    $all = [NativeInput]::AllWindowsOf([uint32]$ProcessId)
    foreach ($h in $all) {
      $r = New-Object NativeInput+RECT
      $ok = [NativeInput]::GetWindowRect($h, [ref]$r)
      $cls = New-Object System.Text.StringBuilder 256
      [NativeInput]::GetClassName($h, $cls, 256) | Out-Null
      [pscustomobject]@{ Hwnd = $h.ToString(); Class = $cls.ToString(); RectOk = $ok; L = $r.Left; T = $r.Top; W = ($r.Right - $r.Left); H = ($r.Bottom - $r.Top) } | ConvertTo-Json -Compress
    }
  }
  'winshot' {
    # 直接抓窗口位图（PrintWindow + PW_RENDERFULLCONTENT）：
    # 桌面截图抓不到 GPU 合成窗口时，这是下一个可试的手段（成不成取决于后端）。
    $h = [IntPtr]::Zero
    if ($Hwnd -ne 0) { $h = [IntPtr]$Hwnd }
    if ($h -eq [IntPtr]::Zero) {
      $proc = Get-Process -Id $ProcessId -ErrorAction Stop
      $h = $proc.MainWindowHandle
    }
    if ($h -eq [IntPtr]::Zero) { $h = [NativeInput]::LargestWindowOf([uint32]$ProcessId) }
    if ($h -eq [IntPtr]::Zero) { throw "process $ProcessId has no window handle" }
    $r = New-Object NativeInput+RECT
    [NativeInput]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left
    $hh = $r.Bottom - $r.Top
    # Non-DPI-aware callers see a virtualized window rect; pass -Scale to capture the physical window.
    if ($Scale -ne 1.0) {
      $w = [int]($w * $Scale)
      $hh = [int]($hh * $Scale)
    }
    $bmp = New-Object System.Drawing.Bitmap($w, $hh)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    $ok = [NativeInput]::PrintWindow($h, $hdc, 2)   # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    "winshot saved: $Path ($w x $hh) printwindow=$ok"
  }
  'click' {
    [NativeInput]::SetCursorPos($X, $Y) | Out-Null
    Start-Sleep -Milliseconds 120
    [NativeInput]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)  # LEFTDOWN
    Start-Sleep -Milliseconds 60
    [NativeInput]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)  # LEFTUP
    Start-Sleep -Milliseconds 200
    "clicked: $X,$Y"
  }
  'keys' {
    foreach ($ch in $Text.ToCharArray()) {
      # GPUI 按键分发看扫描码：Enter / Space 必须带真实 scan code，否则被忽略。
      if ($ch -eq ' ') { $vk = 0x20; $scan = 0x39 } elseif ($ch -eq "`n") { $vk = 0x0D; $scan = 0x1C } else { $vk = [int][char]::ToUpperInvariant($ch); $scan = 0 }
      [NativeInput]::keybd_event([byte]$vk, [byte]$scan, 0, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds $DelayMs
      [NativeInput]::keybd_event([byte]$vk, [byte]$scan, 2, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds $DelayMs
    }
    "keys sent: $Text"
  }
}
