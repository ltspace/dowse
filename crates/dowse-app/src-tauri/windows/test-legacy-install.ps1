param(
    [string]$MakeNsis = "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe"
)

# Executes the production migration macro with real NSIS fixture uninstallers,
# an isolated registry key and temporary files. Never touches installed Dowse.
$ErrorActionPreference = 'Stop'
$testId = [guid]::NewGuid().ToString('N')
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "dowse-migration-$testId"
$registryKey = "Software\DowseMigrationTests\$testId"
$registryPath = "HKCU:\$registryKey"
$legacyDir = Join-Path $testRoot 'custom old installation'
$includePath = Join-Path $PSScriptRoot 'legacy-install.nsh'

function Compile-Nsis([string]$Name, [string]$Source) {
    $path = Join-Path $testRoot "$Name.nsi"
    [IO.File]::WriteAllText($path, $Source)
    & $MakeNsis /V2 $path
    if ($LASTEXITCODE -ne 0) { throw "NSIS compilation failed: $Name" }
}

function Run-Fixture([string]$Name, [int]$ExpectedExit = 0) {
    $process = Start-Process (Join-Path $testRoot "$Name.exe") -ArgumentList '/S' -WindowStyle Hidden -PassThru
    if (!$process.WaitForExit(60000)) {
        $process.Kill()
        $process.WaitForExit()
        throw "$Name timed out"
    }
    if ($process.ExitCode -ne $ExpectedExit) {
        throw "$Name returned $($process.ExitCode); expected $ExpectedExit"
    }
}

function Register-Legacy {
    New-Item -Path $registryPath -Force | Out-Null
    Set-ItemProperty $registryPath DisplayName 'dowse-app'
    Set-ItemProperty $registryPath InstallLocation "`"$legacyDir`""
    Set-ItemProperty $registryPath UninstallString "`"$legacyDir\uninstall.exe`""
    Set-Content (Join-Path $legacyDir 'dowse-app.exe') 'fixture executable'
}

try {
    New-Item -ItemType Directory -Path $legacyDir -Force | Out-Null
    Compile-Nsis 'migrate' (@'
Unicode true
RequestExecutionLevel user
SilentInstall silent
!include LogicLib.nsh
!include FileFunc.nsh
!define DOWSE_LEGACY_KEY "@KEY@"
!macro CheckIfAppIsRunning executableName productName
; No GUI process is launched by these isolated fixtures.
!macroend
!include "@INCLUDE@"
OutFile "@ROOT@\migrate.exe"
Section
  !insertmacro DOWSE_MIGRATE_LEGACY_INSTALL
SectionEnd
'@.Replace('@KEY@', $registryKey).Replace('@INCLUDE@', $includePath).Replace('@ROOT@', $testRoot))

    foreach ($variant in @('success', 'failure', 'incomplete')) {
        $body = if ($variant -eq 'success') {
            'Delete "$INSTDIR\dowse-app.exe"' + "`n" + 'DeleteRegKey HKCU "' + $registryKey + '"'
        } elseif ($variant -eq 'failure') { 'SetErrorLevel 7' }
        else { 'SetErrorLevel 0' }
        Compile-Nsis $variant (@'
Unicode true
RequestExecutionLevel user
SilentInstall silent
OutFile "@ROOT@\@VARIANT@.exe"
InstallDir "@OLD@"
Section
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd
Section Uninstall
  @BODY@
SectionEnd
'@.Replace('@ROOT@', $testRoot).Replace('@VARIANT@', $variant).Replace('@OLD@', $legacyDir).Replace('@BODY@', $body))
    }

    Run-Fixture 'migrate'
    Write-Host 'PASS: clean install without legacy registration'

    # Include user data in the old directory and a coexisting new executable.
    $data = Join-Path $legacyDir 'user-data.txt'
    $newExe = Join-Path $testRoot 'new-dowse.exe'
    Set-Content $data 'keep config and index'
    Set-Content $newExe 'keep new installation'
    Register-Legacy
    Run-Fixture 'success'
    Run-Fixture 'migrate'
    if ((Test-Path $registryPath) -or (Test-Path "$legacyDir\dowse-app.exe") -or (Test-Path "$legacyDir\uninstall.exe")) {
        throw 'Legacy installation was not removed'
    }
    if ((Get-Content $data) -ne 'keep config and index' -or (Get-Content $newExe) -ne 'keep new installation') {
        throw 'Migration changed preserved files'
    }
    Run-Fixture 'migrate'
    Write-Host 'PASS: custom path with spaces, preserved data/new installation, repeat migration'

    Register-Legacy
    Run-Fixture 'failure'
    Run-Fixture 'migrate' 2
    if (!(Test-Path "$legacyDir\dowse-app.exe")) { throw 'Failed uninstall removed the fixture' }
    Write-Host 'PASS: failed uninstall blocks installation'

    Run-Fixture 'incomplete'
    Run-Fixture 'migrate' 2
    Write-Host 'PASS: zero exit without removing legacy installation rejected'

    Run-Fixture 'success'
    Set-Content (Join-Path $legacyDir 'dowse.exe') 'locked or leftover CLI'
    Run-Fixture 'migrate' 2
    if (!(Test-Path "$legacyDir\dowse.exe")) { throw 'Migration deleted the leftover CLI' }
    Remove-Item -LiteralPath "$legacyDir\dowse.exe"
    Write-Host 'PASS: leftover CLI blocks installation without deleting it'
    Register-Legacy

    Set-ItemProperty $registryPath UninstallString 'unexpected-command.exe'
    Run-Fixture 'migrate' 2
    Write-Host 'PASS: unexpected uninstall command rejected'

    Set-ItemProperty $registryPath UninstallString "`"$legacyDir\uninstall.exe`""
    Remove-Item -LiteralPath "$legacyDir\uninstall.exe"
    Run-Fixture 'migrate' 2
    Write-Host 'PASS: missing uninstaller blocks installation'
} finally {
    # Only remove the GUID-scoped fixtures created by this invocation.
    if ($registryPath -ne "HKCU:\Software\DowseMigrationTests\$testId") { throw 'Unsafe registry cleanup target' }
    if (Test-Path $registryPath) { Remove-Item -LiteralPath $registryPath -Recurse -Force }
    $expectedRoot = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) "dowse-migration-$testId"))
    if ([IO.Path]::GetFullPath($testRoot) -ne $expectedRoot) { throw 'Unsafe fixture cleanup target' }
    if (Test-Path $testRoot) { Remove-Item -LiteralPath $testRoot -Recurse -Force }
}
