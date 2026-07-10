!include "MUI2.nsh"
!ifndef VERSION
  !define VERSION "0.0.0"
!endif
Name "Perseus ${VERSION}"
OutFile "perseus-${VERSION}-windows-x64-setup.exe"
InstallDir "$PROGRAMFILES64\Perseus"
RequestExecutionLevel admin
!define MUI_ICON "..\icon.ico"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\perseus.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Perseus (tray)"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Perseus (required)" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "/oname=perseus.exe" "${EXE}"
  File "/oname=perseus.ico" "..\icon.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Perseus.lnk" "$INSTDIR\perseus.exe" "" "$INSTDIR\perseus.ico"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Perseus" "DisplayName" "Perseus"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Perseus" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Perseus" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
SectionEnd

Section "Start with Windows" SecAutostart
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Perseus" "$\"$INSTDIR\perseus.exe$\""
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\perseus.exe"
  Delete "$INSTDIR\perseus.ico"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$SMPROGRAMS\Perseus.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Perseus"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Perseus"
  RMDir "$INSTDIR"
  ; user data in %APPDATA%\Perseus is deliberately left in place
SectionEnd
