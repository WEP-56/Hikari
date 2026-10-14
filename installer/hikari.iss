; Hikari Windows 安装包（Inno Setup 6）
;
; 版本号由 tag 注入：ISCC.exe /DAppVersion=0.1.0 installer\hikari.iss
; 未注入时（本地手工编译）回退到下面的默认值。
;
; 本地验证：
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\hikari.iss
; 产物输出到 target\setup\。

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

#define AppName "Hikari"
#define AppExe "hikari.exe"
#define AppPublisher "WEP"
#define AppUrl "https://github.com/WEP-56/Hikari"

[Setup]
; AppId 是安装包的固定身份，升级/卸载依赖它，不要修改。
AppId={{9E51C911-46E3-48DE-A3CD-68E70B048EE8}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#AppExe}
LicenseFile=..\License
OutputDir=..\target\setup
OutputBaseFilename=Hikari-{#AppVersion}-windows-x64-setup
SetupIconFile=hikari.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; 只支持 64 位 Windows；默认按用户安装（免管理员），也允许用户改成整机安装。
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\License"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
