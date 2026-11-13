; The Windows setup program (Inno Setup 6.3 or newer).
;
; Built by the release workflow (which names the desktop app PrzmTerm.exe):
;   iscc /DAppVersion=1.0.0 /DNumericVersion=1.0.0 /DBinDir=<folder of the
;        two .exe> /DRepoDir=<checkout> /DOutputDir=<where the setup goes>
;        packaging\windows\przmterm.iss
;
; It installs for everyone (Program Files, needs an administrator) or for
; the current user only (%LOCALAPPDATA%\Programs, no administrator): setup
; asks, and /ALLUSERS or /CURRENTUSER choose on the command line. An update
; installs the way the copy it replaces was installed.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef RepoDir
  #define RepoDir "..\.."
#endif
#ifndef BinDir
  #define BinDir RepoDir + "\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; Never change: Windows knows the installed copy by it.
AppId={{F7AD3D21-FD04-43AA-8D9B-C4E2522AB106}
AppName=PrzmTerm
AppVersion={#AppVersion}
AppVerName=PrzmTerm {#AppVersion}
AppPublisher=Rodrigo Prazim
AppPublisherURL=https://github.com/rodrigoprazim/przmterm
AppSupportURL=https://github.com/rodrigoprazim/przmterm/issues
AppUpdatesURL=https://github.com/rodrigoprazim/przmterm/releases
VersionInfoVersion={#NumericVersion}
VersionInfoProductName=PrzmTerm
VersionInfoDescription=PrzmTerm setup
; For the current user unless an administrator chooses everyone.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
DefaultDirName={autopf}\PrzmTerm
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; PrzmTerm running: closed to be replaced.
CloseApplications=yes
RestartApplications=no
ChangesEnvironment=yes
SetupIconFile={#RepoDir}\assets\icon.ico
UninstallDisplayIcon={app}\PrzmTerm.exe
UninstallDisplayName=PrzmTerm
WizardStyle=modern
; Drawn by scripts/installer-art.py; setup picks the size for the screen.
WizardImageFile={#RepoDir}\packaging\windows\wizard-image-100.bmp,{#RepoDir}\packaging\windows\wizard-image-150.bmp,{#RepoDir}\packaging\windows\wizard-image-200.bmp
WizardSmallImageFile={#RepoDir}\packaging\windows\wizard-small-100.bmp,{#RepoDir}\packaging\windows\wizard-small-150.bmp,{#RepoDir}\packaging\windows\wizard-small-200.bmp
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=przmterm-{#AppVersion}-windows-x86_64-setup

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ptbr"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[CustomMessages]
en.AddToPath=Add przm (the command line) to the PATH
ptbr.AddToPath=Adicionar o przm (linha de comando) ao PATH

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "{cm:AddToPath}"

[Files]
Source: "{#BinDir}\PrzmTerm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\przm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoDir}\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoDir}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoDir}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\PrzmTerm"; Filename: "{app}\PrzmTerm.exe"
Name: "{autodesktop}\PrzmTerm"; Filename: "{app}\PrzmTerm.exe"; Tasks: desktopicon

[Registry]
; HKA: the machine's PATH for everyone, the user's PATH for one user.
Root: HKA; Subkey: "{code:EnvKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\PrzmTerm.exe"; Description: "{cm:LaunchProgram,PrzmTerm}"; Flags: nowait postinstall skipifsilent runasoriginaluser
; An update from PrzmTerm itself runs silently: start the new version.
Filename: "{app}\PrzmTerm.exe"; Flags: nowait runasoriginaluser; Check: WizardSilent

[Code]
function EnvKey(Param: String): String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function EnvRoot: Integer;
begin
  if IsAdminInstallMode then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

{ Where Dir is in the PATH, counting from the ';' put in front of it; 0 if
  it is not there. }
function PathPos(Dir, Paths: String): Integer;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';');
end;

function NeedsAddPath(Dir: String): Boolean;
var
  Paths: String;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey(''), 'Path', Paths) then
    Result := True
  else
    Result := PathPos(Dir, Paths) = 0;
end;

procedure RemoveFromPath(Dir: String);
var
  Paths: String;
  P: Integer;
begin
  if not RegQueryStringValue(EnvRoot, EnvKey(''), 'Path', Paths) then
    exit;
  P := PathPos(Dir, Paths);
  if P = 0 then
    exit;
  if P = 1 then
    Delete(Paths, 1, Length(Dir) + 1)
  else
    Delete(Paths, P - 1, Length(Dir) + 1);
  RegWriteExpandStringValue(EnvRoot, EnvKey(''), 'Path', Paths);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
