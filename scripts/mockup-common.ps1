# Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
#
# The contents of this file are subject to the Common Public Attribution License Version 1.0
# (the "License"); you may not use this file except in compliance with the License. You may
# obtain a copy of the License in the LICENSE file at the root of this repository or at
# https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
# Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
# network and provide for limited attribution for the Original Developer. In addition, Exhibit A
# has been modified to be consistent with Exhibit B.
#
# Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
# ANY KIND, either express or implied. See the License for the specific language governing rights
# and limitations under the License.
#
# The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
# portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
# Contributor(s): see the git history of this repository.
#
# Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
# https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
# interface of every copy and derivative; see docs/attribution.md.

<#
.SYNOPSIS
  Drawing helpers shared by make-hero.ps1, make-desktop-mockups.ps1 and make-social-preview.ps1.

.DESCRIPTION
  Dot-sourced after System.Drawing is loaded:

    . (Join-Path $PSScriptRoot 'mockup-common.ps1')

  Provides the soft gradient wallpaper (large radial blobs over a light base, blurred, with a mono
  film grain against banding), the window treatment for a capture (soft drop shadow, 1 px light
  inner border, always at native size), the white callout chips, and a PNG writer that keeps only
  the image chunks. GDI+ has no blur, so soft shapes are drawn on a small bitmap, blurred there
  with a box filter and scaled up with bicubic interpolation.
#>

function C([string]$hex) { return [System.Drawing.ColorTranslator]::FromHtml($hex) }
function CA([int]$a, [string]$hex) { return [System.Drawing.Color]::FromArgb($a, (C $hex)) }

function Read-Image([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "missing input capture: $path (run the demo fixture and .\scripts\compress-png.ps1 first)"
    }
    # Load from a memory copy so the file is not kept open.
    $ms = New-Object System.IO.MemoryStream(, [System.IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $path).Path))
    return [System.Drawing.Image]::FromStream($ms)
}

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = [math]::Min($r, [math]::Min($w, $h) / 2)
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Set-Quality($g) {
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
}

# Draws $img scaled into $dst with its edge pixels repeated, so bicubic scaling does not fade the
# border towards transparent.
function Draw-Stretched($g, $img, [System.Drawing.RectangleF]$dst) {
    $ia = New-Object System.Drawing.Imaging.ImageAttributes
    $ia.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
    $pts = [System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new($dst.X, $dst.Y),
        [System.Drawing.PointF]::new($dst.Right, $dst.Y),
        [System.Drawing.PointF]::new($dst.X, $dst.Bottom))
    $src = [System.Drawing.RectangleF]::new(0, 0, $img.Width, $img.Height)
    $g.DrawImage($img, $pts, $src, [System.Drawing.GraphicsUnit]::Pixel, $ia)
    $ia.Dispose()
}

# Three passes of a running-sum box filter of radius $r over a $w x $h array, horizontally and
# vertically; together close to a Gaussian with a sigma of about $r. Edges repeat.
function Blur-Array([double[]]$a, [int]$w, [int]$h, [int]$r) {
    if ($r -lt 1) { return , $a }
    $n = 2 * $r + 1
    $tmp = New-Object double[] ($w * $h)
    for ($pass = 0; $pass -lt 3; $pass++) {
        for ($y = 0; $y -lt $h; $y++) {
            $row = $y * $w
            $sum = 0.0
            for ($i = -$r; $i -le $r; $i++) { $sum += $a[$row + [math]::Min([math]::Max($i, 0), $w - 1)] }
            for ($x = 0; $x -lt $w; $x++) {
                $tmp[$row + $x] = $sum / $n
                $sum += $a[$row + [math]::Min($x + $r + 1, $w - 1)] - $a[$row + [math]::Max($x - $r, 0)]
            }
        }
        for ($x = 0; $x -lt $w; $x++) {
            $sum = 0.0
            for ($i = -$r; $i -le $r; $i++) { $sum += $tmp[[math]::Min([math]::Max($i, 0), $h - 1) * $w + $x] }
            for ($y = 0; $y -lt $h; $y++) {
                $a[$y * $w + $x] = $sum / $n
                $sum += $tmp[[math]::Min($y + $r + 1, $h - 1) * $w + $x] - $tmp[[math]::Max($y - $r, 0) * $w + $x]
            }
        }
    }
    return , $a
}

# Copies the 32-bit BGRA pixels of $bmp out (Read-Pixels) and back (Write-Pixels).
function Read-Pixels($bmp) {
    $rect = [System.Drawing.Rectangle]::new(0, 0, $bmp.Width, $bmp.Height)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $bytes = New-Object byte[] (4 * $bmp.Width * $bmp.Height)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
    $bmp.UnlockBits($data)
    return , $bytes
}

function Write-Pixels($bmp, [byte[]]$bytes) {
    $rect = [System.Drawing.Rectangle]::new(0, 0, $bmp.Width, $bmp.Height)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::WriteOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    [System.Runtime.InteropServices.Marshal]::Copy($bytes, 0, $data.Scan0, $bytes.Length)
    $bmp.UnlockBits($data)
}

# ---------------------------------------------------------------- wallpaper

# The default blob layout: centre and radius as fractions of the width, height and diagonal, the
# colour and its opacity at the centre.
$DefaultBlobs = @(
    @{ X = 0.08; Y = 0.10; R = 0.50; Color = '#7C3AED'; Alpha = 0.60 },
    @{ X = 0.95; Y = 0.92; R = 0.55; Color = '#2563EB'; Alpha = 0.55 },
    @{ X = 0.80; Y = 0.08; R = 0.40; Color = '#C7D2FE'; Alpha = 0.95 },
    @{ X = 0.30; Y = 0.95; R = 0.40; Color = '#EEF2FF'; Alpha = 0.95 }
)

# Returns a $w x $h 24-bit Bitmap: the blobs as radial gradients with a Gaussian falloff over
# #F5F6FF on a bitmap 1/16 of the size, blurred there, scaled up with bicubic interpolation, then a
# mono noise of grey levels at $Grain opacity (0.035 = 3.5 %) laid over it.
function New-Wallpaper([int]$w, [int]$h, $blobs = $DefaultBlobs, [double]$Grain = 0.035, [int]$Seed = 7) {
    $k = 16
    $sw = [int][math]::Ceiling($w / $k) + 2; $sh = [int][math]::Ceiling($h / $k) + 2
    $small = New-Object System.Drawing.Bitmap($sw, $sh, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $sg = [System.Drawing.Graphics]::FromImage($small)
    $sg.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $sg.Clear((C '#F5F6FF'))
    # Blend positions run from the rim (0) to the centre (1).
    $blend = New-Object System.Drawing.Drawing2D.Blend(9)
    $e = [math]::Exp(-4.0)
    for ($i = 0; $i -lt 9; $i++) {
        $p = $i / 8.0
        $blend.Positions[$i] = [float]$p
        $blend.Factors[$i] = [float](([math]::Exp(-4.0 * (1 - $p) * (1 - $p)) - $e) / (1 - $e))
    }
    $diag = [math]::Sqrt($sw * $sw + $sh * $sh)
    foreach ($b in $blobs) {
        $cx = 1 + $b.X * ($sw - 2); $cy = 1 + $b.Y * ($sh - 2); $r = $b.R * $diag
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $path.AddEllipse([float]($cx - $r), [float]($cy - $r), [float](2 * $r), [float](2 * $r))
        $pgb = New-Object System.Drawing.Drawing2D.PathGradientBrush($path)
        $pgb.CenterPoint = [System.Drawing.PointF]::new($cx, $cy)
        $pgb.CenterColor = CA ([int](255 * $b.Alpha)) $b.Color
        $pgb.SurroundColors = [System.Drawing.Color[]]@((CA 0 $b.Color))
        $pgb.Blend = $blend
        $sg.FillPath($pgb, $path)
        $pgb.Dispose(); $path.Dispose()
    }
    $sg.Dispose()

    # Soften what is left of the gradient rims on the small bitmap, one channel at a time.
    $bytes = Read-Pixels $small
    $n = $sw * $sh
    for ($c = 0; $c -lt 3; $c++) {
        $a = New-Object double[] $n
        for ($i = 0; $i -lt $n; $i++) { $a[$i] = $bytes[4 * $i + $c] }
        $a = Blur-Array $a $sw $sh 2
        for ($i = 0; $i -lt $n; $i++) { $bytes[4 * $i + $c] = [byte][math]::Round($a[$i]) }
    }
    Write-Pixels $small $bytes

    $bmp = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    # The small bitmap has one cell of margin on every side.
    Draw-Stretched $g $small ([System.Drawing.RectangleF]::new(-$k, -$k, $sw * $k, $sh * $k))
    $small.Dispose()

    if ($Grain -gt 0) {
        # A 64 px tile of random grey levels in 2 x 2 px grains at the grain opacity, repeated
        # over the picture. The repeat is invisible at this opacity, the 2 px grain survives the
        # downscaling of a README or a link preview, and both let the PNG encoder find matches,
        # which keeps the files a fraction of the size of noise over every pixel.
        $tn = 64; $cells = $tn / 2
        $tile = New-Object System.Drawing.Bitmap($tn, $tn, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $noise = New-Object byte[] ($cells * $cells)
        (New-Object System.Random($Seed)).NextBytes($noise)
        $alpha = [byte][math]::Round(255 * $Grain)
        $px = New-Object byte[] (4 * $tn * $tn)
        for ($y = 0; $y -lt $tn; $y++) {
            for ($x = 0; $x -lt $tn; $x++) {
                $v = $noise[($y -shr 1) * $cells + ($x -shr 1)]; $o = 4 * ($y * $tn + $x)
                $px[$o] = $v; $px[$o + 1] = $v; $px[$o + 2] = $v; $px[$o + 3] = $alpha
            }
        }
        Write-Pixels $tile $px
        $tb = New-Object System.Drawing.TextureBrush($tile)
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::None
        $g.FillRectangle($tb, 0, 0, $w, $h)
        $tb.Dispose(); $tile.Dispose()
    }
    $g.Dispose()
    return $bmp
}

# ---------------------------------------------------------------- shadows, windows, chips

# Draws the shadow of a rounded rectangle like the CSS box-shadow "0 $dy $blur $color" at $opacity:
# the shape is drawn on a bitmap scaled down so that the Gaussian sigma ($blur / 2) is about 4 px
# there, blurred, and scaled back up.
function Draw-SoftShadow($g, [float]$x, [float]$y, [float]$w, [float]$h, [float]$r, [float]$dy, [float]$blur, [string]$color, [double]$opacity) {
    $sigma = $blur / 2
    $k = [math]::Max(1, [int][math]::Round($sigma / 4))
    $pad = [int][math]::Ceiling(3 * $sigma / $k) + 1
    $sw = [int]([math]::Ceiling($w / $k) + 2 * $pad); $sh = [int]([math]::Ceiling($h / $k) + 2 * $pad)
    $mask = New-Object System.Drawing.Bitmap($sw, $sh, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $mg = [System.Drawing.Graphics]::FromImage($mask)
    $mg.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $mg.Clear([System.Drawing.Color]::Transparent)
    $path = New-RoundRect $pad $pad ($w / $k) ($h / $k) ($r / $k)
    $wb = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
    $mg.FillPath($wb, $path)
    $wb.Dispose(); $path.Dispose(); $mg.Dispose()

    $bytes = Read-Pixels $mask
    $n = $sw * $sh
    $a = New-Object double[] $n
    for ($i = 0; $i -lt $n; $i++) { $a[$i] = $bytes[4 * $i + 3] }
    $a = Blur-Array $a $sw $sh ([int][math]::Max(1, [math]::Round($sigma / $k)))
    $col = C $color
    for ($i = 0; $i -lt $n; $i++) {
        $o = 4 * $i
        $bytes[$o] = $col.B; $bytes[$o + 1] = $col.G; $bytes[$o + 2] = $col.R
        $bytes[$o + 3] = [byte][math]::Round($a[$i] * $opacity)
    }
    Write-Pixels $mask $bytes

    $dst = [System.Drawing.RectangleF]::new($x - $pad * $k, $y + $dy - $pad * $k, $sw * $k, $sh * $k)
    Draw-Stretched $g $mask $dst
    $mask.Dispose()
}

# Corner radius of a window capture. The panel is captured on an opaque dark square, so the radius
# is where the window's border starts on the top row.
function Get-CaptureRadius($img) {
    $bmp = New-Object System.Drawing.Bitmap($img)
    try {
        $c0 = $bmp.GetPixel(0, 0)
        for ($x = 1; $x -lt [math]::Min(64, $bmp.Width); $x++) {
            $c = $bmp.GetPixel($x, 0)
            if ([math]::Abs([int]$c.R + $c.G + $c.B - $c0.R - $c0.G - $c0.B) -gt 18) { return $x + 1 }
        }
        return 14
    } finally { $bmp.Dispose() }
}

# The opaque part of a capture that carries its own shadow in transparent margins (the mini tray),
# as a Rectangle in capture pixels.
function Get-OpaqueBounds($img) {
    $bmp = New-Object System.Drawing.Bitmap($img)
    try {
        $bytes = Read-Pixels $bmp
        $bw = $bmp.Width; $bh = $bmp.Height
        $minX = $bw; $minY = $bh; $maxX = -1; $maxY = -1
        for ($y = 0; $y -lt $bh; $y++) {
            for ($x = 0; $x -lt $bw; $x++) {
                if ($bytes[4 * ($y * $bw + $x) + 3] -gt 200) {
                    if ($x -lt $minX) { $minX = $x }
                    if ($x -gt $maxX) { $maxX = $x }
                    if ($y -lt $minY) { $minY = $y }
                    if ($y -gt $maxY) { $maxY = $y }
                }
            }
        }
        if ($maxX -lt $minX) { throw 'the capture has no opaque pixels' }
        return [System.Drawing.Rectangle]::new($minX, $minY, $maxX - $minX + 1, $maxY - $minY + 1)
    } finally { $bmp.Dispose() }
}

# 1 px rgba(255, 255, 255, .14) just inside the edge of a rounded rectangle.
function Draw-InnerBorder($g, [float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $pen = New-Object System.Drawing.Pen((CA 36 '#FFFFFF'), 1)
    $path = New-RoundRect ($x + 0.5) ($y + 0.5) ($w - 1) ($h - 1) ([math]::Max(0, $r - 0.5))
    $g.DrawPath($pen, $path)
    $path.Dispose(); $pen.Dispose()
}

# A window capture (the panel) at native size at integer ($x, $y): shadow 0 24 px 60 px
# rgba(15, 23, 42, .35), the capture clipped to corner radius $r, and the inner border.
function Draw-Window($g, $img, [int]$x, [int]$y, [float]$r) {
    Draw-SoftShadow $g $x $y $img.Width $img.Height $r 24 60 '#0F172A' 0.35
    $tb = New-Object System.Drawing.TextureBrush($img)
    $tb.WrapMode = [System.Drawing.Drawing2D.WrapMode]::Clamp
    $tb.TranslateTransform($x, $y)
    $path = New-RoundRect $x $y $img.Width $img.Height $r
    $g.FillPath($tb, $path)
    $path.Dispose(); $tb.Dispose()
    Draw-InnerBorder $g $x $y $img.Width $img.Height $r
}

# The mini tray capture at native size at integer ($x, $y), with the same shadow and inner border
# around its opaque pill ($pill, from Get-OpaqueBounds).
function Draw-Tray($g, $img, $pill, [int]$x, [int]$y) {
    $r = $pill.Height / 2
    Draw-SoftShadow $g ($x + $pill.X) ($y + $pill.Y) $pill.Width $pill.Height $r 24 60 '#0F172A' 0.35
    $dst = [System.Drawing.Rectangle]::new($x, $y, $img.Width, $img.Height)
    $g.DrawImage($img, $dst, 0, 0, $img.Width, $img.Height, [System.Drawing.GraphicsUnit]::Pixel)
    Draw-InnerBorder $g ($x + $pill.X) ($y + $pill.Y) $pill.Width $pill.Height $r
}

# A white callout chip: $text in 12 px Segoe UI Semibold #111, radius 10 px, shadow 0 6 px 18 px
# rgba(15, 23, 42, .18). $x is the left edge, or the right edge with -AlignRight; $cy is the
# vertical centre. Returns the chip's RectangleF.
function Draw-Chip($g, [string]$text, [float]$x, [float]$cy, [switch]$AlignRight) {
    $font = New-Object System.Drawing.Font('Segoe UI Semibold', 12, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
    $fmt = [System.Drawing.StringFormat]::GenericTypographic
    $tw = [math]::Ceiling($g.MeasureString($text, $font, 1000, $fmt).Width)
    $padX = 12; $cw = $tw + 2 * $padX; $ch = 28
    $left = if ($AlignRight) { $x - $cw } else { $x }
    $left = [math]::Round($left); $top = [math]::Round($cy - $ch / 2)
    Draw-SoftShadow $g $left $top $cw $ch 10 6 18 '#0F172A' 0.18
    $path = New-RoundRect $left $top $cw $ch 10
    $wb = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
    $g.FillPath($wb, $path)
    $tbr = New-Object System.Drawing.SolidBrush((C '#111111'))
    $ty = $top + ($ch - $font.GetHeight($g)) / 2
    $g.DrawString($text, $font, $tbr, [float]($left + $padX), [float]$ty, $fmt)
    $font.Dispose(); $wb.Dispose(); $tbr.Dispose(); $path.Dispose()
    return [System.Drawing.RectangleF]::new($left, $top, $cw, $ch)
}

# ---------------------------------------------------------------- output

# Saves a 24-bit $bmp as PNG with only the image chunks (IHDR, PLTE, tRNS, IDAT, IEND): no text,
# time, gamma, resolution or colour-profile chunks. The pixels go through the WPF PNG encoder,
# whose row filters compress soft gradients to about half the size the System.Drawing encoder
# reaches. Returns the file size in KB.
function Save-CleanPng($bmp, [string]$path) {
    Add-Type -AssemblyName PresentationCore
    $rect = [System.Drawing.Rectangle]::new(0, 0, $bmp.Width, $bmp.Height)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    try {
        $buf = New-Object byte[] ($data.Stride * $bmp.Height)
        [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $buf, 0, $buf.Length)
        $source = [System.Windows.Media.Imaging.BitmapSource]::Create($bmp.Width, $bmp.Height, 96, 96,
            [System.Windows.Media.PixelFormats]::Bgr24, $null, $buf, $data.Stride)
    } finally { $bmp.UnlockBits($data) }
    $enc = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $enc.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($source))
    $ms = New-Object System.IO.MemoryStream
    $enc.Save($ms)
    $src = $ms.ToArray(); $ms.Dispose()
    $keep = @('IHDR', 'PLTE', 'tRNS', 'IDAT', 'IEND')
    $out = New-Object System.IO.MemoryStream
    $out.Write($src, 0, 8)
    $pos = 8
    while ($pos + 8 -le $src.Length) {
        $len = ([int]$src[$pos] -shl 24) -bor ([int]$src[$pos + 1] -shl 16) -bor ([int]$src[$pos + 2] -shl 8) -bor [int]$src[$pos + 3]
        $type = [System.Text.Encoding]::ASCII.GetString($src, $pos + 4, 4)
        if ($keep -contains $type) { $out.Write($src, $pos, 12 + $len) }
        $pos += 12 + $len
        if ($type -eq 'IEND') { break }
    }
    $full = [System.IO.Path]::GetFullPath($path)
    [System.IO.File]::WriteAllBytes($full, $out.ToArray())
    $out.Dispose()
    return [math]::Round((Get-Item -LiteralPath $full).Length / 1KB, 1)
}
