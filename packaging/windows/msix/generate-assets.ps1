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

try {
    # Standard tile and logo sizes
    Export-ResizedPng -TargetWidth 150 -TargetHeight 150 -DestinationPath (Join-Path $OutputDir "Square150x150Logo.png") -IconSize 150
    Export-ResizedPng -TargetWidth 44 -TargetHeight 44 -DestinationPath (Join-Path $OutputDir "Square44x44Logo.png") -IconSize 44
    Export-ResizedPng -TargetWidth 50 -TargetHeight 50 -DestinationPath (Join-Path $OutputDir "StoreLogo.png") -IconSize 50
    Export-ResizedPng -TargetWidth 71 -TargetHeight 71 -DestinationPath (Join-Path $OutputDir "SmallTile.png") -IconSize 71
    Export-ResizedPng -TargetWidth 310 -TargetHeight 150 -DestinationPath (Join-Path $OutputDir "Wide310x150Logo.png") -IconSize 130
    Export-ResizedPng -TargetWidth 620 -TargetHeight 300 -DestinationPath (Join-Path $OutputDir "SplashScreen.png") -IconSize 260

    # Unplated transparent taskbar icons across all standard target sizes
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
