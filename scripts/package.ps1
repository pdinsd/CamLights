<#
.SYNOPSIS
    Builds a release of CamLights and packages it as dist\CamLights-<version>.zip.

.DESCRIPTION
    The package holds CamLights.exe and camlights_vcam.dll, which must stay side by side:
    the app installs the virtual camera from the DLL next to it. Release builds use the
    Windows GUI subsystem, so no console window opens.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build --release --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    $version = (cargo pkgid -p camlights) -replace '.*[#@]', ''
    $stage = Join-Path $root "dist\CamLights-$version"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null

    Copy-Item target\release\CamLights.exe, target\release\camlights_vcam.dll $stage
    @"
CamLights $version

Run CamLights.exe. To make "CamLights Camera" available to other apps, click
Install... under Virtual camera (asks for administrator approval once).

Keep camlights_vcam.dll next to CamLights.exe.
"@ | Set-Content -Path (Join-Path $stage 'README.txt') -Encoding utf8

    $zip = "$stage.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path "$stage\*" -DestinationPath $zip
    Write-Host "Packaged $zip"
}
finally {
    Pop-Location
}
