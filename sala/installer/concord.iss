; Concord installer (Inno Setup 6). Build with scripts\build-installer.ps1.
#define AppName "Concord"
#define AppVersion "0.1.0"

[Setup]
AppId={{B7E2C1A4-5D3F-4C8E-9A61-2F0D7C4E8B15}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Concord
DefaultDirName={autopf}\Concord
DefaultGroupName=Concord
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
UninstallDisplayIcon={app}\sala-core.exe
UninstallDisplayName={#AppName}
SetupIconFile=..\crates\ui\icons\icon.ico
OutputDir=..\target\installer
OutputBaseFilename=Concord-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=no
UsedUserAreasWarning=no

[Languages]
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "desktopicon"; Description: "Criar atalho na Área de Trabalho"; Flags: unchecked
Name: "startup"; Description: "Iniciar o Concord com o Windows"; Flags: unchecked

[Files]
Source: "..\target\release\sala-core.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\sala-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\crates\ui\icons\icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Concord"; Filename: "{app}\sala-core.exe"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\Concord"; Filename: "{app}\sala-core.exe"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Concord"; ValueData: """{app}\sala-core.exe"""; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Concord"""; Flags: runhidden; StatusMsg: "Configurando o firewall..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Concord"" dir=in action=allow protocol=UDP localport=47800 program=""{app}\sala-core.exe"""; Flags: runhidden; StatusMsg: "Configurando o firewall..."
Filename: "{app}\sala-core.exe"; Description: "Abrir o Concord"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Concord"""; Flags: runhidden; RunOnceId: "DelFirewallRule"

[Code]
procedure KillApp;
var
  rc: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM sala-ui.exe', '', SW_HIDE, ewWaitUntilTerminated, rc);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM sala-core.exe', '', SW_HIDE, ewWaitUntilTerminated, rc);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  KillApp;
  Result := '';
end;

function InitializeUninstall: Boolean;
begin
  KillApp;
  Result := True;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and
     not FileExists(ExpandConstant('{commonpf}\Tailscale\tailscale.exe')) and
     not FileExists(ExpandConstant('{commonpf32}\Tailscale\tailscale.exe')) then
  begin
    WizardForm.FinishedLabel.Height := ScaleY(110);
    WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 +
      'O Concord usa o Tailscale para conectar você aos amigos. ' +
      'Instale em https://tailscale.com/download e entre na mesma rede do seu amigo.';
  end;
end;
