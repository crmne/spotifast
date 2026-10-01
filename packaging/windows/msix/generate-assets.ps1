param(
    [string]$SourceIcon = "$PSScriptRoot\..\..\macos\icon-1024.png",
    [string]$OutputDir = "$PSScriptRoot\Assets"
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing

if (-not (Test-Path $SourceIcon)) {
    throw "Source icon not found at: $SourceIcon"
}

if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

$resolvedSource = (Resolve-Path $SourceIcon).Path
$src = [System.Drawing.Bitmap]::FromFile($resolvedSource)

function Export-ResizedPng {
    param(
        [int]$TargetWidth,
        [int]$TargetHeight,
        [string]$DestinationPath,
        [int]$IconSize
    )

    $bmp = New-Object System.Drawing.Bitmap($TargetWidth, $TargetHeight, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bmp)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.Clear([System.Drawing.Color]::Transparent)

    $x = [int](($TargetWidth - $IconSize) / 2)
    $y = [int](($TargetHeight - $IconSize) / 2)
    $graphics.DrawImage($src, $x, $y, $IconSize, $IconSize)
    $graphics.Dispose()

    $bmp.Save($DestinationPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

# Tile and logo scale factors Windows uses for high-DPI displays.
# scale-100 is the baseline; Windows picks the closest available variant.
$scales = @(100, 125, 150, 200, 400)

# Base (scale-100) logical sizes for each asset type
$tiles = @(
    @{ Base = "Square150x150Logo"; W = 150; H = 150; Icon = 150 },
    @{ Base = "Square44x44Logo";   W = 44;  H = 44;  Icon = 44  },
    @{ Base = "StoreLogo";         W = 50;  H = 50;  Icon = 50  },
    @{ Base = "SmallTile";         W = 71;  H = 71;  Icon = 71  },
    @{ Base = "Wide310x150Logo";   W = 310; H = 150; Icon = 130 },
    @{ Base = "SplashScreen";      W = 620; H = 300; Icon = 260 }
)

try {
    foreach ($tile in $tiles) {
        foreach ($scale in $scales) {
            $factor = $scale / 100.0
            $w = [int]([Math]::Round($tile.W * $factor))
            $h = [int]([Math]::Round($tile.H * $factor))
            $icon = [int]([Math]::Round($tile.Icon * $factor))
            $path = Join-Path $OutputDir "$($tile.Base).scale-$scale.png"
            Export-ResizedPng -TargetWidth $w -TargetHeight $h -DestinationPath $path -IconSize $icon
        }
    }

    # Unplated transparent taskbar icons across all standard target sizes.
    # These use target-size (physical pixels), not scale, so one set covers all DPIs.
    $targetSizes = @(16, 20, 24, 30, 32, 36, 40, 44, 48, 64, 256)
    foreach ($size in $targetSizes) {
        $p1 = Join-Path $OutputDir "Square44x44Logo.targetsize-${size}.png"
        $p2 = Join-Path $OutputDir "Square44x44Logo.targetsize-${size}_altform-unplated.png"
        $p3 = Join-Path $OutputDir "Square44x44Logo.altform-unplated_targetsize-${size}.png"

        Export-ResizedPng -TargetWidth $size -TargetHeight $size -DestinationPath $p1 -IconSize $size
        Copy-Item -Path $p1 -Destination $p2 -Force
        Copy-Item -Path $p1 -Destination $p3 -Force
    }

    Write-Host "Generated MSIX visual assets in $OutputDir"
}
finally {
    $src.Dispose()
}
