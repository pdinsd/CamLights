; Inno Setup script for CamLights. Built by scripts\package.ps1, which passes /DAppVersion.
; Compatible with Inno Setup 6.0 and newer.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif

[Setup]
; Keep AppId stable across versions: it identifies the installation for upgrades and uninstall.
AppId={{8E36E58D-87E6-40DC-8A37-CA8FA4907C48}
AppName=CamLights
AppVersion={#AppVersion}
AppVerName=CamLights {#AppVersion}
AppPublisher=CamLights contributors
AppPublisherURL=https://github.com/pdinsd/CamLights
AppSupportURL=https://github.com/pdinsd/CamLights/issues
AppUpdatesURL=https://github.com/pdinsd/CamLights/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\CamLights
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=CamLights-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; The virtual camera needs Windows 11 (MFCreateVirtualCamera) on x64.
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
MinVersion=10.0.22000
; Registering the virtual camera writes to HKLM and Program Files.
PrivilegesRequired=admin
; CamLights holds this mutex while running; setup asks the user to close it first.
AppMutex=Local\CamLights.SingleInstance
UninstallDisplayIcon={app}\CamLights.exe
UninstallDisplayName=CamLights

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\CamLights.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\camlights_vcam.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\CamLights"; Filename: "{app}\CamLights.exe"
Name: "{autodesktop}\CamLights"; Filename: "{app}\CamLights.exe"; Tasks: desktopicon

[Run]
; Copies the DLL to Program Files\CamLights\VirtualCamera and registers "CamLights Camera".
Filename: "{app}\CamLights.exe"; Parameters: "--install-virtual-camera"; StatusMsg: "Installing the CamLights virtual camera..."; Flags: runhidden waituntilterminated
Filename: "{app}\CamLights.exe"; Description: "{cm:LaunchProgram,CamLights}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{app}\CamLights.exe"; Parameters: "--uninstall-virtual-camera"; Flags: runhidden waituntilterminated; RunOnceId: "UninstallVirtualCamera"

[UninstallDelete]
; Installed virtual camera copies; one still loaded by the camera service is removed on a later uninstall.
Type: filesandordirs; Name: "{commonpf64}\CamLights\VirtualCamera"
Type: dirifempty; Name: "{commonpf64}\CamLights"
