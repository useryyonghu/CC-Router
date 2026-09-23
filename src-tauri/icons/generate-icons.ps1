#requires -Version 7
<#
  CC Router — application icon generator (Plan 4, icon/metadata task).

  Design (all geometry authored in a 512x512 design space, then scaled):
    * rounded square, full-bleed 512 canvas, inset 12, corner radius 104
      (corner rounding is applied as an anti-aliased alpha mask)
    * background  #1F3A5F  (deep blue)
    * two horizontal arrows  #5EEAD4  (teal)
        - top arrow points right  ->  shaft y 150..194, head tip x 412
        - bottom arrow points left <- shaft y 318..362, head tip x 100
      both shafts 228 wide, 44 thick, heads 84 wide / 124 tall
      vertical extent 110..402, horizontal extent 100..412  => symmetric 88px side
      padding and 98px top/bottom padding, 44px gap between the arrows.

  Route: PowerShell 7 + System.Drawing (no new dependency).
  Every size is rendered natively at 4x supersampling and then downscaled with
  HighQualityBicubic, so each output size gets its own anti-aliasing instead of
  being a blurry resize of the 512 master.

  Outputs (written next to this script):
    icon.png (512), 128x128@2x.png (256), 128x128.png (128), 32x32.png (32)
    icon.ico — multi-size ICO, BMP/DIB entries for 16, 32, 48 and 256 px
               (DIB rather than PNG-compressed entries: maximum compatibility
                with the Windows resource compiler used by tauri-build and
                with the NSIS installer icon loader)
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$OutDir = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }
$SuperSample = 4

$BgColor = [System.Drawing.Color]::FromArgb(255, 0x1F, 0x3A, 0x5F)
$FgColor = [System.Drawing.Color]::FromArgb(255, 0x5E, 0xEA, 0xD4)

$BgBrush = New-Object System.Drawing.SolidBrush $BgColor
$FgBrush = New-Object System.Drawing.SolidBrush $FgColor
$BlackBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::Black)
$WhiteBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)

# ---------------------------------------------------------------- geometry ---
$Design = 512.0
$CornerInset = 12.0
$CornerRadius = 104.0

# one arrow: shaft rect + triangular head, expressed in design units
$ShaftThickness = 44.0
$HeadWidth = 84.0
$HeadHalfHeight = 62.0
$Left = 100.0
$Right = 412.0                      # 412 - 100 = 312 wide, centred on 256
$TopCenterY = 172.0
$BottomCenterY = 340.0

function ConvertTo-DesignPoint([double]$x, [double]$y, [double]$scale) {
  return New-Object System.Drawing.PointF ([float]($x * $scale)), ([float]($y * $scale))
}

function New-RoundedRectPath([double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2.0
  $path.AddArc([float]$x, [float]$y, [float]$d, [float]$d, 180, 90)
  $path.AddArc([float]($x + $w - $d), [float]$y, [float]$d, [float]$d, 270, 90)
  $path.AddArc([float]($x + $w - $d), [float]($y + $h - $d), [float]$d, [float]$d, 0, 90)
  $path.AddArc([float]$x, [float]($y + $h - $d), [float]$d, [float]$d, 90, 90)
  $path.CloseFigure()
  return $path
}

function New-ContentBitmap([int]$size) {
  # opaque full-bleed artwork at SuperSample x, then downscale
  $big = New-Object System.Drawing.Bitmap ($size * $SuperSample), ($size * $SuperSample), ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($big)
  try {
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $scale = ($size * $SuperSample) / $Design

    $g.FillRectangle($BgBrush, 0, 0, $big.Width, $big.Height)

    # top arrow: points right
    $topShaft = New-Object System.Drawing.RectangleF ([float]($Left * $scale)), ([float](($TopCenterY - $ShaftThickness / 2) * $scale)), ([float](($Right - $HeadWidth - $Left) * $scale)), ([float]($ShaftThickness * $scale))
    $g.FillRectangle($FgBrush, $topShaft)
    $topHead = @(
      (ConvertTo-DesignPoint ($Right - $HeadWidth) ($TopCenterY - $HeadHalfHeight) $scale),
      (ConvertTo-DesignPoint ($Right - $HeadWidth) ($TopCenterY + $HeadHalfHeight) $scale),
      (ConvertTo-DesignPoint $Right $TopCenterY $scale)
    )
    $g.FillPolygon($FgBrush, [System.Drawing.PointF[]]$topHead)

    # bottom arrow: points left
    $bottomShaft = New-Object System.Drawing.RectangleF ([float](($Left + $HeadWidth) * $scale)), ([float](($BottomCenterY - $ShaftThickness / 2) * $scale)), ([float](($Right - $Left - $HeadWidth) * $scale)), ([float]($ShaftThickness * $scale))
    $g.FillRectangle($FgBrush, $bottomShaft)
    $bottomHead = @(
      (ConvertTo-DesignPoint ($Left + $HeadWidth) ($BottomCenterY - $HeadHalfHeight) $scale),
      (ConvertTo-DesignPoint ($Left + $HeadWidth) ($BottomCenterY + $HeadHalfHeight) $scale),
      (ConvertTo-DesignPoint $Left $BottomCenterY $scale)
    )
    $g.FillPolygon($FgBrush, [System.Drawing.PointF[]]$bottomHead)

    return (Resize-Bitmap $big $size)
  }
  finally {
    $g.Dispose()
    $big.Dispose()
  }
}

function New-MaskBitmap([int]$size) {
  # white rounded square on black, downscaled the same way -> anti-aliased alpha
  $big = New-Object System.Drawing.Bitmap ($size * $SuperSample), ($size * $SuperSample), ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($big)
  try {
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.FillRectangle($BlackBrush, 0, 0, $big.Width, $big.Height)
    $scale = ($size * $SuperSample) / $Design
    $path = New-RoundedRectPath ($CornerInset * $scale) ($CornerInset * $scale) (($Design - 2 * $CornerInset) * $scale) (($Design - 2 * $CornerInset) * $scale) ($CornerRadius * $scale)
    try { $g.FillPath($WhiteBrush, $path) } finally { $path.Dispose() }
    return (Resize-Bitmap $big $size)
  }
  finally {
    $g.Dispose()
    $big.Dispose()
  }
}

function Resize-Bitmap([System.Drawing.Bitmap]$src, [int]$size) {
  $dst = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($dst)
  try {
    $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.DrawImage($src, (New-Object System.Drawing.Rectangle 0, 0, $size, $size), 0, 0, $src.Width, $src.Height, [System.Drawing.GraphicsUnit]::Pixel)
  }
  finally { $g.Dispose() }
  return $dst
}

function Get-PixelBytes([System.Drawing.Bitmap]$bmp) {
  $rect = New-Object System.Drawing.Rectangle 0, 0, $bmp.Width, $bmp.Height
  $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  try {
    $bytes = New-Object byte[] ($data.Stride * $bmp.Height)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
    return @{ Bytes = $bytes; Stride = $data.Stride }
  }
  finally { $bmp.UnlockBits($data) }
}

function New-IconBitmap([int]$size) {
  $content = New-ContentBitmap $size
  $mask = New-MaskBitmap $size
  try {
    $c = Get-PixelBytes $content
    $m = Get-PixelBytes $mask
    $cb = $c.Bytes; $mb = $m.Bytes
    $cs = $c.Stride; $ms = $m.Stride
    for ($y = 0; $y -lt $size; $y++) {
      for ($x = 0; $x -lt $size; $x++) {
        $ci = $y * $cs + $x * 4
        $mi = $y * $ms + $x * 4
        # BGRA: index+2 is the red channel, which is the grey mask value
        $cb[$ci + 3] = $mb[$mi + 2]
      }
    }
    $rect = New-Object System.Drawing.Rectangle 0, 0, $size, $size
    $data = $content.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::WriteOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try { [System.Runtime.InteropServices.Marshal]::Copy($cb, 0, $data.Scan0, $cb.Length) }
    finally { $content.UnlockBits($data) }
    return $content
  }
  finally { $mask.Dispose() }
}

# ------------------------------------------------------------------- ICO -----
function Get-DibEntryBytes([System.Drawing.Bitmap]$bmp) {
  $w = $bmp.Width
  $h = $bmp.Height
  $p = Get-PixelBytes $bmp
  $stride = $p.Stride
  $src = $p.Bytes

  $maskStride = [int](([math]::Floor(($w + 31) / 32)) * 4)
  $maskBytes = $maskStride * $h
  $xorBytes = $w * 4 * $h

  $ms = New-Object System.IO.MemoryStream
  $bw = New-Object System.IO.BinaryWriter $ms
  try {
    $bw.Write([uint32]40)                       # biSize
    $bw.Write([int32]$w)                        # biWidth
    $bw.Write([int32]($h * 2))                  # biHeight (XOR + AND mask)
    $bw.Write([uint16]1)                        # biPlanes
    $bw.Write([uint16]32)                       # biBitCount
    $bw.Write([uint32]0)                        # biCompression = BI_RGB
    $bw.Write([uint32]($xorBytes + $maskBytes)) # biSizeImage
    $bw.Write([int32]0)                         # biXPelsPerMeter
    $bw.Write([int32]0)                         # biYPelsPerMeter
    $bw.Write([int32]0)                         # biClrUsed
    $bw.Write([int32]0)                         # biClrImportant

    # XOR bitmap: 32bpp BGRA, bottom-up. Bitmap memory is top-down and already BGRA.
    for ($y = $h - 1; $y -ge 0; $y--) {
      $rowStart = $y * $stride
      $bw.Write($src, $rowStart, $w * 4)
    }
    # AND mask: 1bpp, all zero (fully opaque; alpha channel carries transparency)
    $zeros = New-Object byte[] $maskBytes
    $bw.Write($zeros, 0, $maskBytes)
    $bw.Flush()
    return $ms.ToArray()
  }
  finally { $bw.Dispose(); $ms.Dispose() }
}

function Write-MultiSizeIco([string]$path, [int[]]$sizes) {
  $entries = @()
  foreach ($s in $sizes) {
    $bmp = New-IconBitmap $s
    try { $entries += , @{ Size = $s; Bytes = (Get-DibEntryBytes $bmp) } }
    finally { $bmp.Dispose() }
  }
  $ms = New-Object System.IO.MemoryStream
  $bw = New-Object System.IO.BinaryWriter $ms
  try {
    $bw.Write([uint16]0)                  # reserved
    $bw.Write([uint16]1)                  # type = icon
    $bw.Write([uint16]$entries.Count)     # image count
    $offset = 6 + 16 * $entries.Count
    foreach ($e in $entries) {
      $s = $e.Size
      $bw.Write([byte]$(if ($s -ge 256) { 0 } else { $s }))  # width  (0 == 256)
      $bw.Write([byte]$(if ($s -ge 256) { 0 } else { $s }))  # height (0 == 256)
      $bw.Write([byte]0)                  # palette colour count
      $bw.Write([byte]0)                  # reserved
      $bw.Write([uint16]1)                # colour planes
      $bw.Write([uint16]32)               # bits per pixel
      $bw.Write([uint32]$e.Bytes.Length)  # bytes in resource
      $bw.Write([uint32]$offset)          # offset
      $offset += $e.Bytes.Length
    }
    foreach ($e in $entries) { $bw.Write($e.Bytes, 0, $e.Bytes.Length) }
    $bw.Flush()
    [System.IO.File]::WriteAllBytes($path, $ms.ToArray())
  }
  finally { $bw.Dispose(); $ms.Dispose() }
}

# ------------------------------------------------------------------ main -----
$pngTargets = @(
  @{ Name = 'icon.png'; Size = 512 },
  @{ Name = '128x128@2x.png'; Size = 256 },
  @{ Name = '128x128.png'; Size = 128 },
  @{ Name = '32x32.png'; Size = 32 }
)

foreach ($t in $pngTargets) {
  $bmp = New-IconBitmap $t.Size
  try {
    $path = Join-Path $OutDir $t.Name
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Host ("wrote {0} ({1}x{1})" -f $t.Name, $t.Size)
  }
  finally { $bmp.Dispose() }
}

$icoPath = Join-Path $OutDir 'icon.ico'
Write-MultiSizeIco $icoPath @(16, 32, 48, 256)
Write-Host "wrote icon.ico (16, 32, 48, 256)"

foreach ($b in @($BgBrush, $FgBrush, $BlackBrush, $WhiteBrush)) { $b.Dispose() }
