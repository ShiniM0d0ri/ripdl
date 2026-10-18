; ripdl Windows installer (Inno Setup 6).
; Version and runtime dir come from the release workflow:
;   ISCC /DAPP_VERSION=x.y.z packaging/windows/ripdl.iss
; Contents: ripdl (CLI + native-messaging host), ripdl-daemon (queue owner),
; ripdl-gui (UI shell; auto-spawns the daemon), ext/ (browser capture).

[Setup]
AppId=org.ripdl
AppName=ripdl
AppVersion={#APP_VERSION}
AppPublisher=ripdl
AppPublisherURL=https://github.com/ShiniM0d0ri/ripdl
DefaultDirName={autopf}\ripdl
DefaultGroupName=ripdl
DisableProgramGroupPage=yes
; Sources are relative to the script, but the release workflow runs ISCC
; from the repo root with the sources/target/dist one level up.
SourceDir=..\..
OutputDir=..\..\dist
OutputDir=dist
OutputBaseFilename=ripdl-v{#APP_VERSION}-x86_64-windows-setup
Compression=lzma2/max
SolidCompression=yes
Architectures=x64compatible
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop icon"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "target\release\ripdl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "target\release\ripdl-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "target\release\ripdl-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "ext\*"; DestDir: "{app}\ext"; Flags: ignoreversion recursesubdirs
Source: "LICENSE-MIT"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "LICENSE-APACHE"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\ripdl"; Filename: "{app}\ripdl-gui.exe"
Name: "{group}\ripdl command line"; Filename: "{app}\ripdl.exe"
Name: "{autodesktop}\ripdl"; Filename: "{app}\ripdl-gui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\ripdl-gui.exe"; Description: "Launch ripdl"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
; Nothing extra: the daemon's data dir (%LOCALAPPDATA%\ripdl) is user data and
; survives uninstalls on purpose.
