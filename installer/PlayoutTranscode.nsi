; PlayoutTranscode NSIS Installer
; Simple portable deployment alternative to WiX MSI

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define PRODUCT_NAME "PlayoutTranscode"
!define PRODUCT_VERSION "1.0.0"
!define PRODUCT_PUBLISHER "PlayoutTranscode"

Name "${PRODUCT_NAME} ${PRODUCT_VERSION}"
OutFile "PlayoutTranscode-Setup.exe"
InstallDir "$PROGRAMFILES64\${PRODUCT_NAME}"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Section "Install"
    SetOutPath "$INSTDIR"

    File "..\dist\installer\PlayoutTranscode.exe"
    File "..\dist\installer\config.toml.example"
    File "..\dist\installer\install.ps1"

    SetOutPath "$INSTDIR\web-ui\dist"
    File /r "..\dist\installer\web-ui\dist\*.*"

    SetOutPath "$INSTDIR\Requirements\ffmpeg\bin"
    File "..\dist\installer\Requirements\ffmpeg\bin\ffmpeg.exe"
    File "..\dist\installer\Requirements\ffmpeg\bin\ffprobe.exe"
    File "..\dist\installer\Requirements\ffmpeg\bin\ffplay.exe"

    ; Desktop shortcut
    CreateShortCut "$DESKTOP\PlayoutTranscode.url" "http://127.0.0.1:4353"

    ; Start Menu
    CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
    CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\PlayoutTranscode Web UI.url" "http://127.0.0.1:4353"
    CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall.lnk" "$INSTDIR\uninstall.exe"

    ; Write uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"

    ; Register Windows Service via powershell
    nsExec::ExecToLog 'powershell.exe -ExecutionPolicy Bypass -File "$INSTDIR\install.ps1"'

    ; Registry for Add/Remove Programs
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "DisplayName" "${PRODUCT_NAME}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "UninstallString" "$INSTDIR\uninstall.exe"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "DisplayVersion" "${PRODUCT_VERSION}"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "Publisher" "${PRODUCT_PUBLISHER}"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "NoRepair" 1
    ${GetSize} "$INSTDIR" "/S=0K" $0
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" "EstimatedSize" $0
SectionEnd

Section "Uninstall"
    ; Stop and delete service
    nsExec::ExecToLog 'sc.exe stop PlayoutTranscode'
    nsExec::ExecToLog 'sc.exe delete PlayoutTranscode'

    ; Remove shortcuts
    Delete "$DESKTOP\PlayoutTranscode.url"
    RMDir /r "$SMPROGRAMS\${PRODUCT_NAME}"

    ; Remove install dir
    RMDir /r "$INSTDIR"

    ; Remove registry
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
SectionEnd
