[CmdletBinding()]
param(
    [ValidateSet("major", "minor", "patch")]
    [string]$BumpType,

    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-TomlSection {
    param(
        [Parameter(Mandatory)]
        [string]$Content,

        [Parameter(Mandatory)]
        [string]$SectionName
    )

    $escapedSectionName = [regex]::Escape($SectionName)
    $pattern = "(?ms)^\[$escapedSectionName\]\s*\r?\n(?<body>.*?)(?=^\[|\z)"
    $section = [regex]::Match($Content, $pattern)
    if (-not $section.Success) {
        throw "Could not find [$SectionName] in Cargo.toml."
    }

    return $section
}

function Get-TomlStringValue {
    param(
        [Parameter(Mandatory)]
        [System.Text.RegularExpressions.Match]$Section,

        [Parameter(Mandatory)]
        [string]$Key
    )

    $escapedKey = [regex]::Escape($Key)
    $valueMatch = [regex]::Match(
        $Section.Groups["body"].Value,
        "(?m)^$escapedKey\s*=\s*`"(?<value>[^`"]+)`"\s*$"
    )
    if (-not $valueMatch.Success) {
        throw "Could not find '$Key' in [package]."
    }

    return $valueMatch
}

function Replace-MatchValue {
    param(
        [Parameter(Mandatory)]
        [string]$Content,

        [Parameter(Mandatory)]
        [int]$Index,

        [Parameter(Mandatory)]
        [int]$Length,

        [Parameter(Mandatory)]
        [string]$NewValue
    )

    return $Content.Substring(0, $Index) + $NewValue + $Content.Substring($Index + $Length)
}

function Get-NextVersion {
    param(
        [Parameter(Mandatory)]
        [string]$CurrentVersion,

        [Parameter(Mandatory)]
        [string]$RequestedBump
    )

    if ($CurrentVersion -notmatch '^(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)$') {
        throw "Package version '$CurrentVersion' is not a three-part semantic version."
    }

    $major = [int]$Matches["major"]
    $minor = [int]$Matches["minor"]
    $patch = [int]$Matches["patch"]

    switch ($RequestedBump.ToLowerInvariant()) {
        "major" {
            $major++
            $minor = 0
            $patch = 0
        }
        "minor" {
            $minor++
            $patch = 0
        }
        "patch" {
            $patch++
        }
    }

    return "$major.$minor.$patch"
}

function Update-CargoLock {
    param(
        [Parameter(Mandatory)]
        [string]$Content,

        [Parameter(Mandatory)]
        [string]$PackageName,

        [Parameter(Mandatory)]
        [string]$OldVersion,

        [Parameter(Mandatory)]
        [string]$NewVersion
    )

    $packageBlocks = [regex]::Matches(
        $Content,
        '(?ms)^\[\[package\]\]\s*\r?\n(?<body>.*?)(?=^\[\[package\]\]|\z)'
    )

    foreach ($block in $packageBlocks) {
        $body = $block.Groups["body"]
        $nameMatch = [regex]::Match($body.Value, '(?m)^name\s*=\s*"(?<value>[^"]+)"\s*$')
        if (-not $nameMatch.Success -or $nameMatch.Groups["value"].Value -ne $PackageName) {
            continue
        }

        $versionMatch = [regex]::Match($body.Value, '(?m)^version\s*=\s*"(?<value>[^"]+)"\s*$')
        if (-not $versionMatch.Success) {
            throw "Could not find the version for '$PackageName' in Cargo.lock."
        }
        if ($versionMatch.Groups["value"].Value -ne $OldVersion) {
            throw "Cargo.lock has version '$($versionMatch.Groups["value"].Value)' for '$PackageName', expected '$OldVersion'."
        }

        $versionGroup = $versionMatch.Groups["value"]
        $absoluteIndex = $body.Index + $versionGroup.Index
        return Replace-MatchValue $Content $absoluteIndex $versionGroup.Length $NewVersion
    }

    throw "Could not find package '$PackageName' in Cargo.lock."
}

if ([string]::IsNullOrWhiteSpace($BumpType)) {
    do {
        $BumpType = (Read-Host "What type of bump do you want to perform? [major/minor/patch]").Trim().ToLowerInvariant()
        if ($BumpType -notin @("major", "minor", "patch")) {
            Write-Warning "Please provide a valid version bump: major, minor, or patch."
            $BumpType = $null
        }
    } while ([string]::IsNullOrWhiteSpace($BumpType))
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$cargoTomlPath = Join-Path $repoRoot "Cargo.toml"
$cargoLockPath = Join-Path $repoRoot "Cargo.lock"

$cargoToml = [IO.File]::ReadAllText($cargoTomlPath)
$packageSection = Get-TomlSection $cargoToml "package"
$nameMatch = Get-TomlStringValue $packageSection "name"
$versionMatch = Get-TomlStringValue $packageSection "version"

$packageName = $nameMatch.Groups["value"].Value
$oldVersion = $versionMatch.Groups["value"].Value
$newVersion = Get-NextVersion $oldVersion $BumpType

$versionIndex = $packageSection.Groups["body"].Index + $versionMatch.Groups["value"].Index
$updatedCargoToml = Replace-MatchValue $cargoToml $versionIndex $versionMatch.Groups["value"].Length $newVersion

$cargoLock = [IO.File]::ReadAllText($cargoLockPath)
$updatedCargoLock = Update-CargoLock $cargoLock $packageName $oldVersion $newVersion

if ($DryRun) {
    Write-Host "$packageName $oldVersion -> $newVersion (dry run)"
    exit 0
}

$utf8WithoutBom = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText($cargoTomlPath, $updatedCargoToml, $utf8WithoutBom)
[IO.File]::WriteAllText($cargoLockPath, $updatedCargoLock, $utf8WithoutBom)

Write-Host "$packageName $oldVersion -> $newVersion"
