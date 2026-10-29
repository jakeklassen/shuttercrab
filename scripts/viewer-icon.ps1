# Makes the stream viewer's icon, crates/shuttercrab/assets/shuttercrab-viewer.ico,
# from the app's: every size of it with its colours swapped, dark brackets on
# coral, so a viewer's taskbar button stands apart from the app's.
#
#   pwsh -NoProfile -File scripts/viewer-icon.ps1
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assets = Join-Path $PSScriptRoot '..\crates\shuttercrab\assets'
$source = [IO.File]::ReadAllBytes((Join-Path $assets 'shuttercrab.ico'))

# The icon's two colours: its background and its brackets.
$dark = [System.Drawing.Color]::FromArgb(42, 46, 54)
$coral = [System.Drawing.Color]::FromArgb(232, 96, 60)

$count = [BitConverter]::ToUInt16($source, 4)
$images = foreach ($i in 0..($count - 1)) {
  $entry = 6 + 16 * $i
  $length = [BitConverter]::ToUInt32($source, $entry + 8)
  $offset = [BitConverter]::ToUInt32($source, $entry + 12)
  $stream = New-Object IO.MemoryStream (, $source[$offset..($offset + $length - 1)])
  $image = New-Object System.Drawing.Bitmap $stream
  for ($y = 0; $y -lt $image.Height; $y++) {
    for ($x = 0; $x -lt $image.Width; $x++) {
      $p = $image.GetPixel($x, $y)
      if ($p.A -eq 0) { continue }
      # Where the pixel lies from the background (0) to the brackets (1),
      # edges between them included, then the same place the other way.
      $t = [Math]::Min(1, [Math]::Max(0, ($p.R - $dark.R) / [double]($coral.R - $dark.R)))
      $mix = { param($from, $to) [int][Math]::Round($from + ($to - $from) * $t) }
      $image.SetPixel($x, $y, [System.Drawing.Color]::FromArgb($p.A,
          (& $mix $coral.R $dark.R), (& $mix $coral.G $dark.G), (& $mix $coral.B $dark.B)))
    }
  }
  $png = New-Object IO.MemoryStream
  $image.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
  , @($source[$entry..($entry + 15)], $png.ToArray())
}

# An icon file of PNG images: the header, a directory entry each, the images.
$out = New-Object IO.MemoryStream
$out.Write($source, 0, 6)
$offset = 6 + 16 * $count
foreach ($image in $images) {
  $entry = [byte[]]$image[0].Clone()
  [BitConverter]::GetBytes([uint32]$image[1].Length).CopyTo($entry, 8)
  [BitConverter]::GetBytes([uint32]$offset).CopyTo($entry, 12)
  $out.Write($entry, 0, 16)
  $offset += $image[1].Length
}
foreach ($image in $images) { $out.Write($image[1], 0, $image[1].Length) }
[IO.File]::WriteAllBytes((Join-Path $assets 'shuttercrab-viewer.ico'), $out.ToArray())
"wrote shuttercrab-viewer.ico, $count sizes"
