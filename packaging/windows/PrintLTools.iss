#define MyAppName "PrintLTools"
#define MyAppPublisher "PrintLTools"
#define MyAppExeName "PrintLTools.exe"
#define MyAppDataDir "PrintLTools"
#define MyAppId "{{C234BD88-713C-4806-ADFE-6098C1639331}}"
#define StageRoot "..\..\dist\windows\staging"
#define OutputRoot "..\..\dist\windows\installer"

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\..\assets\windows\app-icon.ico
OutputDir={#OutputRoot}
#ifdef MyOutputBaseFilename
OutputBaseFilename={#MyOutputBaseFilename}
#else
OutputBaseFilename=PrintLTools-Setup-{#MyAppVersion}
#endif
UninstallDisplayIcon={app}\{#MyAppExeName}
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
UsedUserAreasWarning=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Dirs]
Name: "{userappdata}\{#MyAppDataDir}"

[Files]
Source: "{#StageRoot}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{userappdata}\{#MyAppDataDir}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{userappdata}\{#MyAppDataDir}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; WorkingDir: "{userappdata}\{#MyAppDataDir}"; Flags: nowait postinstall skipifsilent
