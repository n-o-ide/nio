# NioAI installer for Windows PowerShell
$ErrorActionPreference = "Stop"

$Repo = "nio-labs/nio"
$InstallDir = if ($env:NIO_INSTALL_DIR) { $env:NIO_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\Nio" }

Write-Host "==> NioAI Windows Installer" -ForegroundColor Cyan

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
Write-Host "==> Detected target: $Target"

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
    Write-Host "==> Downloading NioAI ($Version)..."
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $ArchiveFile -TimeoutSec 120

    # 3. Require one exact checksum entry; never continue after verification failure.
    $ChecksumFile = Join-Path $TempDir "SHA256SUMS"
    Invoke-WebRequest -Uri $ChecksumUrl -OutFile $ChecksumFile -TimeoutSec 120
    $Entries = @(Get-Content -LiteralPath $ChecksumFile | ForEach-Object {
        $Parts = $_.Trim() -split '\s+'
        if ($Parts.Count -eq 2 -and $Parts[1].TrimStart('*') -ceq $Archive) { $Parts[0] }
    })
    if ($Entries.Count -ne 1 -or $Entries[0] -notmatch '^[a-fA-F0-9]{64}$') {
        throw "Missing, invalid, or duplicate checksum for $Archive."
    }
    $Actual = (Get-FileHash -LiteralPath $ArchiveFile -Algorithm SHA256).Hash
    if ($Actual -ne $Entries[0]) { throw "Checksum verification failed!" }

    # 4. Extract
    Write-Host "==> Extracting archive..."
    Expand-Archive -Path $ArchiveFile -DestinationPath $TempDir -Force

    $SourceExe = Join-Path $TempDir "nio.exe"
    if (-not (Test-Path $SourceExe)) {
        Write-Error "Release archive did not contain 'nio.exe'."
        exit 1
    }

    $DestExe = Join-Path $InstallDir "nio.exe"
    if (Test-Path -LiteralPath $DestExe) {
        $Existing = & $DestExe --version
        if ($LASTEXITCODE -ne 0 -or $Existing -notmatch '^nio \S+ \(NioAI\)$') {
            throw "Refusing to replace an unrelated nio executable. Choose NIO_INSTALL_DIR."
        }
    }
    $Downloaded = & $SourceExe --version
    if ($LASTEXITCODE -ne 0 -or $Downloaded -notmatch '^nio \S+ \(NioAI\)$') {
        throw "Downloaded executable is not NioAI."
    }
    if ($Version -ne "latest" -and $Downloaded -cne "nio $($Version.TrimStart('v')) (NioAI)") {
        throw "Downloaded executable has the wrong version."
    }
    Copy-Item -LiteralPath $SourceExe -Destination $DestExe -Force
    Write-Host "==> Installed nio.exe to $DestExe" -ForegroundColor Green

    # 5. Path check
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
        Write-Host "==> Added $InstallDir to user PATH." -ForegroundColor Yellow
        $env:PATH = "$env:PATH;$InstallDir"
    }

    if (Test-Path $DestExe) {
        & $DestExe --version
        Write-Host "Run 'nio' to start coding!" -ForegroundColor Green
    }
} finally {
    if (Test-Path $TempDir) {
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
