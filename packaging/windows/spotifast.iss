; The Windows installer, built with Inno Setup 6.3 or later from a release
; binary (the release workflow does this on every tag):
;
;   iscc /DVersion=0.9.1 /DArch=x86_64 /DBinary=...\spotifast.exe ^
;        /DOutputDir=dist packaging\windows\spotifast.iss
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

#define AppName "Spotifast"
#define AppExeName "spotifast.exe"
#define AppIdentity "Spotifast"
#define LegacyBinary ExtractFileDir(Binary) + "\fastpotify.exe"

[Setup]
; Never change: this is how Windows tells an update from a new program.
AppId={{FCED1EA0-EBF5-4C32-BA3B-A3AD724BACC3}
AppName={#AppName}
AppVersion={#Version}
AppVerName={#AppName} {#Version}
AppPublisher=Carmine Paolino
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
OutputBaseFilename=spotifast-v{#Version}-{#Arch}-pc-windows-msvc-setup
SetupIconFile=spotifast.ico
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
#if Version == "0.9.1"
Source: "{#LegacyBinary}"; DestDir: "{app}"; Flags: ignoreversion
Source: "fastpotify-installer.txt"; DestDir: "{app}"; Flags: ignoreversion
#else
; The update helper relaunches the app under the name it was running as,
; and that is fastpotify.exe for an installation that reached 0.9.1 through
; an older updater. A copy under that name lets those updates finish.
Source: "{#Binary}"; DestDir: "{app}"; DestName: "fastpotify.exe"; Flags: ignoreversion
#endif
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "spotifast-installer.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[InstallDelete]
Type: files; Name: "{autoprograms}\Fastpotify.lnk"
Type: files; Name: "{autodesktop}\Fastpotify.lnk"
#if Version != "0.9.1"
Type: files; Name: "{app}\fastpotify-installer.txt"
#endif

[Registry]
; Retire only the old application's registrations, not the shared Spotify scheme.
Root: HKCU; Subkey: "Software\Classes\Fastpotify.spotify"; Flags: deletekey
Root: HKCU; Subkey: "Software\Fastpotify\Capabilities"; Flags: deletekey
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueName: "Fastpotify"; Flags: deletevalue
; Spotify links (spotify:track:…) open in Spotifast. Registered for this
; user only, like the program itself. The official client registers the same
; scheme when it is installed; whichever was set up last has the links, and
; Settings > Apps > Default apps can hand them to the other, where Spotifast
; is listed through the capabilities below.
Root: HKCU; Subkey: "Software\Classes\spotify"; ValueType: string; ValueName: ""; ValueData: "URL:Spotify link"
Root: HKCU; Subkey: "Software\Classes\spotify"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\spotify\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKCU; Subkey: "Software\Classes\spotify\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\Spotifast.spotify"; ValueType: string; ValueName: ""; ValueData: "URL:Spotify link"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Spotifast.spotify"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\Spotifast.spotify\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"",0"
Root: HKCU; Subkey: "Software\Classes\Spotifast.spotify\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""
Root: HKCU; Subkey: "Software\{#AppIdentity}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\{#AppIdentity}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "A native Spotify client"
Root: HKCU; Subkey: "Software\{#AppIdentity}\Capabilities\URLAssociations"; ValueType: string; ValueName: "spotify"; ValueData: "Spotifast.spotify"
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppIdentity}"; ValueData: "Software\{#AppIdentity}\Capabilities"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExeName}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
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
