; The Windows setup program: installs ShellRS for the current user, with no
; administrator rights, under %LOCALAPPDATA%\Programs\ShellRS.
;
;   iscc /DAppVersion=0.2.0 /DBuildDir=..\..\target\release /O..\..\dist shellrs.iss
;
; The same program updates an installed copy: ShellRS downloads the next
; version's setup and runs it with /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
; /SP- once it has exited, then starts itself again. AppId must never
; change, or the update installs a second copy beside the first.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif
#ifndef BuildDir
  #define BuildDir "..\..\target\release"
#endif
#define IconFile AddBackslash(SourcePath) + "..\..\assets\logo\shellrs.ico"

[Setup]
AppId={{45D9E9F5-84BB-4B09-ABE8-1FFE107ACF96}
AppName=ShellRS
AppVersion={#AppVersion}
AppVerName=ShellRS {#AppVersion}
AppPublisher=ShellRS
AppPublisherURL=https://shellrs.com
AppSupportURL=https://shellrs.com
AppUpdatesURL=https://shellrs.com/download
DefaultDirName={autopf}\ShellRS
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputBaseFilename=ShellRS-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\shellrs.exe
UninstallDisplayName=ShellRS
; A running ShellRS is closed through the Restart Manager rather than
; making the setup fail on files in use.
CloseApplications=yes
#if FileExists(IconFile)
SetupIconFile={#IconFile}
#endif

[Languages]
#if FileExists(AddBackslash(CompilerPath) + "Languages\ChineseSimplified.isl")
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
#else
Name: "english"; MessagesFile: "compiler:Default.isl"
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BuildDir}\shellrs.exe"; DestDir: "{app}"; Flags: ignoreversion
; The `shellrs` command: 设置 › 外部 CLI copies it onto the PATH.
Source: "{#BuildDir}\shellrs-cli.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\ShellRS"; Filename: "{app}\shellrs.exe"
Name: "{autodesktop}\ShellRS"; Filename: "{app}\shellrs.exe"; Tasks: desktopicon

[Run]
; Interactive installs only: an update is restarted by ShellRS's own helper,
; which waits for this setup to finish.
Filename: "{app}\shellrs.exe"; Description: "{cm:LaunchProgram,ShellRS}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Files a later version added, which this uninstaller does not list.
Type: filesandordirs; Name: "{app}"
