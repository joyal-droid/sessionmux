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
  film grain against banding), the desktop of the desktop mockups (a black and white wallpaper
  with translucent ribbons under a taskbar of plain vector glyphs), the window treatment for a capture (soft drop shadow, 1 px light
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
    Draw-Blobs $sg $sw $sh $blobs
    $sg.Dispose()

    # Soften what is left of the gradient rims on the small bitmap, one channel at a time.
    Blur-Bitmap $small 2

    $bmp = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    # The small bitmap has one cell of margin on every side.
    Draw-Stretched $g $small ([System.Drawing.RectangleF]::new(-$k, -$k, $sw * $k, $sh * $k))
    $small.Dispose()
    Add-Grain $g $w $h $Grain $Seed
    $g.Dispose()
    return $bmp
}

# Fills the blobs on $sg, a small bitmap of $sw x $sh with one cell of margin on every side, as
# radial gradients with a Gaussian falloff.
function Draw-Blobs($sg, [int]$sw, [int]$sh, $blobs) {
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
}

# Blurs the colour channels of a 32-bit $bmp in place with Blur-Array of radius $r.
function Blur-Bitmap($bmp, [int]$r) {
    $bytes = Read-Pixels $bmp
    $bw = $bmp.Width; $bh = $bmp.Height; $n = $bw * $bh
    for ($c = 0; $c -lt 3; $c++) {
        $a = New-Object double[] $n
        for ($i = 0; $i -lt $n; $i++) { $a[$i] = $bytes[4 * $i + $c] }
        $a = Blur-Array $a $bw $bh $r
        for ($i = 0; $i -lt $n; $i++) { $bytes[4 * $i + $c] = [byte][math]::Round($a[$i]) }
    }
    Write-Pixels $bmp $bytes
}

# Lays a mono noise of grey levels over $w x $h on $g at $Grain opacity (0.035 = 3.5 %); a $Grain
# of 0 draws nothing.
function Add-Grain($g, [int]$w, [int]$h, [double]$Grain, [int]$Seed) {
    if ($Grain -le 0) { return }
    # A 64 px tile of random grey levels in 2 x 2 px grains at the grain opacity, repeated over
    # the picture. The repeat is invisible at this opacity, the 2 px grain survives the
    # downscaling of a README or a link preview, and both let the PNG encoder find matches, which
    # keeps the files a fraction of the size of noise over every pixel.
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
    $mode = $g.PixelOffsetMode
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::None
    $g.FillRectangle($tb, 0, 0, $w, $h)
    $g.PixelOffsetMode = $mode
    $tb.Dispose(); $tile.Dispose()
}

# ---------------------------------------------------------------- desktop

# The desktop wallpaper, in black and white: a diagonal field from near-black at the top left to
# light grey at the bottom right (stops as position along the diagonal and colour), a soft light
# bloom in the lower right, and translucent ribbons sweeping across. Each ribbon is the band
# between two cubic Bezier curves, as x, y pairs in fractions of the width and height: Top runs
# from left to right and Bottom back from right to left. Alpha is its opacity at the right end; the
# left end has a third of it.
$DesktopField = @(@(0.0, '#050505'), @(0.45, '#161616'), @(0.78, '#3E3E3E'), @(1.0, '#8E8E8E'))
$DesktopBloom = @(@{ X = 0.96; Y = 0.98; R = 0.40; Color = '#E0E0E0'; Alpha = 0.50 })
$DesktopRibbons = @(
    @{ Top = @(-0.10, 0.78, 0.30, 0.70, 0.55, 0.30, 1.10, 0.18); Bottom = @(1.10, 0.25, 0.60, 0.44, 0.35, 0.88, -0.10, 0.94); Color = '#FFFFFF'; Alpha = 0.28 },
    @{ Top = @(-0.10, 0.56, 0.35, 0.64, 0.65, 0.18, 1.10, 0.04); Bottom = @(1.10, 0.08, 0.66, 0.25, 0.36, 0.70, -0.10, 0.60); Color = '#FFFFFF'; Alpha = 0.36 },
    @{ Top = @(0.15, 1.10, 0.45, 0.86, 0.75, 0.76, 1.10, 0.50); Bottom = @(1.10, 0.72, 0.80, 0.94, 0.55, 1.02, 0.25, 1.15); Color = '#D9D9D9'; Alpha = 0.30 }
)

# Fills one ribbon (see $DesktopRibbons) on $sg, a small bitmap of $sw x $sh with one cell of
# margin on every side, with a horizontal gradient from a third of its opacity to all of it.
function Fill-Ribbon($sg, [int]$sw, [int]$sh, $ribbon) {
    $tp = New-Object System.Drawing.PointF[] 4
    $bp = New-Object System.Drawing.PointF[] 4
    for ($i = 0; $i -lt 4; $i++) {
        $tp[$i] = [System.Drawing.PointF]::new(1 + $ribbon.Top[2 * $i] * ($sw - 2), 1 + $ribbon.Top[2 * $i + 1] * ($sh - 2))
        $bp[$i] = [System.Drawing.PointF]::new(1 + $ribbon.Bottom[2 * $i] * ($sw - 2), 1 + $ribbon.Bottom[2 * $i + 1] * ($sh - 2))
    }
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddBezier($tp[0], $tp[1], $tp[2], $tp[3])
    $path.AddLine($tp[3], $bp[0])
    $path.AddBezier($bp[0], $bp[1], $bp[2], $bp[3])
    $path.CloseFigure()
    $bounds = $path.GetBounds()
    $a = [int](255 * $ribbon.Alpha)
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($bounds, (CA ([int]($a / 3)) $ribbon.Color), (CA $a $ribbon.Color), [float]0)
    $brush.WrapMode = [System.Drawing.Drawing2D.WrapMode]::TileFlipXY
    $sg.FillPath($brush, $path)
    $brush.Dispose(); $path.Dispose()
}

# Returns a $w x $h 24-bit Bitmap of the desktop: the wallpaper drawn on a bitmap 1/8 of the size,
# blurred there by about 8 px and scaled up with bicubic interpolation, the film grain at $Grain
# opacity, and the taskbar along the bottom edge (Draw-Taskbar).
function New-Desktop([int]$w, [int]$h, [double]$Grain = 0.035, [int]$Seed = 7) {
    $k = 8
    $sw = [int][math]::Ceiling($w / $k) + 2; $sh = [int][math]::Ceiling($h / $k) + 2
    $small = New-Object System.Drawing.Bitmap($sw, $sh, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $sg = [System.Drawing.Graphics]::FromImage($small)
    $sg.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    # The field's gradient line runs from the top-left to the bottom-right corner.
    $field = New-Object System.Drawing.Drawing2D.LinearGradientBrush([System.Drawing.PointF]::new(0, 0),
        [System.Drawing.PointF]::new($sw, $sh), [System.Drawing.Color]::Black, [System.Drawing.Color]::White)
    $cb = New-Object System.Drawing.Drawing2D.ColorBlend($DesktopField.Count)
    for ($i = 0; $i -lt $DesktopField.Count; $i++) {
        $cb.Positions[$i] = [float]$DesktopField[$i][0]
        $cb.Colors[$i] = C $DesktopField[$i][1]
    }
    $field.InterpolationColors = $cb
    $field.WrapMode = [System.Drawing.Drawing2D.WrapMode]::TileFlipXY
    $sg.FillRectangle($field, 0, 0, $sw, $sh)
    $field.Dispose()
    Draw-Blobs $sg $sw $sh $DesktopBloom
    foreach ($ribbon in $DesktopRibbons) { Fill-Ribbon $sg $sw $sh $ribbon }
    $sg.Dispose()
    Blur-Bitmap $small 1

    $bmp = New-Object System.Drawing.Bitmap($w, $h, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
        # The small bitmap has one cell of margin on every side.
        Draw-Stretched $g $small ([System.Drawing.RectangleF]::new(-$k, -$k, $sw * $k, $sh * $k))
        Add-Grain $g $w $h $Grain $Seed
        Set-Quality $g
        Draw-Taskbar $g $w $h
    } finally { $g.Dispose(); $small.Dispose() }
    return $bmp
}

function New-Pen($color, [float]$width) {
    $pen = New-Object System.Drawing.Pen($color, $width)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    return $pen
}

function Fill-RoundRect($g, $brush, [float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $path = New-RoundRect $x $y $w $h $r
    $g.FillPath($brush, $path)
    $path.Dispose()
}

function Draw-RoundRect($g, $pen, [float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $path = New-RoundRect $x $y $w $h $r
    $g.DrawPath($pen, $path)
    $path.Dispose()
}

# Draws one taskbar glyph in $color centred in the 28 px tile at ($x, $y): Start (a 2 x 2 grid of
# rounded squares), Search (a magnifier), TaskView (two overlapping rounded rectangles), Folder,
# Chat (a speech bubble with three dots) or Browser (a globe).
function Draw-TaskbarGlyph($g, [string]$name, [float]$x, [float]$y, $color) {
    $cx = $x + 14; $cy = $y + 14
    $brush = New-Object System.Drawing.SolidBrush($color)
    $pen = New-Pen $color 1.75
    switch ($name) {
        'Start' {
            foreach ($dx in 0, 11) { foreach ($dy in 0, 11) { Fill-RoundRect $g $brush ($x + 4 + $dx) ($y + 4 + $dy) 9 9 2 } }
        }
        'Search' {
            $g.DrawEllipse($pen, [float]($cx - 8.5), [float]($cy - 8.5), [float]13, [float]13)
            $handle = New-Pen $color 2.25
            $g.DrawLine($handle, [float]($cx + 2.6), [float]($cy + 2.6), [float]($cx + 7.5), [float]($cy + 7.5))
            $handle.Dispose()
        }
        'TaskView' {
            $front = New-RoundRect ($x + 4) ($y + 10) 15 12 2.5
            $clip = New-RoundRect ($x + 3) ($y + 9) 17 14 3.5
            $g.SetClip($clip, [System.Drawing.Drawing2D.CombineMode]::Exclude)
            Draw-RoundRect $g $pen ($x + 9) ($y + 5) 15 12 2.5
            $g.ResetClip()
            $g.DrawPath($pen, $front)
            $front.Dispose(); $clip.Dispose()
        }
        'Folder' {
            Fill-RoundRect $g $brush ($x + 4) ($y + 6) 9 6 2
            Fill-RoundRect $g $brush ($x + 4) ($y + 8.5) 20 14 2.5
            $seam = New-Object System.Drawing.SolidBrush((CA 150 '#202020'))
            $g.FillRectangle($seam, [float]($x + 4), [float]($y + 11), [float]20, [float]1.25)
            $seam.Dispose()
        }
        'Chat' {
            $bubble = New-RoundRect ($x + 4) ($y + 5) 20 15 5
            $tail = New-Object System.Drawing.Drawing2D.GraphicsPath
            $tail.AddPolygon([System.Drawing.PointF[]]@(
                [System.Drawing.PointF]::new($x + 8, $y + 18), [System.Drawing.PointF]::new($x + 8, $y + 24),
                [System.Drawing.PointF]::new($x + 14, $y + 18)))
            $g.FillPath($brush, $bubble); $g.FillPath($brush, $tail)
            $dot = New-Object System.Drawing.SolidBrush((CA 235 '#202020'))
            foreach ($dx in -5, 0, 5) { $g.FillEllipse($dot, [float]($cx + $dx - 1.6), [float]($y + 12.5 - 1.6), [float]3.2, [float]3.2) }
            $bubble.Dispose(); $tail.Dispose(); $dot.Dispose()
        }
        'Browser' {
            $thin = New-Pen $color 1.5
            $g.DrawEllipse($thin, [float]($cx - 9), [float]($cy - 9), [float]18, [float]18)
            $g.DrawEllipse($thin, [float]($cx - 4), [float]($cy - 9), [float]8, [float]18)
            $g.DrawLine($thin, [float]($cx - 9), [float]$cy, [float]($cx + 9), [float]$cy)
            $hw = [math]::Sqrt(81 - 4.5 * 4.5)
            foreach ($dy in -4.5, 4.5) { $g.DrawLine($thin, [float]($cx - $hw), [float]($cy + $dy), [float]($cx + $hw), [float]($cy + $dy)) }
            $thin.Dispose()
        }
    }
    $brush.Dispose(); $pen.Dispose()
}

# Draws one notification-area icon in $color centred on ($cx, $cy): Chevron (the overflow arrow),
# Dot (a neutral grey app icon), Mark (the Sessionmux mark), Network (three arcs), Volume (a
# speaker) or Battery.
function Draw-TrayGlyph($g, [string]$name, [float]$cx, [float]$cy, $color) {
    $brush = New-Object System.Drawing.SolidBrush($color)
    $pen = New-Pen $color 1.5
    switch ($name) {
        'Chevron' {
            $g.DrawLines($pen, [System.Drawing.PointF[]]@(
                [System.Drawing.PointF]::new($cx - 4.5, $cy + 2.25), [System.Drawing.PointF]::new($cx, $cy - 2.25),
                [System.Drawing.PointF]::new($cx + 4.5, $cy + 2.25)))
        }
        'Dot' {
            $grey = New-Object System.Drawing.SolidBrush((C '#9A9A9A'))
            $g.FillEllipse($grey, [float]($cx - 7), [float]($cy - 7), [float]14, [float]14)
            $grey.Dispose()
        }
        'Mark' { Draw-TrayMark $g ($cx - 8) ($cy - 8) 16 }
        'Network' {
            $by = $cy + 5.5
            foreach ($r in 3.5, 7, 10.5) {
                $g.DrawArc($pen, [float]($cx - $r), [float]($by - $r), [float](2 * $r), [float](2 * $r), [float]225, [float]90)
            }
            $g.FillEllipse($brush, [float]($cx - 1.4), [float]($by - 1.4), [float]2.8, [float]2.8)
        }
        'Volume' {
            $sx = $cx - 3
            $body = New-Object System.Drawing.Drawing2D.GraphicsPath
            $body.AddPolygon([System.Drawing.PointF[]]@(
                [System.Drawing.PointF]::new($sx - 6, $cy - 2.75), [System.Drawing.PointF]::new($sx - 3, $cy - 2.75),
                [System.Drawing.PointF]::new($sx + 1.5, $cy - 6.5), [System.Drawing.PointF]::new($sx + 1.5, $cy + 6.5),
                [System.Drawing.PointF]::new($sx - 3, $cy + 2.75), [System.Drawing.PointF]::new($sx - 6, $cy + 2.75)))
            $g.FillPath($brush, $body)
            $body.Dispose()
            foreach ($r in 4, 7.5) {
                $g.DrawArc($pen, [float]($sx + 1.5 - $r), [float]($cy - $r), [float](2 * $r), [float](2 * $r), [float]-45, [float]90)
            }
        }
        'Battery' {
            $thin = New-Pen $color 1.25
            Draw-RoundRect $g $thin ($cx - 10) ($cy - 5) 18 10 2.5
            Fill-RoundRect $g $brush ($cx + 8.75) ($cy - 2) 2 4 0.75
            Fill-RoundRect $g $brush ($cx - 8) ($cy - 3) 10 6 1
            $thin.Dispose()
        }
    }
    $brush.Dispose(); $pen.Dispose()
}

# Draws the Sessionmux mark (the small variant, as in the notification area) so that its tile
# covers the square at ($x, $y) with side $side, filled with one white-to-grey gradient: a rounded
# tile on a 24-unit grid (x and y 3.5 to 20.5, corner radius 3.7) split into two panes by a
# slanted gap from x 13.7-16.1 at the top to x 8.5-10.9 at the bottom.
function Draw-TrayMark($g, [float]$x, [float]$y, [float]$side) {
    $t = 3.5; $e = 20.5; $r = 3.7; $dd = 2 * $r
    $left = New-Object System.Drawing.Drawing2D.GraphicsPath
    $left.AddLine([float]($t + $r), [float]$t, [float]13.7, [float]$t)
    $left.AddLine([float]13.7, [float]$t, [float]8.5, [float]$e)
    $left.AddLine([float]8.5, [float]$e, [float]($t + $r), [float]$e)
    $left.AddArc([float]$t, [float]($e - $dd), [float]$dd, [float]$dd, 90, 90)
    $left.AddArc([float]$t, [float]$t, [float]$dd, [float]$dd, 180, 90)
    $left.CloseFigure()
    $right = New-Object System.Drawing.Drawing2D.GraphicsPath
    $right.AddLine([float]16.1, [float]$t, [float]($e - $r), [float]$t)
    $right.AddArc([float]($e - $dd), [float]$t, [float]$dd, [float]$dd, 270, 90)
    $right.AddArc([float]($e - $dd), [float]($e - $dd), [float]$dd, [float]$dd, 0, 90)
    $right.AddLine([float]($e - $r), [float]$e, [float]10.9, [float]$e)
    $right.CloseFigure()
    $k = $side / 17
    $m = New-Object System.Drawing.Drawing2D.Matrix
    $m.Translate($x - $t * $k, $y - $t * $k)
    $m.Scale($k, $k)
    $bounds = [System.Drawing.RectangleF]::new($x - 0.5, $y - 0.5, $side + 1, $side + 1)
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($bounds, (C '#FFFFFF'), (C '#A8A8A8'), [float]45, $true)
    $brush.WrapMode = [System.Drawing.Drawing2D.WrapMode]::TileFlipXY
    foreach ($p in $left, $right) {
        $p.Transform($m)
        $g.FillPath($brush, $p)
        $p.Dispose()
    }
    $brush.Dispose(); $m.Dispose()
}

# Draws the taskbar over the bottom 48 px of a $w x $h desktop: an acrylic bar of
# rgba(32, 32, 32, .72) with a 1 px rgba(255, 255, 255, .08) top line. In the centre, six 28 px
# glyph tiles 12 px apart in white at 67 %, the chat tile with the 3 px running-app underline in
# rgba(255, 255, 255, .7). On the right, the overflow chevron, a grey app icon and the Sessionmux
# mark, then network, volume and battery, the time and date in 12 px Segoe UI, and the 2 px
# show-desktop sliver at the edge.
function Draw-Taskbar($g, [int]$w, [int]$h) {
    $top = $h - 48; $cy = $top + 24
    $bar = New-Object System.Drawing.SolidBrush((CA 184 '#202020'))
    $g.FillRectangle($bar, 0, $top, $w, 48)
    $line = New-Object System.Drawing.SolidBrush((CA 20 '#FFFFFF'))
    $g.FillRectangle($line, 0, $top, $w, 1)
    $bar.Dispose(); $line.Dispose()

    $tiles = @('Start', 'Search', 'TaskView', 'Folder', 'Chat', 'Browser')
    $tile = 28; $gap = 12
    $x0 = [math]::Round(($w - ($tiles.Count * $tile + ($tiles.Count - 1) * $gap)) / 2)
    $glyph = CA 171 '#FFFFFF'
    for ($i = 0; $i -lt $tiles.Count; $i++) {
        $tx = $x0 + $i * ($tile + $gap)
        Draw-TaskbarGlyph $g $tiles[$i] $tx ($cy - 16) $glyph
        if ($tiles[$i] -eq 'Chat') {
            $ub = New-Object System.Drawing.SolidBrush((CA 179 '#FFFFFF'))
            Fill-RoundRect $g $ub ($tx + 7) ($top + 41) 14 3 1.5
            $ub.Dispose()
        }
    }

    # The clock: two lines, right-aligned 20 px from the edge.
    $font = New-Object System.Drawing.Font('Segoe UI', 12, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
    $fmt = New-Object System.Drawing.StringFormat([System.Drawing.StringFormat]::GenericTypographic)
    $fmt.Alignment = [System.Drawing.StringAlignment]::Far
    $white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
    $right = $w - 20
    $fh = $font.GetHeight($g)
    $g.DrawString('10:24', $font, $white, [float]$right, [float]($cy - 8 - $fh / 2), $fmt)
    $g.DrawString('Mon 12 Jan', $font, $white, [float]$right, [float]($cy + 8 - $fh / 2), $fmt)
    $tw = [math]::Max($g.MeasureString('10:24', $font, 1000, $fmt).Width, $g.MeasureString('Mon 12 Jan', $font, 1000, $fmt).Width)
    $font.Dispose(); $fmt.Dispose(); $white.Dispose()

    # The notification area, from right to left, with the centre of each icon.
    $icon = CA 235 '#FFFFFF'
    $bx = [math]::Round($right - $tw - 26)
    $slots = @(@('Battery', $bx), @('Volume', ($bx - 26)), @('Network', ($bx - 50)),
        @('Mark', ($bx - 84)), @('Dot', ($bx - 112)), @('Chevron', ($bx - 140)))
    foreach ($s in $slots) { Draw-TrayGlyph $g $s[0] $s[1] $cy $icon }

    $sliver = New-Object System.Drawing.SolidBrush((CA 90 '#FFFFFF'))
    Fill-RoundRect $g $sliver ($w - 6) ($cy - 11) 2 22 1
    $sliver.Dispose()
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
