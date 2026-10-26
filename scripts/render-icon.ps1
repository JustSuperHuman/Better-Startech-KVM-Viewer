param([Parameter(Mandatory = $true)][string]$OutputPath)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $OutputPath) { throw 'Refusing to replace an existing icon.' }
if (-not (Test-Path -LiteralPath (Split-Path $OutputPath -Parent) -PathType Container)) { throw 'Icon output directory does not exist.' }
Add-Type -AssemblyName System.Drawing
function RoundedPath([single]$X, [single]$Y, [single]$Width, [single]$Height, [single]$Radius) {
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $diameter = $Radius * 2
    $path.AddArc($X, $Y, $diameter, $diameter, 180, 90)
    $path.AddArc($X + $Width - $diameter, $Y, $diameter, $diameter, 270, 90)
    $path.AddArc($X + $Width - $diameter, $Y + $Height - $diameter, $diameter, $diameter, 0, 90)
    $path.AddArc($X, $Y + $Height - $diameter, $diameter, $diameter, 90, 90)
    $path.CloseFigure()
    return $path
}
# Mirrors assets/icons/cart.svg. GDI rasterization needs no downloaded renderer.
$bitmap = [System.Drawing.Bitmap]::new(256, 256, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$tile = RoundedPath 4 4 56 56 14
$monitor = RoundedPath 14 17 36 26 4
$brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
    [System.Drawing.PointF]::new(4, 4), [System.Drawing.PointF]::new(60, 60),
    [System.Drawing.Color]::FromArgb(53, 106, 202), [System.Drawing.Color]::FromArgb(35, 74, 148))
$pen = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 3.2)
try {
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.ScaleTransform(4, 4)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $graphics.FillPath($brush, $tile)
    $graphics.DrawPath($pen, $monitor)
    $graphics.DrawLine($pen, 32, 43, 32, 50)
    $graphics.DrawLine($pen, 24, 50, 40, 50)
    $graphics.DrawLines($pen, [System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new(23, 25), [System.Drawing.PointF]::new(28, 30), [System.Drawing.PointF]::new(23, 35)))
    $graphics.DrawLine($pen, 34, 35, 41, 35)
    $graphics.Flush()
    $bitmap.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    "Rendered viewer icon: $OutputPath"
} finally {
    $pen.Dispose(); $brush.Dispose(); $monitor.Dispose(); $tile.Dispose()
    $graphics.Dispose(); $bitmap.Dispose()
}
