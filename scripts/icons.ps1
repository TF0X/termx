# Regenerates every icon from the SVG masters in assets/.
#   powershell -ExecutionPolicy Bypass -File scripts\icons.ps1
# assets/logo.svg is used from 48px up, assets/logo-small.svg below that.
# Rendering goes through headless Edge, which handles the SVG blurs exactly.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
          "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $edge) { throw 'Microsoft Edge not found' }

$work = Join-Path ([IO.Path]::GetTempPath()) ("termx-icons-" + [guid]::NewGuid())
New-Item -ItemType Directory $work | Out-Null

function Render-Svg([string]$svg, [string]$png) {
    $big = 1024
    $html = Join-Path $work ([IO.Path]::GetFileNameWithoutExtension($svg) + '.html')
    $uri = ([Uri](Resolve-Path $svg).Path).AbsoluteUri
    Set-Content -Encoding utf8 $html "<!doctype html><html><body style='margin:0;background:transparent'><img src='$uri' width='$big' height='$big' style='display:block'></body></html>"
    # Start-Process: Edge reports progress on stderr, which Windows PowerShell
    # would otherwise turn into a terminating error.
    $edgeArgs = @('--headless=new', '--disable-gpu', '--hide-scrollbars',
                  '--default-background-color=00000000', "--user-data-dir=`"$work\profile`"",
                  "--window-size=$big,$big", "--screenshot=`"$png`"", ([Uri]$html).AbsoluteUri)
    Start-Process -FilePath $edge -ArgumentList $edgeArgs -Wait -WindowStyle Hidden `
        -RedirectStandardError (Join-Path $work 'edge.err') -RedirectStandardOutput (Join-Path $work 'edge.out')
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path $png)) {
        if ((Get-Date) -gt $deadline) { throw "Edge did not render $svg" }
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep -Milliseconds 300
}

function Resize([Drawing.Image]$src, [int]$size) {
    $bmp = New-Object Drawing.Bitmap $size, $size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.PixelOffsetMode = 'HighQuality'
    $g.SmoothingMode = 'HighQuality'
    $g.CompositingQuality = 'HighQuality'
    $g.Clear([Drawing.Color]::Transparent)
    $attr = New-Object Drawing.Imaging.ImageAttributes
    $attr.SetWrapMode('TileFlipXY')   # no dark fringe from sampling past the edge
    $g.DrawImage($src, (New-Object Drawing.Rectangle 0, 0, $size, $size), 0, 0, $src.Width, $src.Height, 'Pixel', $attr)
    $g.Dispose()
    $bmp
}

try {
    $fullPng = Join-Path $work 'full.png'
    $smallPng = Join-Path $work 'small.png'
    Render-Svg (Join-Path $root 'assets\logo.svg') $fullPng
    Render-Svg (Join-Path $root 'assets\logo-small.svg') $smallPng
    $full = [Drawing.Image]::FromFile($fullPng)
    $small = [Drawing.Image]::FromFile($smallPng)

    $pngDir = Join-Path $root 'icons\png'
    New-Item -ItemType Directory -Force $pngDir | Out-Null

    $sizes = 16, 24, 32, 48, 64, 128, 256, 512
    $icoSizes = 16, 24, 32, 48, 64, 128, 256
    $entries = @{}
    foreach ($s in $sizes) {
        $src = if ($s -le 32) { $small } else { $full }
        $bmp = Resize $src $s
        $bmp.Save((Join-Path $pngDir "icon-$s.png"), [Drawing.Imaging.ImageFormat]::Png)
        if ($s -eq 256) { $bmp.Save((Join-Path $root 'icons\icon.png'), [Drawing.Imaging.ImageFormat]::Png) }
        $ms = New-Object IO.MemoryStream
        $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
        $entries[$s] = $ms.ToArray()
        $bmp.Dispose()
    }
    $full.Dispose(); $small.Dispose()

    # ICO with PNG-compressed entries (supported since Windows Vista).
    $ico = New-Object IO.MemoryStream
    $w = New-Object IO.BinaryWriter $ico
    $w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$icoSizes.Count)
    $offset = 6 + 16 * $icoSizes.Count
    foreach ($s in $icoSizes) {
        $dim = if ($s -ge 256) { 0 } else { $s }
        $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([uint16]1); $w.Write([uint16]32)
        $w.Write([uint32]$entries[$s].Length); $w.Write([uint32]$offset)
        $offset += $entries[$s].Length
    }
    foreach ($s in $icoSizes) { $w.Write($entries[$s]) }
    $w.Flush()
    [IO.File]::WriteAllBytes((Join-Path $root 'icons\icon.ico'), $ico.ToArray())
    Write-Host "icons written to $(Join-Path $root 'icons')"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
