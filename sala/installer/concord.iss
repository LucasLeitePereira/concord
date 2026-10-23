; Concord installer (Inno Setup 6). Build with scripts\build-installer.ps1.
#define AppName "Concord"
#define AppVersion "0.1.4"

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
UninstallDisplayIcon={app}\concord.exe
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

Name: "installtailscale"; Description: "Baixar e instalar o Tailscale (necessário para conectar com amigos)"; Check: not TailscaleInstalled

[InstallDelete]
Type: files; Name: "{app}\sala-core.exe"
Type: files; Name: "{app}\sala-ui.exe"

[Files]
Source: "..\target\release\concord.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\concord-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\crates\ui\icons\icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Concord"; Filename: "{app}\concord.exe"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\Concord"; Filename: "{app}\concord.exe"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Concord"; ValueData: """{app}\concord.exe"""; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Concord"""; Flags: runhidden; StatusMsg: "Configurando o firewall..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Concord"" dir=in action=allow protocol=UDP localport=47800 program=""{app}\concord.exe"""; Flags: runhidden; StatusMsg: "Configurando o firewall..."
Filename: "{tmp}\tailscale-setup.exe"; StatusMsg: "Instalando o Tailscale..."; Flags: waituntilterminated skipifsilent; Tasks: installtailscale; Check: GotTailscaleSetup
Filename: "{app}\concord.exe"; Description: "Abrir o Concord"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Concord"""; Flags: runhidden; RunOnceId: "DelFirewallRule"

[Code]
procedure KillApp;
var
  rc: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM concord-ui.exe', '', SW_HIDE, ewWaitUntilTerminated, rc);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM sala-ui.exe', '', SW_HIDE, ewWaitUntilTerminated, rc);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM concord.exe', '', SW_HIDE, ewWaitUntilTerminated, rc);
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

var
  DownloadPage: TDownloadWizardPage;
  TailscaleDownloaded: Boolean;

function TailscaleInstalled: Boolean;
begin
  Result := FileExists(ExpandConstant('{commonpf}\Tailscale\tailscale.exe')) or
            FileExists(ExpandConstant('{commonpf32}\Tailscale\tailscale.exe'));
end;

function GotTailscaleSetup: Boolean;
begin
  Result := TailscaleDownloaded;
end;

function OnDownloadProgress(const Url, FileName: String; const Progress, ProgressMax: Int64): Boolean;
begin
  Result := True;
end;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage('Baixando o Tailscale',
    'Aguarde enquanto o instalador do Tailscale é baixado...', @OnDownloadProgress);
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = wpReady) and WizardIsTaskSelected('installtailscale') then
  begin
    TailscaleDownloaded := False;
    DownloadPage.Clear;
    DownloadPage.Add('https://pkgs.tailscale.com/stable/tailscale-setup-latest.exe', 'tailscale-setup.exe', '');
    DownloadPage.Show;
    try
      try
        DownloadPage.Download;
        TailscaleDownloaded := True;
      except
        SuppressibleMsgBox('Não foi possível baixar o Tailscale. O Concord será instalado mesmo assim; ' +
          'instale o Tailscale depois em https://tailscale.com/download.', mbInformation, MB_OK, IDOK);
      end;
    finally
      DownloadPage.Hide;
    end;
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
  begin
    WizardForm.FinishedLabel.Height := ScaleY(110);
    if TailscaleInstalled then
      WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 +
        'Abra o Tailscale, entre na sua conta e aceite o convite do seu amigo ' +
        '(ou convide-o) para ficarem na mesma rede.'
    else
      WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 +
        'O Concord usa o Tailscale para conectar você aos amigos. ' +
        'Instale em https://tailscale.com/download e entre na mesma rede do seu amigo.';
  end;
end;
