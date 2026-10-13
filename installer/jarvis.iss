; Установщик Джарвиса (Inno Setup 6). Собирается скриптом scripts\build-installer.ps1:
; он кладёт файлы программы в папку Stage и передаёт её сюда (/DStage=... /DAppVersion=...).
;
; - Для текущего пользователя, без прав администратора: %LOCALAPPDATA%\Programs\Jarvis.
; - Обновление поверх: файлы программы заменяются, ваши — config\user.toml,
;   config\permissions.toml, data\, logs\, commands\my\ — не трогаются (их нет в установщике).
; - Удаление убирает только установленные файлы; ваши остаются в папке.

#ifndef Stage
  #error Запускайте через scripts\build-installer.ps1 (нужен /DStage=...)
#endif
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef OutputName
  #define OutputName "JarvisSetup"
#endif

[Setup]
AppId={{6F3C2A51-8E44-4B7B-9C1D-5A0E2B7D4F18}
AppName=Джарвис
AppVersion={#AppVersion}
AppVerName=Джарвис {#AppVersion}
AppPublisher=dstpnklen20-hue
DefaultDirName={localappdata}\Programs\Jarvis
DefaultGroupName=Джарвис
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\target\installer
OutputBaseFilename={#OutputName}
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayIcon={app}\jarvis.exe
UninstallDisplayName=Джарвис — голосовой ассистент
WizardStyle=modern
; Запущенный Джарвис останавливается в PrepareToInstall (ниже), а не через Restart Manager.
CloseApplications=no

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "autostart"; Description: "Запускать при входе в Windows"; GroupDescription: "Дополнительно:"
Name: "desktopicon"; Description: "Ярлык на рабочем столе"; GroupDescription: "Дополнительно:"; Flags: unchecked

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Джарвис"; Filename: "{app}\jarvisw.exe"; WorkingDir: "{app}"; Comment: "Голосовой ассистент: значок в трее"
Name: "{autoprograms}\Остановить Джарвиса"; Filename: "{app}\jarvisw.exe"; Parameters: "--stop"; WorkingDir: "{app}"
Name: "{autodesktop}\Джарвис"; Filename: "{app}\jarvisw.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; Тот же ключ и имя, что у пункта трея «Запускать при входе в Windows».
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Jarvis"; ValueData: """{app}\jarvisw.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\jarvisw.exe"; Description: "Запустить Джарвиса"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "{app}\jarvis.exe"; Parameters: "--stop"; Flags: runhidden waituntilterminated; RunOnceId: "StopJarvis"

[Code]
{ Перед установкой (и обновлением) — остановить работающего Джарвиса,
  иначе jarvis.exe занят и не заменится. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  Exe: String;
begin
  Result := '';
  Exe := ExpandConstant('{app}\jarvis.exe');
  if FileExists(Exe) then
  begin
    Exec(Exe, '--stop', '', SW_HIDE, ewWaitUntilTerminated, Code);
    Sleep(3000);
  end;
end;
