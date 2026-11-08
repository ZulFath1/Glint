# Draws Glint's app icon and writes assets\glint.ico (and a 256 px preview,
# assets\glint-256.png). Run it again after changing the design:
#
#   powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
#
# The design: a four-point sparkle (a "glint") on a rounded square with a
# blue-to-purple gradient (Tokyo Night's blue #7aa2f7 and purple #bb9af7),
# plus a small second sparkle from 32 px up. Every size is drawn on its own
# (not scaled down from 256), so the small ones stay sharp.

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$assets = Join-Path $root "assets"
New-Item -ItemType Directory -Force $assets | Out-Null

$sizes = 16, 20, 24, 32, 40, 48, 64, 128, 256

function New-RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# A four-point star centered at (cx, cy): long arms of length r, curving
# in towards the center (k = how fat the middle is, 0..1).
function New-Sparkle([float]$cx, [float]$cy, [float]$r, [float]$k) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $c = $r * $k
    $top = New-Object System.Drawing.PointF($cx, ($cy - $r))
    $right = New-Object System.Drawing.PointF(($cx + $r), $cy)
    $bottom = New-Object System.Drawing.PointF($cx, ($cy + $r))
    $left = New-Object System.Drawing.PointF(($cx - $r), $cy)
    $p.AddBezier($top, (New-Object System.Drawing.PointF(($cx + $c), ($cy - $c))), (New-Object System.Drawing.PointF(($cx + $c), ($cy - $c))), $right)
    $p.AddBezier($right, (New-Object System.Drawing.PointF(($cx + $c), ($cy + $c))), (New-Object System.Drawing.PointF(($cx + $c), ($cy + $c))), $bottom)
    $p.AddBezier($bottom, (New-Object System.Drawing.PointF(($cx - $c), ($cy + $c))), (New-Object System.Drawing.PointF(($cx - $c), ($cy + $c))), $left)
    $p.AddBezier($left, (New-Object System.Drawing.PointF(($cx - $c), ($cy - $c))), (New-Object System.Drawing.PointF(($cx - $c), ($cy - $c))), $top)
    $p.CloseFigure()
    return $p
}

function Draw-Icon([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    # The tile: almost the full size (a 1 px edge at small sizes), rounded.
    $pad = [math]::Max(0.5, $s * 0.03)
    $w = $s - 2 * $pad
    $tile = New-RoundedRect $pad $pad $w $w ($w * 0.24)
    $blue = [System.Drawing.Color]::FromArgb(255, 0x7a, 0xa2, 0xf7)
    $purple = [System.Drawing.Color]::FromArgb(255, 0xbb, 0x9a, 0xf7)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.PointF(0, 0)),
        (New-Object System.Drawing.PointF($s, $s)),
        $blue, $purple)
    $g.FillPath($grad, $tile)

    # A soft dark shade at the bottom of the tile, for depth (not on tiny sizes).
    if ($s -ge 32) {
        # One gradient over the whole height (clear until 45%, then darker),
        # so it never repeats or leaves a hard edge.
        $shade = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
            (New-Object System.Drawing.PointF(0, -1)),
            (New-Object System.Drawing.PointF(0, ($s + 1))),
            [System.Drawing.Color]::Transparent,
            [System.Drawing.Color]::Transparent)
        $blend = New-Object System.Drawing.Drawing2D.ColorBlend(3)
        $clear = [System.Drawing.Color]::FromArgb(0, 0x1a, 0x1b, 0x26)
        $blend.Colors = @($clear, $clear, [System.Drawing.Color]::FromArgb(70, 0x1a, 0x1b, 0x26))
        $blend.Positions = @([single]0, [single]0.45, [single]1)
        $shade.InterpolationColors = $blend
        $g.FillPath($shade, $tile)
    }

    # The glint: a big sparkle a little left of and below the center...
    $white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
    $big = New-Sparkle ($s * 0.46) ($s * 0.54) ($s * 0.33) 0.16
    $g.FillPath($white, $big)
    # ...and a small one top right (only where there is room for it).
    if ($s -ge 32) {
        $small = New-Sparkle ($s * 0.74) ($s * 0.27) ($s * 0.12) 0.18
        $g.FillPath($white, $small)
    }
    $g.Dispose()
    return $bmp
}

# A classic icon image: a BITMAPINFOHEADER (twice the height: color + mask),
# 32-bit BGRA pixels bottom-up, then an all-zero AND mask (alpha decides).
# Every program can read these, unlike PNG at small sizes.
function Get-DibBytes($bmp) {
    $s = $bmp.Width
    $rect = New-Object System.Drawing.Rectangle(0, 0, $s, $s)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $row = $s * 4
    $pixels = New-Object byte[] ($row * $s)
    for ($y = 0; $y -lt $s; $y++) {
        # LockBits gives rows top-down; the icon wants them bottom-up.
        $src = [IntPtr]($data.Scan0.ToInt64() + $y * $data.Stride)
        [System.Runtime.InteropServices.Marshal]::Copy($src, $pixels, ($s - 1 - $y) * $row, $row)
    }
    $bmp.UnlockBits($data)
    $maskRow = [math]::Ceiling($s / 32) * 4
    $ms = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($ms)
    $w.Write([uint32]40); $w.Write([int32]$s); $w.Write([int32](2 * $s))
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]0)
    $w.Write([uint32]($pixels.Length + $maskRow * $s))
    $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
    $w.Write($pixels)
    $w.Write((New-Object byte[] ($maskRow * $s)))
    $w.Flush()
    # The comma keeps it one byte[] (PowerShell would unroll it otherwise).
    return , $ms.ToArray()
}

# Small sizes as classic bitmaps, 256 as PNG (that is what Windows itself
# does; a 256 px bitmap would be 256 KB).
$images = @()
foreach ($s in $sizes) {
    $bmp = Draw-Icon $s
    if ($s -ge 256) {
        $ms = New-Object System.IO.MemoryStream
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Save((Join-Path $assets "glint-256.png"), [System.Drawing.Imaging.ImageFormat]::Png)
        $bytes = $ms.ToArray()
    } else {
        $bytes = Get-DibBytes $bmp
    }
    $bmp.Dispose()
    $images += , @{ Size = $s; Bytes = $bytes }
}

# The .ico file: a 6-byte header, a 16-byte entry per image, then the images.
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($out)
$w.Write([uint16]0)            # reserved
$w.Write([uint16]1)            # type: icon
$w.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($img in $images) {
    $dim = if ($img.Size -ge 256) { 0 } else { $img.Size }   # 0 means 256
    $w.Write([byte]$dim)       # width
    $w.Write([byte]$dim)       # height
    $w.Write([byte]0)          # colors in palette (none)
    $w.Write([byte]0)          # reserved
    $w.Write([uint16]1)        # color planes
    $w.Write([uint16]32)       # bits per pixel
    $w.Write([uint32]$img.Bytes.Length)
    $w.Write([uint32]$offset)
    $offset += $img.Bytes.Length
}
foreach ($img in $images) { $w.Write($img.Bytes) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $assets "glint.ico"), $out.ToArray())
"wrote assets\glint.ico ($($images.Count) sizes, $($out.Length) bytes)"
