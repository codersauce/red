[CmdletBinding()]
param(
    [string]$Version = $env:RED_VERSION,
    [string]$InstallDir = $env:RED_INSTALL_DIR,
    [switch]$NoModifyPath,
    [string]$ReleasesUrl = $env:RED_RELEASES_URL
)

$ErrorActionPreference = "Stop"

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "Red currently supports only 64-bit Windows."
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = "latest"
}
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\Red\bin"
}
if ([string]::IsNullOrWhiteSpace($ReleasesUrl)) {
    $ReleasesUrl = "https://github.com/codersauce/red/releases"
}

$archive = "red-x86_64-pc-windows-msvc.zip"
if ($Version -eq "latest") {
    $downloadBase = "$ReleasesUrl/latest/download"
} else {
    $tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
    $downloadBase = "$ReleasesUrl/download/$tag"
}

$tempDir = Join-Path ([IO.Path]::GetTempPath()) ("red-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tempDir | Out-Null

try {
    $archivePath = Join-Path $tempDir $archive
    $checksumsPath = Join-Path $tempDir "SHA256SUMS.txt"

    Write-Host "Downloading Red $Version for x86_64-pc-windows-msvc..."
    Invoke-WebRequest -UseBasicParsing -Uri "$downloadBase/$archive" -OutFile $archivePath
    Invoke-WebRequest -UseBasicParsing -Uri "$downloadBase/SHA256SUMS.txt" -OutFile $checksumsPath

    $escapedArchive = [regex]::Escape($archive)
    $checksumLine = Get-Content $checksumsPath |
        Where-Object { $_ -match "^([a-fA-F0-9]{64})\s+\*?$escapedArchive$" } |
        Select-Object -First 1
    if (-not $checksumLine) {
        throw "The release did not publish a checksum for $archive."
    }

    $expectedChecksum = ([regex]::Match(
        $checksumLine,
        "^([a-fA-F0-9]{64})"
    )).Groups[1].Value.ToLowerInvariant()
    $actualChecksum = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLowerInvariant()
    if ($actualChecksum -ne $expectedChecksum) {
        throw "Checksum mismatch for $archive."
    }

    $extractDir = Join-Path $tempDir "extracted"
    Expand-Archive -Path $archivePath -DestinationPath $extractDir
    $sourceBinary = Join-Path $extractDir "red.exe"
    if (-not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
        throw "The release archive did not contain red.exe."
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $destination = Join-Path $InstallDir "red.exe"
    $stagedBinary = Join-Path $InstallDir (".red.install." + [guid]::NewGuid() + ".exe")
    Copy-Item -LiteralPath $sourceBinary -Destination $stagedBinary
    try {
        Move-Item -LiteralPath $stagedBinary -Destination $destination -Force
    } catch {
        Remove-Item -LiteralPath $stagedBinary -Force -ErrorAction SilentlyContinue
        throw "Could not replace $destination. Close any running Red process and try again."
    }

    if (-not $NoModifyPath) {
        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        $entries = @($userPath -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if (-not ($entries | Where-Object { $_.TrimEnd("\") -ieq $InstallDir.TrimEnd("\") })) {
            $newUserPath = (($entries + $InstallDir) -join ";")
            [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
        }
        if (-not (($env:Path -split ";") | Where-Object { $_.TrimEnd("\") -ieq $InstallDir.TrimEnd("\") })) {
            $env:Path = "$InstallDir;$env:Path"
        }
    }

    & $destination --version
    if ($LASTEXITCODE -ne 0) {
        throw "red --version exited with code $LASTEXITCODE."
    }
    $env:NO_COLOR = "1"
    & $destination --self-check
    if ($LASTEXITCODE -ne 0) {
        throw "red --self-check exited with code $LASTEXITCODE."
    }

    Write-Host ""
    Write-Host "Red is installed at $destination."
    if ($NoModifyPath) {
        Write-Host "Add $InstallDir to PATH to run red from any directory."
    } else {
        Write-Host "Open a new terminal if the red command is not available yet."
    }
    Write-Host "Agent support is optional: install Codex CLI, then run codex login."
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
