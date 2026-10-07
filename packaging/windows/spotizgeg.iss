; The Windows installer, built with Inno Setup 6.3 or later from a release
; binary (the release workflow does this on every tag):
;
;   iscc /DVersion=0.9.1 /DArch=x86_64 /DBinary=...\spotizgeg.exe ^
;        /DOutputDir=dist packaging\windows\spotizgeg.iss
;
; Arch is x86_64 or aarch64, as in the Rust target triple, so the installer
; is named like the zip next to it. It needs no administrator rights: the
; program goes to the user's own Programs folder with a Start menu entry,
; and a running copy is closed before an update replaces it.

#ifndef Version
  #error Version must be defined on the ISCC command line
#endif
#ifndef Arch
  #error Arch must be defined on the ISCC command line (x86_64 or aarch64)
#endif
#ifndef Binary
  #error Binary must be defined on the ISCC command line
#endif
#ifndef OutputDir
  #error OutputDir must be defined on the ISCC command line
#endif
#if Arch == "aarch64"
  #define InnoArch "arm64"
#else
  #define InnoArch "x64compatible"
#endif

#define AppName "Spotizgeg"
#define AppExeName "spotizgeg.exe"
#define AppIdentity "Spotizgeg"

[Setup]
; Never change: this is how Windows tells an update from a new program.
AppId={{FCED1EA0-EBF5-4C32-BA3B-A3AD724BACC3}
AppName={#AppName}
AppVersion={#Version}
AppVerName={#AppName} {#Version}
AppPublisher=Carmine Paolino
AppCopyright=© 2026 Carmine Paolino
AppPublisherURL=https://spotifast.rocks
AppSupportURL=https://github.com/crmne/spotifast/issues
AppUpdatesURL=https://spotifast.rocks/download/
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed={#InnoArch}
ArchitecturesInstallIn64BitMode={#InnoArch}
MinVersion=10.0
LicenseFile=..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=spotizgeg-v{#Version}-{#Arch}-pc-windows-msvc-setup
SetupIconFile=spotizgeg.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
UninstallDisplayIcon={app}\{#AppExeName}
; The file version has to be numbers: a release candidate's -rc1 comes off.
#define Dash Pos("-", Version)
#if Dash > 0
  #define NumericVersion Copy(Version, 1, Dash - 1)
#else
  #define NumericVersion Version
#endif
VersionInfoVersion={#NumericVersion}.0

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#Binary}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "spotizgeg-installer.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
; Spotify links (spotify:track:…) open in Spotizgeg. Registered for this
; user only, like the program itself. The official client registers the same
; scheme when it is installed; whichever was set up last has the links, and
; Settings > Apps > Default apps can hand them to the other, where Spotizgeg
; is listed through the capabilities below.
Root: HKCU; Subkey: "Software\Classes\spotify"; ValueType: string; ValueName: ""; ValueData: "URL:Spotify link"
Root: HKCU; Subkey: "Software\Classes\spotify"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\spotify\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKCU; Subkey: "Software\Classes\spotify\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\Spotizgeg.spotify"; ValueType: string; ValueName: ""; ValueData: "URL:Spotify link"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Spotizgeg.spotify"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\Spotizgeg.spotify\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKCU; Subkey: "Software\Classes\Spotizgeg.spotify\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""
Root: HKCU; Subkey: "Software\{#AppIdentity}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\{#AppIdentity}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "A native Spotify client"
Root: HKCU; Subkey: "Software\{#AppIdentity}\Capabilities\URLAssociations"; ValueType: string; ValueName: "spotify"; ValueData: "Spotizgeg.spotify"
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppIdentity}"; ValueData: "Software\{#AppIdentity}\Capabilities"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExeName}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
#if Arch == "x86_64"
// x64compatible lets this installer run on an ARM PC under emulation, and a
// copy installed from it keeps updating to x64. The ARM installer has
// everything this one has and runs natively, so point to it. A silent
// install (an update) goes ahead without asking.
function InitializeSetup(): Boolean;
begin
  Result := True;
  if IsArm64 and not WizardSilent then
    Result := MsgBox('This PC has an ARM processor. The ARM installer ' +
      '(aarch64) runs natively, faster and on less battery.' + #13#10#13#10 +
      'Install the version for Intel and AMD processors anyway?',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
end;
#endif

// The spotify: scheme key is shared with whatever else opens the links, so
// uninstalling takes it away only while it still names this program.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
  Exe: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if not RegQueryStringValue(HKCU, 'Software\Classes\spotify\shell\open\command', '', Command) then
    Exit;
  Exe := Lowercase(ExpandConstant('{app}\{#AppExeName}'));
  if Pos(Exe, Lowercase(Command)) > 0 then
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\spotify');
end;
