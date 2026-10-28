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
  Draws assets\icon.ico (the application icon), assets\tray.ico (the notification area icon) and
  the macOS images in assets\macos from the Sessionmux mark.

.DESCRIPTION
  The mark is a rounded tile (24-unit grid, x and y 3.5 to 20.5, corner radius 3.7) split by a
  slanted gap into two panes, both filled with one gradient from #7C3AED at the top left to
  #2563EB at the bottom right. The gap runs from x 14.1-15.7 at the top to x 8.9-10.5 at the
  bottom; sizes of 24 px and below use the small variant, whose gap is 2.4 units wide so the
  split stays visible at 16 px.

  Both icons draw the mark itself, centred at about 86 % of the side on a transparent
  background: icon.ico in sizes 16, 20, 24, 32, 40, 48, 64, 128 and 256, tray.ico in sizes 16,
  20, 24, 32 and 48.

  Every frame is drawn with System.Drawing and stored as a PNG entry; the ICO container is
  written here, and each file is loaded back with System.Drawing.Icon as a check. The frames are
  also saved as PNG files to -Frames (by default a folder in the temporary directory, outside the
  repository) for review. build.rs embeds both icons.

  For macOS, assets\macos\Sessionmux.iconset holds the application icon as the ten PNG files
  the system's icon format is built from (16, 32, 128, 256 and 512 points, each also at twice
  the pixels as @2x), drawn like icon.ico; the macOS build turns the folder into Sessionmux.icns.
  assets\macos\trayTemplate.png (18 x 18) and trayTemplate@2x.png (36 x 36) are the menu bar
  icon: the small variant of the mark in black on transparent, which macOS tints for light and
  dark menu bars. Each PNG records 72 dpi, or 144 dpi for @2x files, so its size in points is
  its 1x size.

  Regenerate after the mark changes, from the repository root:

    .\scripts\make-icons.ps1
#>
[CmdletBinding()]
param(
    [string]$Assets = (Join-Path $PSScriptRoot '..\assets'),
    [string]$Frames = (Join-Path ([System.IO.Path]::GetTempPath()) 'sessionmux-icon-frames')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$violet = [System.Drawing.ColorTranslator]::FromHtml('#7C3AED')
$blue = [System.Drawing.ColorTranslator]::FromHtml('#2563EB')

# The tile on the 24-unit grid, and how far each side of the gap moves out in the small variant.
$Tile = @{ X = 3.5; Size = 17; R = 3.7 }
$SmallWiden = 0.4

# Builds the two panes in grid units. $d moves the left pane's right edge left and the right
# pane's left edge right by the same amount.
function New-Panes([float]$d) {
    $t = $Tile.X; $e = $Tile.X + $Tile.Size; $dd = 2 * $Tile.R
    $left = New-Object System.Drawing.Drawing2D.GraphicsPath
    $left.AddLine([float]($t + $Tile.R), [float]$t, [float](14.1 - $d), [float]$t)
    $left.AddLine([float](14.1 - $d), [float]$t, [float](8.9 - $d), [float]$e)
    $left.AddLine([float](8.9 - $d), [float]$e, [float]($t + $Tile.R), [float]$e)
    $left.AddArc([float]$t, [float]($e - $dd), [float]$dd, [float]$dd, 90, 90)
    $left.AddArc([float]$t, [float]$t, [float]$dd, [float]$dd, 180, 90)
    $left.CloseFigure()

    $right = New-Object System.Drawing.Drawing2D.GraphicsPath
    $right.AddLine([float](15.7 + $d), [float]$t, [float]($e - $Tile.R), [float]$t)
    $right.AddArc([float]($e - $dd), [float]$t, [float]$dd, [float]$dd, 270, 90)
    $right.AddArc([float]($e - $dd), [float]($e - $dd), [float]$dd, [float]$dd, 0, 90)
    $right.AddLine([float]($e - $Tile.R), [float]$e, [float](10.5 + $d), [float]$e)
    $right.CloseFigure()
    return $left, $right
}

# Draws the mark so that its tile covers the square at ($x, $y) with side $side: both panes are
# filled by one gradient laid over the tile bounds, so the colour runs on across the gap. With
# $solid (a colour) both panes are filled with that colour instead.
function Draw-Mark($g, [float]$x, [float]$y, [float]$side, [bool]$small, $solid = $null) {
    $k = $side / $Tile.Size
    $m = New-Object System.Drawing.Drawing2D.Matrix
    $m.Translate($x - $Tile.X * $k, $y - $Tile.X * $k)
    $m.Scale($k, $k)
    $d = if ($small) { $SmallWiden } else { 0 }
    $panes = New-Panes $d
    if ($null -ne $solid) {
        $brush = New-Object System.Drawing.SolidBrush($solid)
    } else {
        $bounds = New-Object System.Drawing.RectangleF(($x - 0.5), ($y - 0.5), ($side + 1), ($side + 1))
        $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($bounds, $violet, $blue, [float]45, $true)
        $brush.WrapMode = [System.Drawing.Drawing2D.WrapMode]::TileFlipXY
    }
    foreach ($p in $panes) {
        $p.Transform($m)
        $g.FillPath($brush, $p)
        $p.Dispose()
    }
    $brush.Dispose(); $m.Dispose()
}

function New-Canvas([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    return $bmp, $g
}

# One icon frame: the mark at about 86 % of the side, on a whole-pixel margin so the straight
# tile edges stay sharp, on a transparent background.
function New-Frame([int]$size) {
    $bmp, $g = New-Canvas $size
    try {
        $margin = [int][math]::Round($size * 0.07)
        Draw-Mark $g $margin $margin ($size - 2 * $margin) ($size -le 24)
    } finally { $g.Dispose() }
    return $bmp
}

# One menu bar template frame: the small variant of the mark in black, one pixel in from each
# edge per 18 pixels, on a transparent background.
function New-TemplateFrame([int]$size) {
    $bmp, $g = New-Canvas $size
    try {
        $margin = [int]($size / 18)
        Draw-Mark $g $margin $margin ($size - 2 * $margin) $true ([System.Drawing.Color]::Black)
    } finally { $g.Dispose() }
    return $bmp
}

# Saves $bmp as a PNG file recording $dpi, and a copy to the frames folder as macos-<name>.
function Save-Png($bmp, [string]$path, [int]$dpi) {
    $bmp.SetResolution($dpi, $dpi)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    [System.IO.File]::Copy($path, (Join-Path $script:FrameDir ('macos-' + (Split-Path -Leaf $path))), $true)
}

# Writes the macOS images to $macDir (see the description).
function Write-MacImages([string]$macDir) {
    $iconset = Join-Path $macDir 'Sessionmux.iconset'
    [void](New-Item -ItemType Directory -Force -Path $iconset)
    foreach ($pt in @(16, 32, 128, 256, 512)) {
        foreach ($scale in @(1, 2)) {
            $suffix = if ($scale -eq 2) { '@2x' } else { '' }
            $bmp = New-Frame ($pt * $scale)
            try {
                Save-Png $bmp (Join-Path $iconset ('icon_{0}x{0}{1}.png' -f $pt, $suffix)) (72 * $scale)
            } finally { $bmp.Dispose() }
        }
    }
    foreach ($scale in @(1, 2)) {
        $suffix = if ($scale -eq 2) { '@2x' } else { '' }
        $bmp = New-TemplateFrame (18 * $scale)
        try {
            Save-Png $bmp (Join-Path $macDir ('trayTemplate{0}.png' -f $suffix)) (72 * $scale)
        } finally { $bmp.Dispose() }
    }
    $count = (Get-ChildItem $iconset -Filter '*.png').Count
    if ($count -ne 10) { throw "Sessionmux.iconset: expected 10 PNG files, found $count" }
    Write-Host ("macos: Sessionmux.iconset {0} PNG files, trayTemplate 18/36 px" -f $count)
}

# Writes an ICO file whose entries are PNG images: ICONDIR, one ICONDIRENTRY per image (a side of
# 256 is stored as 0), then the PNG data in the same order. Each frame is also saved to the
# frames folder as <name>-<size>.png.
function Write-Ico([string]$path, [int[]]$sizes, [scriptblock]$draw) {
    $name = [System.IO.Path]::GetFileNameWithoutExtension($path)
    $images = @()
    foreach ($s in $sizes) {
        $bmp = & $draw $s
        $ms = New-Object System.IO.MemoryStream
        try {
            $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
            $images += , $ms.ToArray()
            [System.IO.File]::WriteAllBytes((Join-Path $script:FrameDir ('{0}-{1}.png' -f $name, $s)), $ms.ToArray())
        } finally { $ms.Dispose(); $bmp.Dispose() }
    }
    $out = New-Object System.IO.MemoryStream
    $wr = New-Object System.IO.BinaryWriter($out)
    try {
        $wr.Write([uint16]0)              # reserved
        $wr.Write([uint16]1)              # type: icon
        $wr.Write([uint16]$sizes.Count)
        $offset = 6 + 16 * $sizes.Count
        for ($i = 0; $i -lt $sizes.Count; $i++) {
            $side = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
            $wr.Write([byte]$side)        # width
            $wr.Write([byte]$side)        # height
            $wr.Write([byte]0)            # palette colours
            $wr.Write([byte]0)            # reserved
            $wr.Write([uint16]1)          # colour planes
            $wr.Write([uint16]32)         # bits per pixel
            $wr.Write([uint32]$images[$i].Length)
            $wr.Write([uint32]$offset)
            $offset += $images[$i].Length
        }
        foreach ($img in $images) { $wr.Write($img) }
        $wr.Flush()
        [System.IO.File]::WriteAllBytes($path, $out.ToArray())
    } finally { $wr.Dispose(); $out.Dispose() }

    # Load check: the file must open as an icon at every size, and every size below 256 must come
    # back at its own side (System.Drawing on .NET Framework hands out the next smaller entry for
    # 256, whose side is stored as 0).
    foreach ($s in $sizes) {
        $ico = New-Object System.Drawing.Icon($path, $s, $s)
        try {
            if ($s -lt 256 -and ($ico.Width -ne $s -or $ico.Height -ne $s)) {
                throw ("{0}: asked for {1} px, got {2}x{3}" -f [System.IO.Path]::GetFileName($path), $s, $ico.Width, $ico.Height)
            }
        } finally { $ico.Dispose() }
    }
    $kb = [math]::Round((Get-Item $path).Length / 1KB, 1)
    Write-Host ("{0} {1} px {2} KB" -f [System.IO.Path]::GetFileName($path), ($sizes -join '/'), $kb)
}

$dir = [System.IO.Path]::GetFullPath($Assets)
$script:FrameDir = [System.IO.Path]::GetFullPath($Frames)
[void](New-Item -ItemType Directory -Force -Path $script:FrameDir)
Write-Ico (Join-Path $dir 'icon.ico') @(16, 20, 24, 32, 40, 48, 64, 128, 256) { param($s) New-Frame $s }
Write-Ico (Join-Path $dir 'tray.ico') @(16, 20, 24, 32, 48) { param($s) New-Frame $s }
Write-MacImages (Join-Path $dir 'macos')
Write-Host ("frames: {0} PNG files" -f (Get-ChildItem $script:FrameDir -Filter '*.png').Count)
