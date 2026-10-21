# Renders the ripdl mark (docs/logo.svg) into the app icon files with GDI+.
#
#   powershell -ExecutionPolicy Bypass -File packaging/icon/generate.ps1
#
# Writes:
#   crates/ripdl-gui/icons/icon.png   512x512 (Linux/AppImage/GUI bundle)
#   crates/ripdl-gui/icons/icon.ico   multi-size (Windows exe/taskbar/installer)
#   docs/logo-512.png                 flat copy for READMEs that want a PNG
#
# Geometry mirrors docs/logo.svg: a deep-teal rounded tile, a white download
# arrow and three mint speed dashes. Every size is drawn from scratch so
# small icons stay crisp instead of being downscaled.

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $root) { $root = (Get-Location).Path }
$iconDir = Join-Path $root 'crates/ripdl-gui/icons'
$docsDir = Join-Path $root 'docs'

$TileColor = [System.Drawing.Color]::FromArgb(255, 0x0B, 0x32, 0x30)
$White = [System.Drawing.Color]::White
# Mint speed dashes: farthest is faintest, so the arrow reads as moving.
$DashSoft = [System.Drawing.Color]::FromArgb(140, 0x4E, 0xE0, 0xB5)
$DashMid = [System.Drawing.Color]::FromArgb(191, 0x4E, 0xE0, 0xB5)
$DashSolid = [System.Drawing.Color]::FromArgb(255, 0x4E, 0xE0, 0xB5)

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

# Draws the mark into a fresh ARGB bitmap at $size x $size.
function New-MarkBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $s = $size / 512.0
    $tilePath = New-RoundedPath 0 0 $size $size (112 * $s)
    $tileBrush = New-Object System.Drawing.SolidBrush($TileColor)
    $g.FillPath($tileBrush, $tilePath)

    # Mint speed dashes, farthest first.
    $dashes = @(
        @(104, 176, 86, $DashSoft),
        @(104, 242, 64, $DashMid),
        @(104, 308, 42, $DashSolid)
    )
    foreach ($d in $dashes) {
        $x = [float]($d[0] * $s)
        $y = [float]($d[1] * $s)
        $w = [float]($d[2] * $s)
        $h = [float](20 * $s)
        $radius = [Math]::Min($h / 2.0, $w / 2.0)
        $dash = New-RoundedPath $x $y $w $h $radius
        $brush = New-Object System.Drawing.SolidBrush($d[3])
        $g.FillPath($brush, $dash)
        $brush.Dispose()
        $dash.Dispose()
    }

    # Download arrow.
    $arrow = [System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new(222 * $s, 132 * $s),
        [System.Drawing.PointF]::new(290 * $s, 132 * $s),
        [System.Drawing.PointF]::new(290 * $s, 254 * $s),
        [System.Drawing.PointF]::new(352 * $s, 254 * $s),
        [System.Drawing.PointF]::new(256 * $s, 384 * $s),
        [System.Drawing.PointF]::new(160 * $s, 254 * $s),
        [System.Drawing.PointF]::new(222 * $s, 254 * $s)
    )
    $whiteBrush = New-Object System.Drawing.SolidBrush($White)
    $g.FillPolygon($whiteBrush, $arrow)
    $whiteBrush.Dispose()

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
