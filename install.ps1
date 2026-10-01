# NioAI installer for Windows PowerShell
$ErrorActionPreference = "Stop"

$Repo = "nio-labs/nio"
$InstallDir = if ($env:NIO_INSTALL_DIR) { $env:NIO_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\Nio" }

Write-Host "📦 NioAI Windows Installer" -ForegroundColor Cyan

# 1. Detect architecture
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
switch ($Arch) {
    "X64"   { $TargetArch = "x86_64" }
    "Arm64" { $TargetArch = "aarch64" }
    Default {
        Write-Error "Unsupported architecture: $Arch"
        exit 1
    }
}

$Target = "$TargetArch-pc-windows-msvc"
$Archive = "nio-$Target.zip"
Write-Host "🔍 Detected target: $Target"

# 2. Setup paths
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

$TempDir = Join-Path $env:TEMP ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

try {
    $Version = if ($env:NIO_VERSION) { $env:NIO_VERSION } else { "latest" }
    if ($Version -eq "latest") {
        $DownloadUrl = "https://github.com/$Repo/releases/latest/download/$Archive"
        $ChecksumUrl = "https://github.com/$Repo/releases/latest/download/SHA256SUMS"
    } else {
        $DownloadUrl = "https://github.com/$Repo/releases/download/$Version/$Archive"
        $ChecksumUrl = "https://github.com/$Repo/releases/download/$Version/SHA256SUMS"
    }

    $ArchiveFile = Join-Path $TempDir $Archive
    Write-Host "⬇️  Downloading NioAI ($Version)..."
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $ArchiveFile

    # 3. Checksum check if available
    try {
        $ChecksumFile = Join-Path $TempDir "SHA256SUMS"
        Invoke-WebRequest -Uri $ChecksumUrl -OutFile $ChecksumFile -ErrorAction SilentlyContinue
        if (Test-Path $ChecksumFile) {
            Write-Host "🔒 Verifying checksum..."
            $Expected = Select-String -Path $ChecksumFile -Pattern $Archive | ForEach-Object { ($_ -split '\s+')[0] }
            if ($Expected) {
                $Actual = (Get-FileHash -Path $ArchiveFile -Algorithm SHA256).Hash.ToLower()
                if ($Actual -ne $Expected.ToLower()) {
                    Write-Error "Checksum verification failed! Expected $Expected but got $Actual."
                    exit 1
                }
            }
        }
    } catch {}

    # 4. Extract
    Write-Host "📂 Extracting archive..."
    Expand-Archive -Path $ArchiveFile -DestinationPath $TempDir -Force

    $SourceExe = Join-Path $TempDir "nio.exe"
    if (-not (Test-Path $SourceExe)) {
        Write-Error "Release archive did not contain 'nio.exe'."
        exit 1
    }

    $DestExe = Join-Path $InstallDir "nio.exe"
    Copy-Item -Path $SourceExe -Destination $DestExe -Force
    Write-Host "✅ Installed nio.exe to $DestExe" -ForegroundColor Green

    # 5. Path check
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
        Write-Host "📌 Added $InstallDir to user PATH." -ForegroundColor Yellow
        $env:PATH = "$env:PATH;$InstallDir"
    }

    if (Test-Path $DestExe) {
        & $DestExe --version
        Write-Host "🚀 Run 'nio' to start coding!" -ForegroundColor Green
    }
} finally {
    if (Test-Path $TempDir) {
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
