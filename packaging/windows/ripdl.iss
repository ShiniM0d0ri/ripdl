; ripdl Windows installer (Inno Setup 6).
; Version and runtime dir come from the release workflow:
;   ISCC /DAPP_VERSION=x.y.z packaging/windows/ripdl.iss
; Contents: ripdl (CLI + native-messaging host), ripdl-daemon (queue owner),
; ripdl-gui (UI shell; auto-spawns the daemon), ext/ (browser capture).
;
; Upgrade path: the AppId below is the install identity — it must NEVER
; change, or Setup would detect no previous installation. Running this
; installer over an existing ripdl replaces the binaries in place and keeps
; settings + queue (%LOCALAPPDATA%\ripdl) untouched.

[Setup]
AppId=org.ripdl
AppName=ripdl
AppVersion={#APP_VERSION}
AppPublisher=ripdl
AppPublisherURL=https://github.com/ShiniM0d0ri/ripdl
UninstallDisplayName=ripdl
UninstallDisplayIcon={app}\ripdl-gui.exe
DefaultDirName={autopf}\ripdl
DefaultGroupName=ripdl
DisableProgramGroupPage=yes
; Sources are relative to the script, but the release workflow runs ISCC
; from the repo root with the sources/target/dist one level up.
SourceDir=..\..
OutputDir=dist
OutputBaseFilename=ripdl-v{#APP_VERSION}-x86_64-windows-setup
Compression=lzma2/max
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
; Restart Manager closes a running GUI/daemon whenever possible instead of
; demanding a reboot; OpenFilesFromRM shows its name in the files-in-use
; list. PrepareToInstall + the uninstall step additionally taskkill as a
; belt-and-braces measure below.
CloseApplications=yes
RestartApplications=yes

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

[InstallDelete]
; A new release may rename or drop files under ext/; recopying over the old
; tree would leave stale scripts behind. Wipe it, then recopy, every time.
Type: filesandordirs; Name: "{app}\ext"

[Run]
; runasoriginaluser: Setup runs elevated (Program Files), but the app itself
; has no need for admin rights - after an in-app update the relaunched GUI
; used to inherit the elevation.
Filename: "{app}\ripdl-gui.exe"; Description: "Launch ripdl"; Flags: postinstall nowait skipifsilent runasoriginaluser

[UninstallRun]
; The native-messaging registrations below are deleted from Pascal code so
; foreign keys stay untouched; nothing else runs on uninstall.

[Code]
// Belt-and-braces process killer: the Restart Manager usually arranges the
// file locks on its own, but silent installs (/S) skip its prompt, so both
// the upgrade and the uninstall paths call this first. Harmless no-op when
// nothing is running.
//
// NEVER add /T here. When the update is app-driven, Setup is a *child* of
// ripdl-daemon.exe (the daemon ShellExecutes it), so a tree kill on the
// daemon terminates Setup itself mid-install: the daemon dies, the GUI
// survives, and nothing gets installed. The daemon stops its own sidecar
// children before handing over, so name-only kills are enough.
procedure KillRipdlProcesses;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'),
       '/F /IM ripdl-daemon.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'),
       '/F /IM ripdl-gui.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'),
       '/F /IM ripdl.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

// Uninstall-time native-messaging cleanup: delete the HKCU keys this
// install created (Chrome, Edge, Brave), but only when their manifest
// value really is ripdl's. A left-behind key pointing at a ripdl path in
// Program Files would break; a foreign key (another app's host under the
// same key name with a different manifest) must survive.
procedure RemoveNativeHostRegistrations;
var
  Subkeys: array[1..3] of String;
  I: Integer;
  Key, Manifest: String;
begin
  Subkeys[1] := 'Software\Google\Chrome\NativeMessagingHosts';
  Subkeys[2] := 'Software\Microsoft\Edge\NativeMessagingHosts';
  Subkeys[3] := 'Software\BraveSoftware\Brave\NativeMessagingHosts';
  for I := Low(Subkeys) to High(Subkeys) do
  begin
    { The default value is the manifest path; only delete ripdl's own
      registration, never a foreign host sharing the key name. }
    Key := Subkeys[I] + '\' + 'org.ripdl.host';
    if RegQueryStringValue(HKCU, Key, '', Manifest) then
      if Pos('ripdl', LowerCase(Manifest)) > 0 then
        RegDeleteKeyIncludingSubkeys(HKCU, Key);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    KillRipdlProcesses;
    RemoveNativeHostRegistrations;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // Runs right before files are copied: stop the daemon/GUI so an in-place
  // upgrade never trips over a locked exe.
  KillRipdlProcesses;
  Result := ''; // no error = keep going
end;
