#define MyAppName "ashell"
#define MyAppVersion GetEnv("PACKAGE_VERSION")
#define MyAppArch GetEnv("ASHELL_ARCH")
#define MyAppExeSource GetEnv("ASHELL_BINARY_PATH")
#define MyOutputDir GetEnv("ASHELL_OUTPUT_DIR")
#define MyOutputBaseName GetEnv("PACKAGE_BASENAME")

#if MyAppVersion == ""
  #error PACKAGE_VERSION is required
#endif
#if MyAppExeSource == ""
  #error ASHELL_BINARY_PATH is required
#endif
#if MyOutputDir == ""
  #error ASHELL_OUTPUT_DIR is required
#endif
#if MyOutputBaseName == ""
  #error PACKAGE_BASENAME is required
#endif

#if MyAppArch == "x64"
  #define MyAppId "dev.ashell.app.x64"
#elif MyAppArch == "x86"
  #define MyAppId "dev.ashell.app.x86"
#elif MyAppArch == "arm64"
  #define MyAppId "dev.ashell.app.arm64"
#else
  #error Unsupported ASHELL_ARCH value
#endif

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=ashell contributors
AppPublisherURL=https://github.com/rust-kotlin/ashell
AppSupportURL=https://github.com/rust-kotlin/ashell/issues
AppUpdatesURL=https://github.com/rust-kotlin/ashell/releases
DefaultDirName={localappdata}\Programs\ashell
DefaultGroupName=ashell
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#MyOutputDir}
OutputBaseFilename={#MyOutputBaseName}-setup
SetupIconFile=..\..\assets\icons\ashell.ico
UninstallDisplayIcon={app}\ashell.exe
LicenseFile=..\..\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
MinVersion=10.0
CloseApplications=yes
RestartApplications=no

#if MyAppArch == "x64"
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
#elif MyAppArch == "x86"
ArchitecturesAllowed=x86compatible and not x64compatible and not arm64
ArchitecturesInstallIn64BitMode=
#elif MyAppArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#endif

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "zhcn"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#MyAppExeSource}"; DestDir: "{app}"; DestName: "ashell.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\ashell"; Filename: "{app}\ashell.exe"
Name: "{autodesktop}\ashell"; Filename: "{app}\ashell.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\ashell.exe"; Description: "{cm:LaunchProgram,ashell}"; Flags: nowait postinstall skipifsilent
