; v1.0.0 and early v1.1.0 used productName=dowse-app. NSIS keys
; depend on productName, not bundle identifier, so migrate explicitly.
!ifndef DOWSE_LEGACY_KEY
  !define DOWSE_LEGACY_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\dowse-app"
!endif
Var DowseLegacyDir
Var DowseLegacyUninstaller
Var DowseLegacyResult

!macro DOWSE_MIGRATE_LEGACY_INSTALL
  ReadRegStr $0 HKCU "${DOWSE_LEGACY_KEY}" "DisplayName"
  ReadRegStr $DowseLegacyDir HKCU "${DOWSE_LEGACY_KEY}" "InstallLocation"
  ReadRegStr $DowseLegacyUninstaller HKCU "${DOWSE_LEGACY_KEY}" "UninstallString"
  ${If} $0 != ""
  ${OrIf} $DowseLegacyDir != ""
  ${OrIf} $DowseLegacyUninstaller != ""
    ${If} $0 != "dowse-app"
      Goto dowse_legacy_failed
    ${EndIf}

    ; Tauri writes a quoted InstallLocation. Accept unquoted paths too.
    StrCpy $0 $DowseLegacyDir 1
    ${If} $0 == '$\"'
      StrCpy $0 $DowseLegacyDir 1 -1
      ${If} $0 != '$\"'
        Goto dowse_legacy_failed
      ${EndIf}
      StrCpy $DowseLegacyDir $DowseLegacyDir -1 1
    ${EndIf}
    ${If} $DowseLegacyDir == ""
      Goto dowse_legacy_failed
    ${EndIf}
    ; Do not execute an arbitrary registry command or guess a default path.
    ${If} $DowseLegacyUninstaller != '$\"$DowseLegacyDir\uninstall.exe$\"'
      Goto dowse_legacy_failed
    ${EndIf}
    ${IfNot} ${FileExists} "$DowseLegacyDir\uninstall.exe"
      Goto dowse_legacy_failed
    ${EndIf}

    ; Both generations have the same GUI executable name. Use Tauri's
    ; current-user process handling before the silent legacy uninstaller.
    ; The CLI/MCP executable (dowse.exe) is deliberately left alone.
    !insertmacro CheckIfAppIsRunning "dowse-app.exe" "dowse"
    DetailPrint "Removing the previous dowse-app installation (keeping user data)"
    ClearErrors
    ; _?= keeps NSIS in this process so ExecWait waits for the real uninstall.
    ; /S leaves the delete-app-data checkbox unchecked. Do not use /UPDATE:
    ; the old name's shortcuts and Run entry must also be removed.
    ExecWait '$\"$DowseLegacyDir\uninstall.exe$\" /S _?=$DowseLegacyDir' $DowseLegacyResult
    ${If} ${Errors}
      Goto dowse_legacy_failed
    ${EndIf}
    ${If} $DowseLegacyResult != 0
      Goto dowse_legacy_failed
    ${EndIf}
    ${If} ${FileExists} "$DowseLegacyDir\dowse-app.exe"
      Goto dowse_legacy_failed
    ${EndIf}
    ${If} ${FileExists} "$DowseLegacyDir\dowse.exe"
      Goto dowse_legacy_failed
    ${EndIf}
    ReadRegStr $0 HKCU "${DOWSE_LEGACY_KEY}" "UninstallString"
    ${If} $0 != ""
      Goto dowse_legacy_failed
    ${EndIf}
    ; _?= leaves the running uninstaller behind. Remove only that file;
    ; never recursively delete a directory that could contain user data.
    Delete "$DowseLegacyDir\uninstall.exe"
    RMDir "$DowseLegacyDir"
  ${EndIf}
  Goto dowse_legacy_done

  dowse_legacy_failed:
    SetErrorLevel 2
    DetailPrint "Could not safely remove the previous dowse-app installation."
    IfSilent +2
    MessageBox MB_OK|MB_ICONSTOP "Could not remove the previous dowse-app installation. Uninstall dowse-app from Windows Settings without deleting app data, then run this installer again."
    Abort
  dowse_legacy_done:
!macroend
