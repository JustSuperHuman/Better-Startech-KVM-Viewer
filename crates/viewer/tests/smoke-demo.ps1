param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$Screenshot,
    [switch]$Narrow,
    [switch]$WheelZoom,
    [switch]$NativePixels,
    [switch]$MiddleDrag,
    [string]$WindowScreenshot
)
$ErrorActionPreference = 'Stop'
# Always --demo: no helper/device. Clicks require a PID/title-verified window
# under the cursor; keyboard/wheel messages stay window-targeted. GPU captures
# use the app's owned target; optional native captures render only this verified
# demo window, never the desktop or another app.
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ViewerSmokeNative {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] private static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [StructLayout(LayoutKind.Sequential)] private struct MouseInput { public int X, Y; public uint Data, Flags, Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Sequential)] private struct Input { public uint Type; public MouseInput Mouse; }
    [DllImport("user32.dll")] private static extern uint SendInput(uint count, Input[] inputs, int size);
    public static bool ClickDemo(IntPtr window) {
        if (!GetCursorPos(out Point point) || WindowFromPoint(point) != window || (GetAsyncKeyState(1) & 0x8000) != 0) return false;
        var inputs = new Input[] { new Input { Mouse = new MouseInput { Flags = 2 } }, new Input { Mouse = new MouseInput { Flags = 4 } } };
        uint sent = SendInput(2, inputs, Marshal.SizeOf<Input>());
        if (sent == 1) SendInput(1, new Input[] { inputs[1] }, Marshal.SizeOf<Input>());
        return sent == 2;
    }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr context, uint flags);
    private delegate bool WindowCallback(IntPtr window, IntPtr state);
    [DllImport("user32.dll")] private static extern bool EnumWindows(WindowCallback callback, IntPtr state);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, System.Text.StringBuilder title, int capacity);
    public static IntPtr FindDemo(uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((window, state) => {
            GetWindowThreadProcessId(window, out uint owner);
            if (owner != pid || !IsWindowVisible(window)) return true;
            var title = new System.Text.StringBuilder(256);
            GetWindowText(window, title, title.Capacity);
            if (title.ToString().Contains("USB Crash Cart") && title.ToString().Contains("DEMO")) { found = window; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = $Executable
$start.UseShellExecute = $false
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$duration = if ($WindowScreenshot) { '8' } else { '5' }
foreach ($argument in @('--demo', '--smoke-test', '--smoke-seconds', $duration, '--screenshot', $Screenshot)) { $start.ArgumentList.Add($argument) }
$process = [System.Diagnostics.Process]::Start($start)
$stdout = $process.StandardOutput.ReadToEndAsync()
$stderr = $process.StandardError.ReadToEndAsync()
$window = [IntPtr]::Zero
$originalCursor = [ViewerSmokeNative+Point]::new()
$restoreCursor = [ViewerSmokeNative]::GetCursorPos([ref]$originalCursor)
$lastCursor = $null
try {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $window = [ViewerSmokeNative]::FindDemo($process.Id)
        if ($window -ne [IntPtr]::Zero) { break }
        if ($process.WaitForExit(50)) { throw 'Demo exited before creating a window.' }
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($window -eq [IntPtr]::Zero) { throw 'Demo window was not created within fifteen seconds.' }
    # Native pointer hit-testing rejects messages when another app occludes the
    # client point. Keep only this short-lived demo above it, without activation.
    [void][ViewerSmokeNative]::SetWindowPos($window, [IntPtr](-1), 0, 0, 0, 0, 0x0013)
    # Synthetic activation supports wheel-only tests; guarded toolbar clicks
    # activate the demo normally. No global keyboard events or helper can run.
    [void][ViewerSmokeNative]::PostMessage($window, 0x0086, [IntPtr]1, [IntPtr]::Zero)
    [void][ViewerSmokeNative]::PostMessage($window, 0x0007, [IntPtr]::Zero, [IntPtr]::Zero)
    # First presentation follows visibility by one queued frame.
    Start-Sleep -Milliseconds 250
    $dpi = [ViewerSmokeNative]::GetDpiForWindow($window) / 96.0
    foreach ($kind in @(0, 1)) {
        if ([ViewerSmokeNative]::SendMessage($window, 0x007f, [IntPtr]$kind, [IntPtr]::Zero) -eq [IntPtr]::Zero) {
            throw "Native window/taskbar icon $kind is missing."
        }
    }
    'Native window and taskbar icons are present.'
    if ($Narrow) {
        $outer = [ViewerSmokeNative+Rect]::new(); $client = [ViewerSmokeNative+Rect]::new()
        [void][ViewerSmokeNative]::GetWindowRect($window, [ref]$outer)
        [void][ViewerSmokeNative]::GetClientRect($window, [ref]$client)
        $width = [int](760 * $dpi) + ($outer.Right - $outer.Left) - $client.Right
        $height = [int](520 * $dpi) + ($outer.Bottom - $outer.Top) - $client.Bottom
        [void][ViewerSmokeNative]::SetWindowPos($window, [IntPtr](-1), 0, 0, $width, $height, 0x0012)
    }
    function Save-DemoWindow {
        if ([ViewerSmokeNative]::FindDemo($process.Id) -ne $window) { throw 'Demo window identity changed before capture.' }
        $bounds = [ViewerSmokeNative+Rect]::new()
        if (-not [ViewerSmokeNative]::GetWindowRect($window, [ref]$bounds)) { throw 'Could not read the demo window bounds.' }
        $width = $bounds.Right - $bounds.Left; $height = $bounds.Bottom - $bounds.Top
        if ($width -le 0 -or $height -le 0 -or [long]$width * $height -gt 16777216) { throw 'Demo window capture exceeds the bounded size.' }
        Add-Type -AssemblyName System.Drawing
        $bitmap = [System.Drawing.Bitmap]::new($width, $height, [System.Drawing.Imaging.PixelFormat]::Format32bppRgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $context = $graphics.GetHdc()
            try {
                if (-not [ViewerSmokeNative]::PrintWindow($window, $context, 2)) { throw 'Could not render the verified demo window.' }
            } finally { $graphics.ReleaseHdc($context) }
            if ([Environment]::OSVersion.Version.Build -ge 22000) {
                # DWM's color attributes are set-only. Check the rendered caption instead.
                $origin = [ViewerSmokeNative+Point]::new()
                if (-not [ViewerSmokeNative]::ClientToScreen($window, [ref]$origin)) { throw 'Could not locate the demo caption.' }
                $client = [ViewerSmokeNative+Rect]::new()
                if (-not [ViewerSmokeNative]::GetClientRect($window, [ref]$client)) { throw 'Could not locate the demo toolbar.' }
                $caption = $bitmap.GetPixel([int]($width / 2), [int](($origin.Y - $bounds.Top) / 2))
                $toolbarX = $origin.X - $bounds.Left + $client.Right - [int](4 * $dpi)
                $toolbar = $bitmap.GetPixel($toolbarX, $origin.Y - $bounds.Top + [int](8 * $dpi))
                if ($caption.R -ne 14 -or $caption.G -ne 16 -or $caption.B -ne 21 -or $caption.ToArgb() -ne $toolbar.ToArgb()) {
                    throw "Rendered caption and toolbar do not match the app palette: caption=$caption, toolbar=$toolbar"
                }
                'Rendered native caption and toolbar share the app background.'
            }
            $output = [System.IO.File]::Open($WindowScreenshot, [System.IO.FileMode]::CreateNew)
            try { $bitmap.Save($output, [System.Drawing.Imaging.ImageFormat]::Png) } finally { $output.Dispose() }
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
        "Native demo window screenshot: $WindowScreenshot"
    }
    # Activate with an inert DEMO-label click before 1:1/+; Windows may eat the
    # first click on an inactive window. Narrow keeps Fit for edge visibility.
    # Move/restore the cursor over this verified demo to prevent WM_MOUSELEAVE.
    # Mouse clicks verify the window under the pointer; keys remain window-targeted.
    $points = @(if ($Narrow -or $WheelZoom) { } elseif ($NativePixels -or $MiddleDrag) { @(@(24, 24), @(287, 24)) } else { @(@(24, 24), @(287, 24), @(405, 24)) })
    foreach ($point in $points) {
        $x = [int]($point[0] * $dpi); $y = [int]($point[1] * $dpi)
        $screen = [ViewerSmokeNative+Point]::new(); $screen.X = $x; $screen.Y = $y
        if (-not [ViewerSmokeNative]::ClientToScreen($window, [ref]$screen)) { throw 'Could not map demo coordinates.' }
        if (-not [ViewerSmokeNative]::SetCursorPos($screen.X, $screen.Y)) { throw 'Could not move the test cursor.' }
        $lastCursor = $screen
        $packed = [IntPtr](($y -shl 16) -bor ($x -band 65535))
        [void][ViewerSmokeNative]::PostMessage($window, 0x0200, [IntPtr]::Zero, $packed)
        Start-Sleep -Milliseconds 80
        if (-not [ViewerSmokeNative]::ClickDemo($window)) { $lastCursor = $null; throw 'Demo click was blocked or the pointer is no longer over this window.' }
        Start-Sleep -Milliseconds 80
    }
    if ($MiddleDrag) {
        # Control mode first: middle-drag must pan locally there as well.
        [void][ViewerSmokeNative]::PostMessage($window, 0x0100, [IntPtr]0x7b, [IntPtr](1 -bor (0x58 -shl 16)))
        [void][ViewerSmokeNative]::PostMessage($window, 0x0101, [IntPtr]0x7b, [IntPtr](-1073741823 -bor (0x58 -shl 16)))
        Start-Sleep -Milliseconds 120
        $path = @(@(680, 450), @(650, 430), @(610, 405), @(570, 385), @(540, 370))
        $first = $true
        foreach ($point in $path) {
            $x = [int]($point[0] * $dpi); $y = [int]($point[1] * $dpi)
            $packed = [IntPtr](($y -shl 16) -bor ($x -band 65535))
            if ($first) {
                $screen = [ViewerSmokeNative+Point]::new(); $screen.X = $x; $screen.Y = $y
                if (-not [ViewerSmokeNative]::ClientToScreen($window, [ref]$screen)) { throw 'Could not map the drag start.' }
                if (-not [ViewerSmokeNative]::SetCursorPos($screen.X, $screen.Y)) { throw 'Could not position the drag start.' }
                $lastCursor = $screen
                [void][ViewerSmokeNative]::PostMessage($window, 0x0200, [IntPtr]::Zero, $packed)
                [void][ViewerSmokeNative]::PostMessage($window, 0x0207, [IntPtr]0x10, $packed)
                $first = $false
            } else {
                [void][ViewerSmokeNative]::PostMessage($window, 0x0200, [IntPtr]0x10, $packed)
            }
            Start-Sleep -Milliseconds 40
        }
        [void][ViewerSmokeNative]::PostMessage($window, 0x0208, [IntPtr]::Zero, $packed)
    } elseif ($WheelZoom) {
        $x = [int](680 * $dpi); $y = [int](450 * $dpi)
        $screen = [ViewerSmokeNative+Point]::new(); $screen.X = $x; $screen.Y = $y
        if (-not [ViewerSmokeNative]::ClientToScreen($window, [ref]$screen)) { throw 'Could not map the zoom anchor.' }
        if (-not [ViewerSmokeNative]::SetCursorPos($screen.X, $screen.Y)) { throw 'Could not position the zoom anchor.' }
        $lastCursor = $screen
        $clientPoint = [IntPtr](($y -shl 16) -bor ($x -band 65535))
        $screenPoint = [IntPtr](($screen.Y -shl 16) -bor ($screen.X -band 65535))
        [void][ViewerSmokeNative]::PostMessage($window, 0x0200, [IntPtr]::Zero, $clientPoint)
        [void][ViewerSmokeNative]::PostMessage($window, 0x0086, [IntPtr]1, [IntPtr]::Zero)
        [void][ViewerSmokeNative]::PostMessage($window, 0x0007, [IntPtr]::Zero, [IntPtr]::Zero)
        [void][ViewerSmokeNative]::PostMessage($window, 0x0100, [IntPtr]0x11, [IntPtr](1 -bor (0x1d -shl 16)))
        [void][ViewerSmokeNative]::PostMessage($window, 0x020a, [IntPtr]((240 -shl 16) -bor 8), $screenPoint)
        [void][ViewerSmokeNative]::PostMessage($window, 0x0101, [IntPtr]0x11, [IntPtr](-1073741823 -bor (0x1d -shl 16)))
    } else {
        # F12 changes mode but must never enable target input in demo.
        [void][ViewerSmokeNative]::PostMessage($window, 0x0100, [IntPtr]0x7b, [IntPtr](1 -bor (0x58 -shl 16)))
        [void][ViewerSmokeNative]::PostMessage($window, 0x0101, [IntPtr]0x7b, [IntPtr](-1073741823 -bor (0x58 -shl 16)))
    }
    # Drive the controls before the additional native-capture work.
    if ($WindowScreenshot) { Save-DemoWindow }
    if (-not $process.WaitForExit(10000)) { throw 'Demo smoke test did not finish within ten seconds.' }
    $text = $stdout.GetAwaiter().GetResult()
    $errors = $stderr.GetAwaiter().GetResult()
    if ($text) { Write-Output $text.Trim() }
    if ($errors) { Write-Output $errors.Trim() }
    if ($process.ExitCode -ne 0) { throw "Demo viewer exited with code $($process.ExitCode)." }
    if ($text -notmatch 'input_captured=false, scale=([0-9.]+)') { throw 'Demo input isolation or scale report is missing.' }
    $scale = [double]::Parse($Matches[1], [System.Globalization.CultureInfo]::InvariantCulture)
    if ($WheelZoom -and ($scale -lt 0.72 -or $scale -gt 1.0)) { throw "Ctrl+wheel did not zoom the local viewport: $scale" }
    if (($NativePixels -or $MiddleDrag) -and [math]::Abs($scale - 1.0) -gt 0.0001) { throw "1:1 was not pixel-exact: $scale" }
    if ($MiddleDrag) {
        if ($text -notmatch 'pan=(-?[0-9.]+),(-?[0-9.]+)') { throw 'Pan report is missing.' }
        $panX = [double]::Parse($Matches[1], [System.Globalization.CultureInfo]::InvariantCulture)
        $panY = [double]::Parse($Matches[2], [System.Globalization.CultureInfo]::InvariantCulture)
        if ($panX -gt -100 -or $panY -gt -50) { throw "Middle-drag did not pan the view: $panX,$panY" }
    }
    if (-not $WheelZoom -and -not $NativePixels -and -not $Narrow -and -not $MiddleDrag -and $scale -le 1.0) { throw "Toolbar zoom did not change the viewport: $scale" }
    if (-not (Test-Path -LiteralPath $Screenshot -PathType Leaf)) { throw 'GPU screenshot was not written.' }
    "Demo interaction completed; owned GPU screenshot: $Screenshot; DPI scale: $dpi"
} finally {
    if ($restoreCursor -and $null -ne $lastCursor) {
        $current = [ViewerSmokeNative+Point]::new()
        if ([ViewerSmokeNative]::GetCursorPos([ref]$current) -and $current.X -eq $lastCursor.X -and $current.Y -eq $lastCursor.Y) {
            [void][ViewerSmokeNative]::SetCursorPos($originalCursor.X, $originalCursor.Y)
        }
    }
    if (-not $process.HasExited) {
        if ($window -ne [IntPtr]::Zero) { [void][ViewerSmokeNative]::PostMessage($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
        if (-not $process.WaitForExit(5000)) { $process.Kill() }
    }
}
