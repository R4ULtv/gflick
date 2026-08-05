[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [Parameter(Mandatory = $true)]
    [string]$Target,
    [Parameter(Mandatory = $true)]
    [string]$Architecture,
    [Parameter(Mandatory = $true)]
    [string]$BinaryDir,
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,
    [string]$SettingsAppDir
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$') {
    throw "Version must be an unprefixed semantic version, for example 0.1.0"
}
if ($Target -ne "windows") {
    throw "This packaging script supports target 'windows' only"
}
if ($Architecture -ne "x86_64") {
    throw "This packaging script supports architecture 'x86_64' only"
}

$binaryRoot = (Resolve-Path -LiteralPath $BinaryDir).Path
$outputRoot = [IO.Path]::GetFullPath($OutputDir)
[IO.Directory]::CreateDirectory($outputRoot) | Out-Null
$settingsRoot = $null
if ($SettingsAppDir) {
    $settingsRootItem = Get-Item -LiteralPath $SettingsAppDir -Force
    if (-not $settingsRootItem.PSIsContainer) {
        throw "Settings application tree is not a directory: $SettingsAppDir"
    }
    if (($settingsRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Settings application tree root must not be a link or reparse point: $SettingsAppDir"
    }
    $settingsRoot = $settingsRootItem.FullName
}

$requiredBinaries = @(
    "gflick-setup.exe",
    "gflick-agent.exe",
    "gflick-tray.exe",
    "gflick.exe",
    "gflick-probe.exe",
    "gflick-bench.exe"
)
foreach ($name in $requiredBinaries) {
    $candidate = Join-Path $binaryRoot $name
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "Required release binary is missing: $candidate"
    }
}

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("gflick-package-" + [Guid]::NewGuid().ToString("N"))
$userStage = Join-Path $temporaryRoot "user"
$developerStage = Join-Path $temporaryRoot "devtools"

function Copy-ReleaseFile {
    param([string]$Name, [string]$Destination)
    $source = Join-Path $binaryRoot $Name
    $parent = Split-Path -Parent $Destination
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    Copy-Item -LiteralPath $source -Destination $Destination
}

function New-FileRecord {
    param(
        [string]$AbsolutePath,
        [string]$Source,
        [string]$Root,
        [string]$Destination,
        [bool]$Executable
    )
    $item = Get-Item -LiteralPath $AbsolutePath
    [ordered]@{
        source = $Source.Replace('\', '/')
        root = $Root
        destination = $Destination.Replace('\', '/')
        length = [long]$item.Length
        sha256 = (Get-FileHash -LiteralPath $AbsolutePath -Algorithm SHA256).Hash.ToLowerInvariant()
        executable = $Executable
    }
}

try {
    [IO.Directory]::CreateDirectory((Join-Path $userStage "payload")) | Out-Null
    [IO.Directory]::CreateDirectory($developerStage) | Out-Null

    $setupPath = Join-Path $userStage "gflick-setup.exe"
    $agentPath = Join-Path $userStage "payload/gflick-agent.exe"
    $trayPath = Join-Path $userStage "payload/gflick-tray.exe"
    $cliPath = Join-Path $userStage "payload/gflick.exe"
    Copy-ReleaseFile "gflick-setup.exe" $setupPath
    Copy-ReleaseFile "gflick-agent.exe" $agentPath
    Copy-ReleaseFile "gflick-tray.exe" $trayPath
    Copy-ReleaseFile "gflick.exe" $cliPath

    $components = [ordered]@{
        agent = [ordered]@{ files = @(
            (New-FileRecord $agentPath "payload/gflick-agent.exe" "private_bin" "gflick-agent.exe" $true)
        ) }
        tray = [ordered]@{ files = @(
            (New-FileRecord $trayPath "payload/gflick-tray.exe" "private_bin" "gflick-tray.exe" $true)
        ) }
        cli = [ordered]@{ files = @(
            (New-FileRecord $cliPath "payload/gflick.exe" "private_bin" "gflick.exe" $true)
        ) }
    }
    $defaults = @("agent", "tray")

    if ($settingsRoot) {
        $settingsFiles = @()
        $files = New-Object System.Collections.Generic.List[IO.FileInfo]
        $pendingDirectories = New-Object System.Collections.Generic.Stack[IO.DirectoryInfo]
        $pendingDirectories.Push((Get-Item -LiteralPath $settingsRoot -Force))
        while ($pendingDirectories.Count -gt 0) {
            $directory = $pendingDirectories.Pop()
            foreach ($item in Get-ChildItem -LiteralPath $directory.FullName -Force) {
                if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                    throw "Settings application tree must not contain links or reparse points: $($item.FullName)"
                }
                if ($item.PSIsContainer) {
                    $pendingDirectories.Push($item)
                }
                else {
                    $files.Add($item)
                }
            }
        }
        $files = @($files | Sort-Object FullName)
        if ($files.Count -eq 0) {
            throw "Settings application tree contains no regular files: $settingsRoot"
        }
        foreach ($file in $files) {
            $relative = $file.FullName.Substring($settingsRoot.Length).TrimStart('\', '/').Replace('\', '/')
            if ([string]::IsNullOrWhiteSpace($relative) -or $relative.Split('/') -contains '..') {
                throw "Invalid settings application path: $relative"
            }
            $staged = Join-Path $userStage ("payload/settings/" + $relative)
            [IO.Directory]::CreateDirectory((Split-Path -Parent $staged)) | Out-Null
            Copy-Item -LiteralPath $file.FullName -Destination $staged
            $stagedItem = Get-Item -LiteralPath $staged -Force
            $stagedItem.Attributes = $stagedItem.Attributes -band (-bnot [IO.FileAttributes]::Hidden) -band (-bnot [IO.FileAttributes]::System)
            $isExecutable = @(".exe", ".com", ".bat", ".cmd") -contains $file.Extension.ToLowerInvariant()
            $settingsFiles += New-FileRecord $staged ("payload/settings/" + $relative) "user_applications" $relative $isExecutable
        }
        $components.Add("settings", [ordered]@{ files = $settingsFiles })
        $defaults += "settings"
    }

    $manifest = [ordered]@{
        schema = 1
        product_version = $Version
        platform = "windows"
        arch = "x86_64"
        required = @("agent")
        defaults = $defaults
        setup = New-FileRecord $setupPath "gflick-setup.exe" "private_bin" "gflick-setup.exe" $true
        components = $components
    }
    $json = $manifest | ConvertTo-Json -Depth 12
    $utf8WithoutBom = New-Object Text.UTF8Encoding($false)
    [IO.File]::WriteAllText((Join-Path $userStage "bundle.json"), $json + [Environment]::NewLine, $utf8WithoutBom)

    & $setupPath verify-bundle $userStage
    if ($LASTEXITCODE -ne 0) {
        throw "gflick-setup verify-bundle failed with exit code $LASTEXITCODE"
    }

    Copy-ReleaseFile "gflick-probe.exe" (Join-Path $developerStage "gflick-probe.exe")
    Copy-ReleaseFile "gflick-bench.exe" (Join-Path $developerStage "gflick-bench.exe")
    $warning = @"
# GFlick developer tools

These tools are not part of the user installation. `gflick-probe` opens HID
devices directly and must not run at the same time as `gflick-agent`.
"@
    [IO.File]::WriteAllText((Join-Path $developerStage "README.md"), $warning.TrimStart() + [Environment]::NewLine, $utf8WithoutBom)

    $userArchiveName = "gflick-$Version-windows-x86_64.zip"
    $developerArchiveName = "gflick-devtools-$Version-windows-x86_64.zip"
    $userArchive = Join-Path $outputRoot $userArchiveName
    $developerArchive = Join-Path $outputRoot $developerArchiveName
    Compress-Archive -Path (Join-Path $userStage "*") -DestinationPath $userArchive -Force
    Compress-Archive -Path (Join-Path $developerStage "*") -DestinationPath $developerArchive -Force

    $fragment = Join-Path $outputRoot "SHA256SUMS-windows-x86_64"
    $lines = @(
        "{0}  {1}" -f (Get-FileHash -LiteralPath $userArchive -Algorithm SHA256).Hash.ToLowerInvariant(), $userArchiveName
        "{0}  {1}" -f (Get-FileHash -LiteralPath $developerArchive -Algorithm SHA256).Hash.ToLowerInvariant(), $developerArchiveName
    )
    [IO.File]::WriteAllText($fragment, ($lines -join "`n") + "`n", $utf8WithoutBom)
}
finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
