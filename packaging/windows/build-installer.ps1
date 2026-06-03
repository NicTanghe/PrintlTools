[CmdletBinding()]
param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [string]$InnoSetupCompiler,
    [string]$OutputBaseFilename,
    [switch]$SkipBuild,
    [switch]$SkipCompile,
    [switch]$Sign,
    [string]$SignToolPath,
    [string]$CertificateThumbprint,
    [string]$CertificateSubjectName,
    [string]$PfxPath,
    [string]$PfxPassword,
    [ValidateSet("CurrentUser", "LocalMachine")]
    [string]$CertificateStoreLocation = "CurrentUser",
    [ValidateSet("SHA256", "SHA384", "SHA512")]
    [string]$DigestAlgorithm = "SHA256",
    [string]$TimestampUrl = "http://timestamp.digicert.com"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-AppVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CargoTomlPath
    )

    $content = Get-Content -LiteralPath $CargoTomlPath -Raw
    if ($content -match '(?ms)^\[package\]\s*(.*?)^version\s*=\s*"([^"]+)"') {
        return $matches[2]
    }

    if ($content -match '(?ms)^\[workspace\.package\]\s*(.*?)^version\s*=\s*"([^"]+)"') {
        return $matches[2]
    }

    throw "Unable to read package version from $CargoTomlPath."
}

function Resolve-InnoSetupCompiler {
    param(
        [string]$PathHint
    )

    if ($PathHint) {
        return (Resolve-Path -LiteralPath $PathHint).Path
    }

    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
        (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
        (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe")
    ) | Where-Object { $_ }

    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) {
            return $candidate
        }
    }

    throw "ISCC.exe was not found. Install Inno Setup 6 or pass -InnoSetupCompiler."
}

function Resolve-SignTool {
    param(
        [string]$PathHint
    )

    if ($PathHint) {
        return (Resolve-Path -LiteralPath $PathHint).Path
    }

    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $kitsRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    if (Test-Path -LiteralPath $kitsRoot) {
        $candidate = Get-ChildItem -LiteralPath $kitsRoot -Directory |
            Sort-Object Name -Descending |
            ForEach-Object {
                @(
                    (Join-Path $_.FullName "x64\signtool.exe"),
                    (Join-Path $_.FullName "x86\signtool.exe")
                )
            } |
            Where-Object { Test-Path -LiteralPath $_ } |
            Select-Object -First 1

        if ($candidate) {
            return $candidate
        }
    }

    throw "signtool.exe was not found. Install the Windows SDK / Visual Studio signing tools or pass -SignToolPath."
}

function Get-SigningSelectorArgs {
    $selectorCount = 0
    if ($CertificateThumbprint) { $selectorCount++ }
    if ($CertificateSubjectName) { $selectorCount++ }
    if ($PfxPath) { $selectorCount++ }

    if ($selectorCount -eq 0) {
        throw "Signing requires one certificate selector: -CertificateThumbprint, -CertificateSubjectName, or -PfxPath."
    }

    if ($selectorCount -gt 1) {
        throw "Specify only one certificate selector for signing."
    }

    if ($PfxPath) {
        $resolvedPfxPath = (Resolve-Path -LiteralPath $PfxPath).Path
        $args = @("/f", $resolvedPfxPath)
        if (-not [string]::IsNullOrEmpty($PfxPassword)) {
            $args += @("/p", $PfxPassword)
        }

        return $args
    }

    $storeArgs = @("/s", "My")
    if ($CertificateStoreLocation -eq "LocalMachine") {
        $storeArgs += "/sm"
    }

    if ($CertificateThumbprint) {
        $normalizedThumbprint = ($CertificateThumbprint -replace "\s", "").ToUpperInvariant()
        return @("/sha1", $normalizedThumbprint) + $storeArgs
    }

    return @("/n", $CertificateSubjectName, "/a") + $storeArgs
}

function Get-SignableFiles {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $signableExtensions = @(".exe", ".dll", ".msi", ".ocx", ".cpl", ".scr", ".sys")

    Get-ChildItem -LiteralPath $Root -Recurse -File |
        Where-Object { $signableExtensions -contains $_.Extension.ToLowerInvariant() }
}

function Invoke-CodeSigning {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Paths,
        [Parameter(Mandatory = $true)]
        [string]$ToolPath
    )

    $selectorArgs = Get-SigningSelectorArgs
    $commonArgs = @(
        "sign",
        "/fd", $DigestAlgorithm,
        "/td", $DigestAlgorithm,
        "/tr", $TimestampUrl
    ) + $selectorArgs

    foreach ($path in $Paths) {
        & $ToolPath @commonArgs $path
        if ($LASTEXITCODE -ne 0) {
            throw "Code signing failed for $path with exit code $LASTEXITCODE."
        }
    }
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..\..")).Path
$distRoot = Join-Path $repoRoot "dist\windows"
$stageRoot = Join-Path $distRoot "staging"
$installerRoot = Join-Path $distRoot "installer"
$builtExe = Join-Path $repoRoot "target\$Configuration\printltools.exe"
$stagedExe = Join-Path $stageRoot "PrintLTools.exe"
$issPath = Join-Path $scriptDir "PrintLTools.iss"
$version = Get-AppVersion -CargoTomlPath (Join-Path $repoRoot "Cargo.toml")
$outputBaseName = if ($OutputBaseFilename) { $OutputBaseFilename } else { "PrintLTools-Setup-$version" }
$installerExe = Join-Path $installerRoot "$outputBaseName.exe"

if (-not $SkipBuild) {
    Push-Location $repoRoot
    try {
        if ($Configuration -eq "release") {
            cargo build --release
        }
        else {
            cargo build
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $builtExe)) {
    throw "Built executable not found at $builtExe."
}

if (Test-Path -LiteralPath $stageRoot) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force
}

New-Item -ItemType Directory -Path $stageRoot -Force | Out-Null
New-Item -ItemType Directory -Path $installerRoot -Force | Out-Null

Copy-Item -LiteralPath $builtExe -Destination $stagedExe -Force

if ($Sign) {
    $signTool = Resolve-SignTool -PathHint $SignToolPath
    $stageFilesToSign = @(Get-SignableFiles -Root $stageRoot | ForEach-Object { $_.FullName })
    if ($stageFilesToSign.Count -eq 0) {
        throw "No signable files were found under $stageRoot."
    }

    Invoke-CodeSigning -Paths $stageFilesToSign -ToolPath $signTool
}

if ($SkipCompile) {
    Write-Host "Staged installer payload in $stageRoot"
    return
}

$compiler = Resolve-InnoSetupCompiler -PathHint $InnoSetupCompiler
$compilerArgs = @("/DMyAppVersion=$version")
if ($OutputBaseFilename) {
    $compilerArgs += "/DMyOutputBaseFilename=$OutputBaseFilename"
}
$compilerArgs += $issPath

& $compiler @compilerArgs

if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup compilation failed with exit code $LASTEXITCODE."
}

if ($Sign) {
    if (-not (Test-Path -LiteralPath $installerExe)) {
        throw "Expected installer output was not found at $installerExe."
    }

    Invoke-CodeSigning -Paths @($installerExe) -ToolPath $signTool
}

Write-Host "Installer created in $installerRoot"
