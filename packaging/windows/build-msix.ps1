<#
.SYNOPSIS
    Builds Windows MSIX packages and universal multi-architecture bundles for Spotifast.

.DESCRIPTION
    Packages spotifast.exe into a Windows App Package (.msix) or a dual-architecture
    universal bundle (.msixbundle) containing both x86_64 and aarch64 payloads.
    Visual assets, including unplated transparent taskbar icons, are generated
    dynamically from packaging/macos/icon-1024.png during the build.

    By default, packages are produced unsigned. For production distribution, packages
    can be signed with a standard code-signing certificate or via Microsoft Azure
    Trusted Signing.

.EXAMPLE
    # Build a universal multi-architecture bundle (version auto-detected from Cargo.toml):
    .\packaging\windows\build-msix.ps1

.EXAMPLE
    # Build a single-architecture MSIX package (unsigned):
    .\packaging\windows\build-msix.ps1 -Arch aarch64 -Binary .\dist\spotifast.exe

.EXAMPLE
    # Build a universal bundle with explicit binaries:
    .\packaging\windows\build-msix.ps1 -Bundle `
        -X64Binary .\target\x86_64-pc-windows-msvc\release\spotifast.exe `
        -Arm64Binary .\target\aarch64-pc-windows-msvc\release\spotifast.exe

.EXAMPLE
    # Build with an explicit version override:
    .\packaging\windows\build-msix.ps1 -Version 1.2.3

.EXAMPLE
    # Test unpacked installation locally without signing (Developer Mode):
    .\packaging\windows\build-msix.ps1 -Arch aarch64 -Binary .\dist\spotifast.exe -Register
#>

[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet("x86_64", "aarch64", "x64", "arm64")]
    [string]$Arch,
    [string]$Binary,
    [string]$OutputDir = "dist",
    [string]$X64Binary,
    [string]$Arm64Binary,
    [switch]$Bundle,
    [string]$Publisher = "CN=Carmine Paolino",
    [string]$PublisherDisplayName = "Carmine Paolino",
    [string]$CertificatePath,
    [string]$CertificatePassword,
    [string]$CertificateThumbprint,
    [string]$TrustedSigningEndpoint,
    [string]$TrustedSigningAccount,
    [string]$TrustedSigningProfile,
    [switch]$NoSign,
    [switch]$Register,
    [switch]$KeepStage
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path "$PSScriptRoot\..\..").Path

# 1. Resolve Version
if (-not $Version) {
    if ($env:GITHUB_REF_NAME) {
        $Version = $env:GITHUB_REF_NAME.TrimStart("v")
    } else {
        $cargoToml = Get-Content (Join-Path $repoRoot "Cargo.toml") -Raw
        if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
            $Version = $matches[1]
        } else {
            throw "Unable to determine version. Please provide -Version."
        }
    }
}

# MSIX requires a 4-part numeric version: Major.Minor.Build.Revision
$cleanVer = ($Version -split '-')[0]
$verParts = $cleanVer -split '\.'
while ($verParts.Count -lt 3) { $verParts += "0" }
$quadVersion = "$($verParts[0]).$($verParts[1]).$($verParts[2]).0"

# 2. Locate Windows SDK Tools (MakeAppx and SignTool)
function Find-SdkTool {
    param([string]$ToolName)
    $cmd = Get-Command $ToolName -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $kitsRoot = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (Test-Path $kitsRoot) {
        $hostArch = if ([System.Environment]::Is64BitOperatingSystem) {
            if ($env:PROCESSOR_ARCHITECTURE -match 'ARM64') { "arm64" } else { "x64" }
        } else { "x86" }

        $tools = Get-ChildItem -Path $kitsRoot -Filter $ToolName -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "\\bin\\[0-9.]+\\($hostArch|x64)\\" } |
            Sort-Object { $_.FullName } -Descending

        if ($tools) { return $tools[0].FullName }
    }
    return $null
}

$makeappx = Find-SdkTool "makeappx.exe"
if (-not $makeappx) {
    throw "makeappx.exe not found. Please install the Windows SDK."
}

$signtool = Find-SdkTool "signtool.exe"

# 3. Resolve Output Directory
$absOutputDir = (Resolve-Path $OutputDir -ErrorAction SilentlyContinue)
if (-not $absOutputDir) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
    $absOutputDir = (Resolve-Path $OutputDir).Path
} else {
    $absOutputDir = $absOutputDir.Path
}

# 4. Packaging Helper Function
function Build-SingleMsix {
    param(
        [string]$TargetArch,
        [string]$TargetBinary,
        [string]$DestinationDir,
        [string]$StageParentDir
    )

    $msixArch = switch ($TargetArch) {
        "x86_64" { "x64" }
        "x64"    { "x64" }
        "aarch64" { "arm64" }
        "arm64"  { "arm64" }
        default { throw "Unsupported architecture: $TargetArch" }
    }

    $rustArch = if ($msixArch -eq "arm64") { "aarch64" } else { "x86_64" }

    if (-not (Test-Path $TargetBinary)) {
        throw "Binary not found at: $TargetBinary"
    }

    $baseStage = if ($StageParentDir) { $StageParentDir } else { $DestinationDir }
    $stageDir = Join-Path $baseStage "msix-$msixArch-stage"
    if (Test-Path $stageDir) {
        Remove-Item -Recurse -Force $stageDir
    }
    $stageAssets = Join-Path $stageDir "Assets"
    New-Item -ItemType Directory -Path $stageAssets -Force | Out-Null

    # Generate visual assets dynamically on the fly
    Write-Host "==> Generating visual assets for $msixArch payload..."
    & pwsh -NoProfile -File (Join-Path $repoRoot "packaging\windows\msix\generate-assets.ps1") -OutputDir $stageAssets

    # Stage application payload
    Copy-Item $TargetBinary (Join-Path $stageDir "spotifast.exe") -Force
    # Keep compatibility alias for older launchers and scripts
    Copy-Item $TargetBinary (Join-Path $stageDir "fastpotify.exe") -Force
    Copy-Item (Join-Path $repoRoot "README.md") (Join-Path $stageDir "README.md") -Force
    Copy-Item (Join-Path $repoRoot "LICENSE") (Join-Path $stageDir "LICENSE") -Force
    $instTxt = Join-Path $repoRoot "packaging\windows\spotifast-installer.txt"
    if (Test-Path $instTxt) {
        Copy-Item $instTxt (Join-Path $stageDir "spotifast-installer.txt") -Force
    }

    # Generate AppxManifest.xml
    $manifestTemplate = Get-Content (Join-Path $repoRoot "packaging\windows\msix\AppxManifest.xml.template") -Raw
    $manifest = $manifestTemplate `
        -replace '\{\{PACKAGE_NAME\}\}', "Spotifast" `
        -replace '\{\{PUBLISHER\}\}', $Publisher `
        -replace '\{\{VERSION\}\}', $quadVersion `
        -replace '\{\{PROCESSOR_ARCHITECTURE\}\}', $msixArch `
        -replace '\{\{DISPLAY_NAME\}\}', "Spotifast" `
        -replace '\{\{PUBLISHER_DISPLAY_NAME\}\}', $PublisherDisplayName `
        -replace '\{\{DESCRIPTION\}\}', "A native Spotify client"

    $manifestPath = Join-Path $stageDir "AppxManifest.xml"
    [System.IO.File]::WriteAllText($manifestPath, $manifest, [System.Text.Encoding]::UTF8)

    $packageFileName = "spotifast-v$Version-$rustArch-pc-windows-msvc.msix"
    $packagePath = Join-Path $DestinationDir $packageFileName

    Write-Host "==> Packaging MSIX ($msixArch): $packagePath"
    & $makeappx pack /v /h SHA256 /d $stageDir /p $packagePath /o
    if ($LASTEXITCODE -ne 0) {
        throw "makeappx pack failed for $msixArch with exit code $LASTEXITCODE"
    }

    return @{
        Path = $packagePath
        StageDir = $stageDir
        Architecture = $msixArch
    }
}

# 5. Signing Helper Function
function Sign-Package {
    param([string]$FilePath)

    if ($NoSign) {
        Write-Host "==> Skipping code signing (-NoSign specified)."
        return
    }

    if (-not $signtool) {
        Write-Warning "signtool.exe not found. Package left unsigned."
        return
    }

    # Case A: Microsoft Azure Trusted Signing
    if ($TrustedSigningEndpoint -and $TrustedSigningAccount -and $TrustedSigningProfile) {
        Write-Host "==> Signing $FilePath with Microsoft Azure Trusted Signing ($TrustedSigningProfile)..."
        $dlibPath = (Get-Command Azure.CodeSigning.Dlib.dll -ErrorAction SilentlyContinue).Source
        $metadataJson = @"
{
  "Endpoint": "$TrustedSigningEndpoint",
  "CodeSigningAccountName": "$TrustedSigningAccount",
  "CertificateProfileName": "$TrustedSigningProfile"
}
"@
        $metadataFile = [System.IO.Path]::GetTempFileName() + ".json"
        [System.IO.File]::WriteAllText($metadataFile, $metadataJson)
        try {
            $signArgs = @(
                "sign", "/v", "/fd", "SHA256",
                "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256"
            )
            if ($dlibPath) {
                $signArgs += @("/dlib", $dlibPath, "/dmdf", $metadataFile)
            }
            $signArgs += $FilePath
            & $signtool @signArgs
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "Trusted Signing failed with exit code $LASTEXITCODE."
            }
        } finally {
            if (Test-Path $metadataFile) { Remove-Item -Force $metadataFile }
        }
        return
    }

    # Case B: Local PFX certificate
    if ($CertificatePath) {
        Write-Host "==> Signing $FilePath with certificate: $CertificatePath..."
        $signArgs = @("sign", "/v", "/fd", "SHA256")
        if ($CertificatePassword) {
            $signArgs += @("/f", (Resolve-Path $CertificatePath).Path, "/p", $CertificatePassword)
        } else {
            $signArgs += @("/f", (Resolve-Path $CertificatePath).Path)
        }
        $signArgs += @("/tr", "http://timestamp.digicert.com", "/td", "SHA256", $FilePath)
        & $signtool @signArgs
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Timestamped signing failed, retrying without timestamp..."
            $signArgsNoTs = @("sign", "/v", "/fd", "SHA256")
            if ($CertificatePassword) {
                $signArgsNoTs += @("/f", (Resolve-Path $CertificatePath).Path, "/p", $CertificatePassword)
            } else {
                $signArgsNoTs += @("/f", (Resolve-Path $CertificatePath).Path)
            }
            $signArgsNoTs += $FilePath
            & $signtool @signArgsNoTs
        }
        return
    }

    # Case C: Certificate Store by Thumbprint
    if ($CertificateThumbprint) {
        Write-Host "==> Signing $FilePath with store thumbprint: $CertificateThumbprint..."
        $signArgs = @("sign", "/v", "/fd", "SHA256", "/sha1", $CertificateThumbprint,
                      "/tr", "http://timestamp.digicert.com", "/td", "SHA256", $FilePath)
        & $signtool @signArgs
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Timestamped signing failed, retrying without timestamp..."
            & $signtool sign /v /fd SHA256 /sha1 $CertificateThumbprint $FilePath
        }
        return
    }

    Write-Host "==> No signing credentials provided. Package is produced unsigned."
}

# 6. Locate or Compile Binaries
function Resolve-TargetBinary {
    param(
        [string]$TargetArch,
        [string]$ExplicitBinary
    )

    if ($ExplicitBinary) {
        if (-not (Test-Path $ExplicitBinary)) {
            throw "Specified binary not found: $ExplicitBinary"
        }
        return (Resolve-Path $ExplicitBinary).Path
    }

    $rustArch = if ($TargetArch -match 'arm|aarch64') { "aarch64" } else { "x86_64" }
    $triplet = "$rustArch-pc-windows-msvc"

    $searchPaths = @(
        (Join-Path $repoRoot "target\$triplet\release\spotifast.exe"),
        (Join-Path $repoRoot "target\release\spotifast.exe"),
        (Join-Path $repoRoot "dist\spotifast.exe")
    )
    if ($env:CARGO_TARGET_DIR) {
        $searchPaths += (Join-Path $env:CARGO_TARGET_DIR "$triplet\release\spotifast.exe")
        $searchPaths += (Join-Path $env:CARGO_TARGET_DIR "release\spotifast.exe")
    }
    # Common local workspace build outputs
    $searchPaths += (Join-Path $repoRoot "..\RustBuilds\$triplet\release\spotifast.exe")
    $searchPaths += (Join-Path $repoRoot "..\RustBuilds\release\spotifast.exe")

    foreach ($path in $searchPaths) {
        if (Test-Path $path) {
            return (Resolve-Path $path).Path
        }
    }

    # Attempt to build via cargo if cargo is available
    $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cargoCmd) {
        Write-Host "==> Binary for $rustArch not found in build directories. Building with cargo ($triplet)..."
        $cargoArgs = @("build", "--release", "--target", $triplet)
        if ($rustArch -eq "aarch64" -and (-not ($env:PROCESSOR_ARCHITECTURE -match 'ARM64'))) {
            $cargoArgs += "--no-default-features"
        }
        & cargo @cargoArgs
        foreach ($path in $searchPaths) {
            if (Test-Path $path) {
                return (Resolve-Path $path).Path
            }
        }
    }

    return $null
}

# 7. Execution Flow
$builtPackages = @()
$isSingleArch = [bool]$Arch -or [bool]$Binary -or [bool]$Register

if ($isSingleArch -and (-not $Bundle)) {
    # Single architecture build explicitly requested
    if (-not $Arch) {
        $Arch = if ($env:PROCESSOR_ARCHITECTURE -match 'ARM64') { "aarch64" } else { "x86_64" }
    }
    $targetBin = Resolve-TargetBinary -TargetArch $Arch -ExplicitBinary $Binary
    if (-not $targetBin) {
        throw "Could not find or build binary for $Arch."
    }

    $pkg = Build-SingleMsix -TargetArch $Arch -TargetBinary $targetBin -DestinationDir $absOutputDir
    Sign-Package -FilePath $pkg.Path
    $builtPackages += $pkg.Path

    if ($Register) {
        Write-Host "==> Registering unpacked package for current user (Developer Mode)..."
        Add-AppxPackage -Register (Join-Path $pkg.StageDir "AppxManifest.xml")
        Write-Host "==> Spotifast registered successfully in the Start menu."
    } elseif (-not $KeepStage) {
        Remove-Item -Recurse -Force $pkg.StageDir -ErrorAction SilentlyContinue
    }
} else {
    # Default mode: Universal multi-architecture bundle (x64 + arm64)
    Write-Host "==> Building universal dual-architecture bundle (x64 + arm64)..."
    $x64Bin = Resolve-TargetBinary -TargetArch "x86_64" -ExplicitBinary $X64Binary
    $arm64Bin = Resolve-TargetBinary -TargetArch "aarch64" -ExplicitBinary $Arm64Binary

    if (-not $x64Bin -and -not $arm64Bin) {
        throw "Could not find or build binaries for either x86_64 or aarch64."
    }

    if (-not $x64Bin -or -not $arm64Bin) {
        $availableArch = if ($arm64Bin) { "aarch64" } else { "x86_64" }
        $availableBin = if ($arm64Bin) { $arm64Bin } else { $x64Bin }
        Write-Warning "Only the $availableArch binary is available ($availableBin). The universal bundle requires both x64 and arm64 binaries. Packaging single $availableArch MSIX..."
        $pkg = Build-SingleMsix -TargetArch $availableArch -TargetBinary $availableBin -DestinationDir $absOutputDir
        Sign-Package -FilePath $pkg.Path
        $builtPackages += $pkg.Path
        if (-not $KeepStage) {
            Remove-Item -Recurse -Force $pkg.StageDir -ErrorAction SilentlyContinue
        }
    } else {
        $bundleStageDir = Join-Path $absOutputDir "msix-bundle-stage"
        $buildStageDir = Join-Path $absOutputDir "msix-bundle-build-stage"
        if (Test-Path $bundleStageDir) { Remove-Item -Recurse -Force $bundleStageDir }
        if (Test-Path $buildStageDir) { Remove-Item -Recurse -Force $buildStageDir }
        New-Item -ItemType Directory -Path $bundleStageDir -Force | Out-Null
        New-Item -ItemType Directory -Path $buildStageDir -Force | Out-Null

        $pkgX64 = Build-SingleMsix -TargetArch "x86_64" -TargetBinary $x64Bin -DestinationDir $bundleStageDir -StageParentDir $buildStageDir
        $pkgArm64 = Build-SingleMsix -TargetArch "aarch64" -TargetBinary $arm64Bin -DestinationDir $bundleStageDir -StageParentDir $buildStageDir

        # Move individual MSIX packages to OutputDir as well
        $finalX64Path = Join-Path $absOutputDir (Split-Path $pkgX64.Path -Leaf)
        $finalArm64Path = Join-Path $absOutputDir (Split-Path $pkgArm64.Path -Leaf)
        Copy-Item $pkgX64.Path $finalX64Path -Force
        Copy-Item $pkgArm64.Path $finalArm64Path -Force

        $builtPackages += $finalX64Path
        $builtPackages += $finalArm64Path

        # Create the unified .msixbundle
        $bundleFileName = "spotifast-v$Version-windows-universal.msixbundle"
        $bundlePath = Join-Path $absOutputDir $bundleFileName

        Write-Host "==> Creating universal bundle: $bundlePath"
        & $makeappx bundle /v /o /bv $quadVersion /d $bundleStageDir /p $bundlePath
        if ($LASTEXITCODE -ne 0) {
            throw "makeappx bundle failed with exit code $LASTEXITCODE"
        }

        Sign-Package -FilePath $bundlePath
        $builtPackages += $bundlePath

        if (-not $KeepStage) {
            Remove-Item -Recurse -Force $bundleStageDir -ErrorAction SilentlyContinue
            Remove-Item -Recurse -Force $buildStageDir -ErrorAction SilentlyContinue
        }
    }
}

# 7. Print Summary
Write-Host ""
Write-Host "============================================================"
Write-Host " Windows MSIX Packaging Complete"
Write-Host "============================================================"
foreach ($pkg in $builtPackages) {
    $sizeMb = (Get-Item $pkg).Length / 1MB
    $hash = (Get-FileHash -Path $pkg -Algorithm SHA256).Hash
    Write-Host (" {0,-45} {1,7:N2} MB  SHA256: {2}" -f (Split-Path $pkg -Leaf), $sizeMb, $hash.Substring(0, 16) + "...")
}
Write-Host "Note: Double-click installation of .msix and .msixbundle requires a signature."
Write-Host "Signing options (https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options):"
Write-Host " 1. Microsoft Store (Free): Microsoft re-signs MSIX packages automatically upon ingestion."
Write-Host " 2. SignPath Foundation (Free): Free code signing certificates for open-source projects."
Write-Host " 3. Azure Artifact Signing (~`$9.99/mo) or standard PFX certificate."
Write-Host " 4. Local testing without certs: Add-AppxPackage -Register <stageDir>\AppxManifest.xml"
Write-Host ""
