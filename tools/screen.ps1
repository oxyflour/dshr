# Minimal local "computer use" helper for probe verification.
# Actions: shot | click | keys | focus
# Uses only built-in Windows APIs (System.Drawing + user32), no external tools.
param(
  [Parameter(Mandatory = $true)][ValidateSet('shot', 'click', 'keys', 'focus', 'raise', 'lower', 'list', 'resize')][string]$Action,
  [int]$Width = 1000,
  [int]$Height = 700,
  [string]$Path = 'C:\Projects\dshr\.tmp\shot.png',
  [int]$X = 0,
  [int]$Y = 0,
  [string]$Text = '',
  [int]$ProcessId = 0,
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

    public static IntPtr LargestWindowOf(uint pid) {
        IntPtr best = IntPtr.Zero;
        long bestArea = 0;
        foreach (var h in WindowsOf(pid)) {
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
      if ($ch -eq ' ') { $vk = 0x20 }
      elseif ($ch -eq "`n") { $vk = 0x0D }
      else { $vk = [int][char]::ToUpperInvariant($ch) }
      [NativeInput]::keybd_event([byte]$vk, 0, 0, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds $DelayMs
      [NativeInput]::keybd_event([byte]$vk, 0, 2, [UIntPtr]::Zero)
      Start-Sleep -Milliseconds $DelayMs
    }
    "keys sent: $Text"
  }
}
