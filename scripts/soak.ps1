param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$Screenshot,
    [ValidateRange(10, 600)][int]$Seconds = 600
)
$ErrorActionPreference = 'Stop'
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = $Executable
$start.UseShellExecute = $false
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
foreach ($argument in @('--connect', '--smoke-test', '--smoke-seconds', "$Seconds", '--screenshot', $Screenshot)) {
    $start.ArgumentList.Add($argument)
}
$viewer = [System.Diagnostics.Process]::Start($start)
$stdout = $viewer.StandardOutput.ReadToEndAsync()
$stderr = $viewer.StandardError.ReadToEndAsync()
$timer = [System.Diagnostics.Stopwatch]::StartNew()
$samples = [System.Collections.Generic.List[object]]::new()
$helperId = $null
try {
    while (-not $viewer.WaitForExit(1000)) {
        if ($timer.Elapsed.TotalSeconds -gt ($Seconds + 30)) { throw 'Viewer exceeded its bounded soak deadline.' }
        $viewer.Refresh()
        if ($null -eq $helperId) {
            $child = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($viewer.Id) AND Name='cart-device-helper.exe'" | Select-Object -First 1
            if ($null -ne $child) { $helperId = [int]$child.ProcessId }
        }
        $helperBytes = 0L
        if ($null -ne $helperId) {
            $helper = Get-Process -Id $helperId -ErrorAction SilentlyContinue
            if ($null -ne $helper -and $helper.ProcessName -eq 'cart-device-helper') { $helperBytes = $helper.PrivateMemorySize64 }
        }
        if ($timer.Elapsed.TotalSeconds -gt 15) {
            $samples.Add([pscustomobject]@{
                seconds = $timer.Elapsed.TotalSeconds
                viewer = $viewer.PrivateMemorySize64
                helper = $helperBytes
            })
        }
    }
    $text = $stdout.GetAwaiter().GetResult()
    $errors = $stderr.GetAwaiter().GetResult()
    if ($text) { Write-Output $text.Trim() }
    if ($errors) { Write-Output $errors.Trim() }
    if ($viewer.ExitCode -ne 0) { throw "Real-device soak exited with code $($viewer.ExitCode)." }
    if ($samples.Count -eq 0 -or $null -eq $helperId) { throw 'No live helper memory samples were collected.' }
    if (-not (Test-Path -LiteralPath $Screenshot -PathType Leaf)) { throw 'Soak screenshot was not produced.' }
    $first = @($samples | Select-Object -First 30)
    $last = @($samples | Select-Object -Last 30)
    [pscustomobject]@{
        duration_seconds = [math]::Round($timer.Elapsed.TotalSeconds, 1)
        samples = $samples.Count
        viewer_private_min_mib = [math]::Round((($samples | Measure-Object viewer -Minimum).Minimum / 1MB), 1)
        viewer_private_max_mib = [math]::Round((($samples | Measure-Object viewer -Maximum).Maximum / 1MB), 1)
        helper_private_min_mib = [math]::Round((($samples | Measure-Object helper -Minimum).Minimum / 1MB), 1)
        helper_private_max_mib = [math]::Round((($samples | Measure-Object helper -Maximum).Maximum / 1MB), 1)
        viewer_first_window_mib = [math]::Round((($first | Measure-Object viewer -Average).Average / 1MB), 1)
        viewer_last_window_mib = [math]::Round((($last | Measure-Object viewer -Average).Average / 1MB), 1)
        helper_first_window_mib = [math]::Round((($first | Measure-Object helper -Average).Average / 1MB), 1)
        helper_last_window_mib = [math]::Round((($last | Measure-Object helper -Average).Average / 1MB), 1)
        screenshot = $Screenshot
    } | ConvertTo-Json
} finally {
    if (-not $viewer.HasExited) {
        [void]$viewer.CloseMainWindow()
        if (-not $viewer.WaitForExit(5000)) { $viewer.Kill(); $viewer.WaitForExit() }
    }
}
