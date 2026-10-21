# Renders the ripdl mark (docs/logo.svg) into the app icon files with GDI+.
#
#   powershell -ExecutionPolicy Bypass -File packaging/icon/generate.ps1
#
# Writes:
#   crates/ripdl-gui/icons/icon.png   512x512 (Linux/AppImage/GUI bundle)
#   crates/ripdl-gui/icons/icon.ico   multi-size (Windows exe/taskbar/installer)
#   docs/logo-512.png                 flat copy for READMEs that want a PNG
#
# Geometry mirrors docs/logo.svg: a rounded blue tile, three uplinks feeding
# one download arrow, and a tray line. Every size is drawn from scratch so
# small icons stay crisp instead of being downscaled.

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $root) { $root = (Get-Location).Path }
$iconDir = Join-Path $root 'crates/ripdl-gui/icons'
$docsDir = Join-Path $root 'docs'

$TopColor = [System.Drawing.Color]::FromArgb(255, 0x4D, 0x9E, 0xDC)
$BottomColor = [System.Drawing.Color]::FromArgb(255, 0x1F, 0x53, 0x84)
$White = [System.Drawing.Color]::White
$LaneSoft = [System.Drawing.Color]::FromArgb(168, 255, 255, 255)
$LaneStrong = [System.Drawing.Color]::FromArgb(232, 255, 255, 255)
$Tray = [System.Drawing.Color]::FromArgb(230, 255, 255, 255)

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-LinePen([System.Drawing.Color]$color, [float]$width) {
    $pen = New-Object System.Drawing.Pen($color, $width)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    return $pen
}

# Draws the mark into a fresh ARGB bitmap at $size x $size.
function New-MarkBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = $size / 512.0
    $rect = New-Object System.Drawing.RectangleF(0, 0, $size, $size)
    $tile = New-RoundedPath 0 0 $size $size (116 * $s)
    $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $TopColor, $BottomColor, 65.0)
    $g.FillPath($bg, $tile)

    # Three uplinks feeding the stream (behind the arrow).
    $laneWidth = [Math]::Max(2.0, 21 * $s)
    $g.DrawLine((New-LinePen $LaneSoft $laneWidth), 142 * $s, 146 * $s, 236 * $s, 208 * $s)
    $g.DrawLine((New-LinePen $LaneStrong $laneWidth), 256 * $s, 82 * $s, 256 * $s, 208 * $s)
    $g.DrawLine((New-LinePen $LaneSoft $laneWidth), 370 * $s, 146 * $s, 276 * $s, 208 * $s)

    # Download arrow.
    $arrow = [System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new(220 * $s, 166 * $s),
        [System.Drawing.PointF]::new(292 * $s, 166 * $s),
        [System.Drawing.PointF]::new(292 * $s, 268 * $s),
        [System.Drawing.PointF]::new(356 * $s, 268 * $s),
        [System.Drawing.PointF]::new(256 * $s, 356 * $s),
        [System.Drawing.PointF]::new(156 * $s, 268 * $s),
        [System.Drawing.PointF]::new(220 * $s, 268 * $s)
    )
    $whiteBrush = New-Object System.Drawing.SolidBrush($White)
    $g.FillPolygon($whiteBrush, $arrow)

    # Tray.
    $trayWidth = [Math]::Max(2.0, 24 * $s)
    $g.DrawLine((New-LinePen $Tray $trayWidth), 172 * $s, 400 * $s, 340 * $s, 400 * $s)

    $g.Dispose()
    return $bmp
}

function Get-PngBytes([System.Drawing.Bitmap]$bmp) {
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $ms.ToArray()
    $ms.Dispose()
    return $bytes
}

# Classic ICO frame: BITMAPINFOHEADER + bottom-up BGRA pixels + AND mask.
# Small frames must be DIB, not PNG: some shell/GDI paths still cannot decode
# PNG-compressed entries (System.Drawing.Icon, older installers).
function Get-DibBytes([System.Drawing.Bitmap]$bmp) {
    $w = $bmp.Width
    $h = $bmp.Height
    $rect = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $pixels = New-Object byte[] ($stride * $h)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $pixels, 0, $pixels.Length)
    $bmp.UnlockBits($data)

    $maskRow = [int]([Math]::Ceiling($w / 32.0) * 4)
    $maskBytes = $maskRow * $h
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([UInt32]40)                          # header size
    $bw.Write([Int32]$w)                           # width
    $bw.Write([Int32]($h * 2))                     # height: XOR + AND
    $bw.Write([UInt16]1)                           # planes
    $bw.Write([UInt16]32)                          # bits per pixel
    $bw.Write([UInt32]0)                           # no compression
    $bw.Write([UInt32]($w * $h * 4 + $maskBytes))  # image size
    $bw.Write([Int32]0)
    $bw.Write([Int32]0)
    $bw.Write([UInt32]0)
    $bw.Write([UInt32]0)
    for ($y = $h - 1; $y -ge 0; $y--) {
        $bw.Write($pixels, $y * $stride, $w * 4)
    }
    $bw.Write((New-Object byte[] $maskBytes))      # AND mask: alpha does the work
    $bw.Flush()
    $bytes = $ms.ToArray()
    $bw.Dispose()
    $ms.Dispose()
    return $bytes
}

# ICO container. Frames up to 128px are DIB (maximum compatibility), the
# 256px frame is PNG (what every modern icon uses to stay small).
function Write-Ico([string]$path, [int[]]$sizes) {
    $frames = @()
    foreach ($size in $sizes) {
        $bmp = New-MarkBitmap $size
        if ($size -ge 256) {
            $frame = Get-PngBytes $bmp
            $bmp.Save((Join-Path $docsDir 'logo-256.png'), [System.Drawing.Imaging.ImageFormat]::Png)
        } else {
            $frame = Get-DibBytes $bmp
        }
        $bmp.Dispose()
        $frames += , @($size, $frame)
    }
    $fs = [System.IO.File]::Create($path)
    $bw = New-Object System.IO.BinaryWriter($fs)
    $bw.Write([UInt16]0)               # reserved
    $bw.Write([UInt16]1)               # type: icon
    $bw.Write([UInt16]$frames.Count)
    $offset = 6 + 16 * $frames.Count
    foreach ($frame in $frames) {
        $size = [int]$frame[0]
        $data = [byte[]]$frame[1]
        $dim = if ($size -ge 256) { 0 } else { $size }
        $bw.Write([byte]$dim)          # width (0 = 256)
        $bw.Write([byte]$dim)          # height
        $bw.Write([byte]0)             # palette count
        $bw.Write([byte]0)             # reserved
        $bw.Write([UInt16]1)           # color planes
        $bw.Write([UInt16]32)          # bits per pixel
        $bw.Write([UInt32]$data.Length)
        $bw.Write([UInt32]$offset)
        $offset += $data.Length
    }
    foreach ($frame in $frames) { $bw.Write([byte[]]$frame[1]) }
    $bw.Flush()
    $bw.Dispose()
    $fs.Dispose()
}

New-Item -ItemType Directory -Force -Path $iconDir | Out-Null

$png512 = New-MarkBitmap 512
$png512.Save((Join-Path $iconDir 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$png512.Save((Join-Path $docsDir 'logo-512.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$png512.Dispose()

Write-Ico (Join-Path $iconDir 'icon.ico') @(16, 24, 32, 48, 64, 128, 256)

Get-ChildItem $iconDir | ForEach-Object { "{0}  {1} bytes" -f $_.Name, $_.Length }
