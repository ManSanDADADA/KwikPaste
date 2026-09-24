<#
.SYNOPSIS
    Builds the Windows portable package from an exe produced by `tauri build`.

.DESCRIPTION
    Writes KwikPaste_<version>_<arch>_portable.zip containing
        KwikPaste/KwikPaste.exe
        KwikPaste/portable.txt
    The exe is the same binary the installer ships; the marker file next to it
    (name must match MARKER_FILENAME in src-tauri/src/core/portable.rs) switches
    the app into portable mode. The in-app updater extracts the only .exe entry.

    Used by the release workflows and the local packaging script. Kept ASCII-only
    so Windows PowerShell 5.1 reads it correctly without a BOM. Prints the zip path.

.EXAMPLE
    ./scripts/package-portable.ps1 -ExePath src-tauri/target/release/KwikPaste.exe -Version 1.4.0 -Arch x64 -OutDir release-local
#>
param(
    [Parameter(Mandatory = $true)]
    [string] $ExePath,

    [Parameter(Mandatory = $true)]
    [string] $Version,

    [Parameter(Mandatory = $true)]
    [ValidateSet('x64', 'arm64')]
    [string] $Arch,

    [Parameter(Mandatory = $true)]
    [string] $OutDir
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$repoRoot = Split-Path -Parent $PSScriptRoot
$marker = Join-Path $repoRoot 'src-tauri/assets/portable.txt'
$exe = (Resolve-Path -LiteralPath $ExePath).Path

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$zipPath = Join-Path (Resolve-Path -LiteralPath $OutDir).Path "KwikPaste_${Version}_${Arch}_portable.zip"
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}

# Entries are added one by one so their names always use '/', whichever PowerShell runs this.
$level = [System.IO.Compression.CompressionLevel]::Optimal
$zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $exe, 'KwikPaste/KwikPaste.exe', $level)
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $marker, 'KwikPaste/portable.txt', $level)
}
finally {
    $zip.Dispose()
}

Write-Output $zipPath
