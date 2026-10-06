; ==============================================================================
; isolmaSS NSIS Installer Script
; Produces: target\release\isolmass-setup.exe (Budget: <= 3 MB)
; ==============================================================================

!define PRODUCT_NAME "isolmaSS"
!ifndef PRODUCT_VERSION
  !error "PRODUCT_VERSION must be supplied by package.bat"
!endif
!ifndef PRODUCT_EXE_SOURCE
  !define PRODUCT_EXE_SOURCE "target\release\isolmass.exe"
!endif
!ifndef PRODUCT_OUTPUT
  !define PRODUCT_OUTPUT "target\release\isolmass-setup.exe"
!endif
!define PRODUCT_PUBLISHER "isolmaSS"
!define PRODUCT_WEB_SITE "https://github.com/isolmaz/isolmaSS-app"
!define PRODUCT_DIR_REGKEY "Software\Microsoft\Windows\CurrentVersion\App Paths\isolmass.exe"
!define PRODUCT_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
!define PRODUCT_UNINST_ROOT_KEY "HKCU"

RequestExecutionLevel user
ManifestDPIAware true
ManifestSupportedOS all
SetFont "Segoe UI" 10
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

Var Activated
Var HadPrevious
Var IsUpdate
Var WaitPid
Var ProcessHandle
Var WaitResult
Var PreviousVersion
Var Restored

; MUI Configuration
!define MUI_ABORTWARNING
!define MUI_BGCOLOR "F6F8FB"
!define MUI_TEXTCOLOR "1B212A"
!define MUI_WELCOMEPAGE_TITLE "A clearer way to capture"
!define MUI_WELCOMEPAGE_TEXT "Capture, annotate, and save in a few keystrokes.$\r$\n$\r$\nScreenshots stay local until you choose Upload and finish pairing your own Cloudflare account."
BrandingText "isolmaSS | Capture. Annotate. Done."
!define MUI_ICON "resources\app.ico"
!define MUI_UNICON "resources\app.ico"

; Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\isolmass.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch isolmaSS now"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; VIProductVersion requires exactly four numeric parts; the trailing ".0" is
; padding only. String versions below match the executable's three-part form.
VIProductVersion "${PRODUCT_VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${PRODUCT_VERSION}"
VIAddVersionKey /LANG=1033 "FileVersion" "${PRODUCT_VERSION}"
VIAddVersionKey /LANG=1033 "FileDescription" "isolmaSS Setup"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Copyright (c) 2026 isolmaSS contributors"
Name "${PRODUCT_NAME} ${PRODUCT_VERSION}"
OutFile "${PRODUCT_OUTPUT}"
InstallDir "$LOCALAPPDATA\isolmaSS"
InstallDirRegKey HKCU "${PRODUCT_DIR_REGKEY}" ""
ShowInstDetails show
ShowUnInstDetails show

!macro WaitForApplication PREFIX
Function ${PREFIX}WaitForApplication
  StrCpy $WaitPid ""
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/WAITPID=" $WaitPid
  ${If} $WaitPid == ""
    FindWindow $R2 "isolmaSS_TrayClass"
    ${If} $R2 != 0
      System::Call 'user32::GetWindowThreadProcessId(p $R2, *i .r3)'
      StrCpy $WaitPid $3
      ; Request graceful exit. The app defers exit while a capture/settings session is open.
      System::Call 'user32::PostMessageW(p $R2, i 0x0010, p 0, p 0)'
    ${EndIf}
  ${EndIf}
  ${If} $WaitPid != ""
    IntOp $WaitPid $WaitPid + 0
    ${If} $WaitPid <= 0
      MessageBox MB_ICONSTOP|MB_OK "The process identifier is invalid. Run setup again without a custom WAITPID argument." /SD IDOK
      SetErrorLevel 2
      Abort
    ${EndIf}
    System::Call 'kernel32::OpenProcess(i 0x100000, i 0, i $WaitPid) p .r4 ?e'
    Pop $5
    StrCpy $ProcessHandle $4
    ${If} $ProcessHandle == 0
      ${If} $5 != 87
        MessageBox MB_ICONSTOP|MB_OK "Setup could not verify that isolmaSS has closed. Close the application and retry. No installed files were changed." /SD IDOK
        SetErrorLevel 2
        Abort
      ${EndIf}
    ${EndIf}
    ${If} $ProcessHandle != 0
      System::Call 'kernel32::WaitForSingleObject(p r4, i 30000) i .r5'
      StrCpy $WaitResult $5
      System::Call 'kernel32::CloseHandle(p r4)'
      ${If} $WaitResult != 0
        MessageBox MB_ICONEXCLAMATION|MB_OK "Finish your capture and close isolmaSS, then run setup again. No installed files were changed." /SD IDOK
        SetErrorLevel 2
        Abort
      ${EndIf}
    ${EndIf}
  ${EndIf}
FunctionEnd
!macroend
!insertmacro WaitForApplication ""
!insertmacro WaitForApplication "un."

Function .onInit
  StrCpy $Activated "0"
  StrCpy $HadPrevious "0"
  StrCpy $IsUpdate "0"
  ReadRegStr $PreviousVersion HKCU "${PRODUCT_UNINST_KEY}" "DisplayVersion"
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $IsUpdate "1"
  ${EndIf}
  Call WaitForApplication
FunctionEnd

Function un.onInit
  Call un.WaitForApplication
FunctionEnd

Section "MainSection" SEC01
  SetOutPath "$INSTDIR"
  SetOverwrite on
  ; Stage first. A locked old executable must never leave a half-written installation.
  ClearErrors
  File /oname=isolmass.new.exe "${PRODUCT_EXE_SOURCE}"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "The new application could not be staged. The installed version was kept." /SD IDOK
    SetErrorLevel 3
    Abort
  ${EndIf}
  IfFileExists "$INSTDIR\isolmass.previous.exe" 0 +5
    Delete "$INSTDIR\isolmass.new.exe"
    MessageBox MB_ICONSTOP|MB_OK "A previous installation backup is still present. Restore it or contact support before retrying; no installed files were changed." /SD IDOK
    SetErrorLevel 4
    Abort
  IfFileExists "$INSTDIR\isolmass.exe" 0 activate_new
  StrCpy $HadPrevious "1"
  ClearErrors
  Rename "$INSTDIR\isolmass.exe" "$INSTDIR\isolmass.previous.exe"
  ${If} ${Errors}
    Delete "$INSTDIR\isolmass.new.exe"
    MessageBox MB_ICONSTOP|MB_OK "isolmaSS is still in use. Close it and retry. The installed version was kept." /SD IDOK
    SetErrorLevel 4
    Abort
  ${EndIf}
activate_new:
  ClearErrors
  Rename "$INSTDIR\isolmass.new.exe" "$INSTDIR\isolmass.exe"
  ${If} ${Errors}
    Rename "$INSTDIR\isolmass.previous.exe" "$INSTDIR\isolmass.exe"
    MessageBox MB_ICONSTOP|MB_OK "Setup could not activate the new version. The previous executable was restored when possible; retry setup." /SD IDOK
    SetErrorLevel 5
    Abort
  ${EndIf}
  StrCpy $Activated "1"
  ClearErrors
  File "LICENSE"
  File "THIRD_PARTY_NOTICES.md"

  ; Start Menu Shortcuts
  CreateDirectory "$SMPROGRAMS\isolmaSS"
  CreateShortcut "$SMPROGRAMS\isolmaSS\isolmaSS.lnk" "$INSTDIR\isolmass.exe"
  CreateShortcut "$SMPROGRAMS\isolmaSS\Uninstall.lnk" "$INSTDIR\uninstall.exe"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "Setup could not write the support files or shortcuts. Retry setup after checking folder access." /SD IDOK
    SetErrorLevel 7
    Abort
  ${EndIf}
SectionEnd

Section -AdditionalIcons
  ClearErrors
  WriteIniStr "$INSTDIR\${PRODUCT_NAME}.url" "InternetShortcut" "URL" "${PRODUCT_WEB_SITE}"
  CreateShortcut "$SMPROGRAMS\isolmaSS\Website.lnk" "$INSTDIR\${PRODUCT_NAME}.url"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "Setup could not write the website shortcut. Retry setup after checking folder access." /SD IDOK
    SetErrorLevel 7
    Abort
  ${EndIf}
SectionEnd

Section -Post
  ClearErrors
  WriteUninstaller "$INSTDIR\uninstall.exe"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "Setup could not write the uninstaller. Retry setup after checking folder access." /SD IDOK
    SetErrorLevel 7
    Abort
  ${EndIf}
  WriteRegStr HKCU "${PRODUCT_DIR_REGKEY}" "" "$INSTDIR\isolmass.exe"
  WriteRegStr ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}" "DisplayName" "$(^Name)"
  WriteRegStr ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}" "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  WriteRegStr ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}" "DisplayIcon" "$INSTDIR\isolmass.exe"
  WriteRegStr ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}" "URLInfoAbout" "${PRODUCT_WEB_SITE}"
  WriteRegStr ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"

  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "Setup could not finish registration. Retry setup to repair the application entry." /SD IDOK
    SetErrorLevel 7
    Abort
  ${EndIf}
  ; Prove the installed executable can initialize its tray and shortcut before
  ; deleting the rollback copy. Silent mode is never evidence of startup success.
  ${If} $HadPrevious == "1"
  ${OrIf} $IsUpdate == "1"
    ClearErrors
    ExecWait '"$INSTDIR\isolmass.exe" --health-check' $R7
    ${If} ${Errors}
    ${OrIf} $R7 != 0
      MessageBox MB_ICONSTOP|MB_OK "The installed version failed its startup check. Setup is restoring the previous executable." /SD IDOK
      SetErrorLevel 8
      Abort
    ${EndIf}
  ${EndIf}
  ${If} $IsUpdate == "1"
    ClearErrors
    Exec '"$INSTDIR\isolmass.exe"'
    ${If} ${Errors}
      MessageBox MB_ICONSTOP|MB_OK "The updated application could not start. Setup is restoring the previous executable." /SD IDOK
      SetErrorLevel 8
      Abort
    ${EndIf}
    StrCpy $R9 0
  wait_for_tray:
    FindWindow $R8 "isolmaSS_TrayClass"
    ${If} $R8 == 0
      Sleep 200
      IntOp $R9 $R9 + 1
      ${If} $R9 < 50
        Goto wait_for_tray
      ${EndIf}
      MessageBox MB_ICONSTOP|MB_OK "The updated application did not start its tray. Setup is restoring the previous executable." /SD IDOK
      SetErrorLevel 8
      Abort
    ${EndIf}
  ${EndIf}
  StrCpy $Activated "0"
  Delete "$INSTDIR\isolmass.previous.exe"
SectionEnd

Function .onInstFailed
  StrCpy $Restored "0"
  ${If} $Activated == "1"
    Delete "$INSTDIR\isolmass.exe"
    ${If} $HadPrevious == "1"
      ClearErrors
      Rename "$INSTDIR\isolmass.previous.exe" "$INSTDIR\isolmass.exe"
      ${If} ${Errors}
        MessageBox MB_ICONSTOP|MB_OK "The previous executable could not be restored automatically. It remains at $INSTDIR\isolmass.previous.exe. Close isolmaSS and retry setup." /SD IDOK
      ${Else}
        StrCpy $Restored "1"
        ${If} $PreviousVersion != ""
          WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayVersion" "$PreviousVersion"
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
  ${If} $IsUpdate == "1"
    System::Call 'user32::MessageBoxW(p 0, w "isolmaSS update failed. The previous version was restored if possible. Check installation permissions and try again.", w "isolmaSS update", i 0x10) i .r0'
    ${If} $Restored == "1"
      Exec '"$INSTDIR\isolmass.exe"'
    ${EndIf}
  ${EndIf}
FunctionEnd

Section Uninstall
  ClearErrors
  Delete "$INSTDIR\isolmass.exe"
  ${If} ${Errors}
    MessageBox MB_ICONSTOP|MB_OK "isolmaSS could not be removed. Close the application and retry. Its registration and shortcuts were kept." /SD IDOK
    SetErrorLevel 6
    Abort
  ${EndIf}
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\${PRODUCT_NAME}.url"
  Delete "$INSTDIR\uninstall.exe"

  Delete "$SMPROGRAMS\isolmaSS\Website.lnk"
  Delete "$SMPROGRAMS\isolmaSS\Uninstall.lnk"
  Delete "$SMPROGRAMS\isolmaSS\isolmaSS.lnk"
  RMDir "$SMPROGRAMS\isolmaSS"

  RMDir "$INSTDIR"

  DeleteRegKey ${PRODUCT_UNINST_ROOT_KEY} "${PRODUCT_UNINST_KEY}"
  DeleteRegKey HKCU "${PRODUCT_DIR_REGKEY}"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "isolmaSS"
  SetAutoClose true
SectionEnd
