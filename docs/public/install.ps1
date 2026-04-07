#!/usr/bin/env pwsh

param(
    [string]$Version = "latest",
    [string]$Dir,
    [switch]$DryRun,
    [switch]$Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Owner = "m42e"
$Repo = "pw-env"
$BinaryName = "pw-env"

function Show-Usage {
    @"
Usage: install.ps1 [-Version <version>] [-Dir <install-dir>] [-DryRun]

Downloads the matching pw-env release archive for the current platform and
installs the binary.

Options:
  -Version <version>  Release version to install (e.g. 0.1.0 or v0.1.0).
                      Defaults to the latest GitHub release.
  -Dir <install-dir>  Destination directory for the binary.
                      Defaults to: $HOME/.local/bin on Unix-like systems,
                      and $env:USERPROFILE\bin on Windows.
  -DryRun             Print the resolved download URL and exit.
  -Help               Show this help text.

Environment:
  GITHUB_TOKEN        Optional token for GitHub API requests.
"@
}

function Normalize-Tag {
    param([Parameter(Mandatory = $true)][string]$InputTag)

    if ($InputTag -eq "latest") {
        return "latest"
    }
    if ($InputTag.StartsWith("v")) {
        return $InputTag
    }
    return "v$InputTag"
}

function Resolve-Latest-Tag {
    $apiUrl = "https://api.github.com/repos/$Owner/$Repo/releases/latest"
    $headers = @{
        "Accept" = "application/vnd.github+json"
        "X-GitHub-Api-Version" = "2022-11-28"
    }
    if ($env:GITHUB_TOKEN) {
        $headers["Authorization"] = "Bearer $($env:GITHUB_TOKEN)"
    }

    $response = Invoke-RestMethod -Uri $apiUrl -Headers $headers
    if (-not $response.tag_name) {
        throw "Unable to resolve the latest release tag"
    }
    return [string]$response.tag_name
}

function Get-TargetInfo {
    $osDescription = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
    $osPlatform = [System.Environment]::OSVersion.Platform
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture

    $target = $null
    $archiveFormat = $null
    $archiveBinaryName = $BinaryName

    if ($osPlatform -eq [System.PlatformID]::Win32NT -or $osDescription -match "Windows") {
        if ($arch -eq [System.Runtime.InteropServices.Architecture]::X64) {
            $target = "x86_64-pc-windows-msvc"
            $archiveFormat = "zip"
            $archiveBinaryName = "$BinaryName.exe"
        }
        else {
            throw "Unsupported Windows architecture: $arch"
        }
    }
    elseif ($osDescription -match "Darwin|macOS") {
        if ($arch -eq [System.Runtime.InteropServices.Architecture]::Arm64) {
            $target = "aarch64-apple-darwin"
            $archiveFormat = "tar.gz"
        }
        elseif ($arch -eq [System.Runtime.InteropServices.Architecture]::X64) {
            $target = "x86_64-apple-darwin"
            $archiveFormat = "tar.gz"
        }
        else {
            throw "Unsupported macOS architecture: $arch"
        }
    }
    elseif ($osDescription -match "Linux") {
        if ($arch -eq [System.Runtime.InteropServices.Architecture]::Arm64) {
            $target = "aarch64-unknown-linux-gnu"
            $archiveFormat = "tar.gz"
        }
        elseif ($arch -eq [System.Runtime.InteropServices.Architecture]::X64) {
            $target = "x86_64-unknown-linux-gnu"
            $archiveFormat = "tar.gz"
        }
        else {
            throw "Unsupported Linux architecture: $arch"
        }
    }
    else {
        throw "Unsupported operating system: $osDescription"
    }

    return [pscustomobject]@{
        Target = $target
        ArchiveFormat = $archiveFormat
        ArchiveBinaryName = $archiveBinaryName
    }
}

function Get-DefaultInstallDir {
    if ($Dir) {
        return $Dir
    }

    if ($IsWindows) {
        if ($env:USERPROFILE) {
            return (Join-Path $env:USERPROFILE "bin")
        }
        throw "USERPROFILE is not set and no install directory was provided"
    }

    if ($env:HOME) {
        return (Join-Path $env:HOME ".local/bin")
    }

    throw "HOME is not set and no install directory was provided"
}

function Expand-ArchiveFile {
    param(
        [Parameter(Mandatory = $true)][string]$ArchivePath,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$ArchiveFormat
    )

    if ($ArchiveFormat -eq "zip") {
        Expand-Archive -LiteralPath $ArchivePath -DestinationPath $Destination -Force
        return
    }

    if ($ArchiveFormat -eq "tar.gz") {
        & tar -xzf $ArchivePath -C $Destination
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to extract tar.gz archive"
        }
        return
    }

    throw "Unsupported archive format: $ArchiveFormat"
}

if ($Help) {
    Show-Usage
    exit 0
}

$targetInfo = Get-TargetInfo
$tag = Normalize-Tag -InputTag $Version
if ($tag -eq "latest") {
    $tag = Resolve-Latest-Tag
}

$archiveName = "$BinaryName-$tag-$($targetInfo.Target).$($targetInfo.ArchiveFormat)"
$downloadUrl = "https://github.com/$Owner/$Repo/releases/download/$tag/$archiveName"
$installDir = Get-DefaultInstallDir
$installPath = Join-Path $installDir $targetInfo.ArchiveBinaryName

if ($DryRun) {
    Write-Output "tag=$tag"
    Write-Output "target=$($targetInfo.Target)"
    Write-Output "archive=$archiveName"
    Write-Output "url=$downloadUrl"
    Write-Output "install_dir=$installDir"
    exit 0
}

$tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

try {
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null

    $archivePath = Join-Path $tmpDir $archiveName
    Write-Output "Downloading $archiveName..."
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath

    Write-Output "Installing $BinaryName to $installPath..."
    Expand-ArchiveFile -ArchivePath $archivePath -Destination $tmpDir -ArchiveFormat $targetInfo.ArchiveFormat

    $extractedBinary = Join-Path $tmpDir $targetInfo.ArchiveBinaryName
    if (-not (Test-Path -LiteralPath $extractedBinary -PathType Leaf)) {
        throw "Archive did not contain $($targetInfo.ArchiveBinaryName)"
    }

    Copy-Item -LiteralPath $extractedBinary -Destination $installPath -Force

    if (-not $IsWindows) {
        & chmod 755 $installPath
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to set executable permissions on $installPath"
        }
    }

    Write-Output "Installed $BinaryName $tag to $installPath"

    $pathEntries = ($env:PATH -split [System.IO.Path]::PathSeparator)
    if ($pathEntries -notcontains $installDir) {
        Write-Output ""
        Write-Output "Add $installDir to PATH if it is not already available in your shell session."
    }

    Write-Output ""
    Write-Output "To enable the pw-env shell hook, add the following to your shell config:"
    Write-Output ""
    if ($IsWindows) {
        Write-Output '  Invoke-Expression (& pw-env init powershell)'
        Write-Output ""
        Write-Output "  Add the line above to your PowerShell profile (`$PROFILE)."
    }
    else {
        Write-Output '  bash:  eval "$(pw-env init bash)"  # add to ~/.bashrc'
        Write-Output '  zsh:   eval "$(pw-env init zsh)"   # add to ~/.zshrc'
        Write-Output '  fish:  pw-env init fish | source   # add to ~/.config/fish/config.fish'
        Write-Output '  pwsh:  Invoke-Expression (& pw-env init powershell)  # add to $PROFILE'
    }
}
finally {
    if (Test-Path -LiteralPath $tmpDir) {
        Remove-Item -LiteralPath $tmpDir -Recurse -Force
    }
}
