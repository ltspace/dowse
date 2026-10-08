# Windows legacy-name migration

The Windows NSIS uninstall key is based on `productName`. Changing it from
`dowse-app` to `dowse` left the earlier installation and its startup entry behind,
even though the bundle identifier stayed `com.dowse.app`.

`path-hooks.nsh` runs `legacy-install.nsh` before installing new files. It reads
the current user's legacy uninstall registration, validates the uninstall command,
and invokes that uninstaller synchronously with `/S`. It deliberately does not
use `/UPDATE`, because the legacy shortcuts and startup entry must be removed.
Silent uninstall does not select Tauri's delete-app-data checkbox. No directory
is recursively deleted by the migration. Existing configuration, including
`autostart_user_disabled`, and index data keep their current paths.

The migration aborts on invalid registration, missing uninstaller, nonzero exit,
remaining application binaries or remaining uninstall registration. It does not
guess a directory or delete arbitrary files to repair an incomplete installation.
Only current-user installations are migrated, matching the shipped install mode.
The existing Tauri process check handles running GUI applications; CLI/MCP
processes are not terminated. A locked old CLI can therefore require the user to
close its consumer before retrying migration.

The desktop single-instance plugin is registered before other plugins. A second
launch shows the existing search window. Older releases do not implement that
protocol, so uninstall migration is still necessary.

## Automated check

Run on Windows with the Tauri NSIS toolchain installed:

```powershell
pwsh -NoProfile -File crates/dowse-app/src-tauri/windows/test-legacy-install.ps1
```

The script compiles and executes the production migration macro with fixture
uninstallers and GUID-scoped temporary files/registry keys. It covers clean
install, a custom path with spaces, coexistence, preserved files, repeat execution,
uninstall failure and invalid/missing uninstall commands. It does not launch the
desktop app or use the real installed application's registry keys.

## Release validation in a disposable Windows user or VM

Automated fixtures and compilation do not replace these installer checks:

- Install the released 1.0.0, set up a small index and change settings, then install
  the new build. Repeat with the old-name 1.1.0 and with both names installed.
- Repeat with a custom install directory and with autostart disabled in Dowse.
- Exercise upgrade while the GUI is running, cancellation of the close prompt,
  and silent installation. Ensure an unsuccessful migration does not install a
  second copy.
- Verify only `dowse` remains in Apps, shortcuts, user PATH and startup entries
  (or no startup entry when disabled), and that settings and search still work.
- Launch the new desktop executable twice: one GUI process and tray icon should
  remain, with the existing window shown. Reboot and verify the same condition.
- Reinstall the new build to verify the no-legacy path remains usable.

## Local real-installer verification (2026-10-08)

Tested on Windows 11 with the released `dowse-app_1.0.0_x64-setup.exe`
(SHA256 `8ca283d6aaf67241972e231ae3fed5dec658de490d85b729f645685962998ad3`)
and a release build containing this migration:

- Default legacy directory with old and new registrations coexisting and the
  legacy GUI running: upgrade removed the legacy binary, uninstall registration,
  startup entry and PATH entry. Configuration bytes and existing index segment
  files were preserved; the new CLI successfully searched a test index built by
  the released 1.0.0 CLI.
- A custom legacy path containing spaces with autostart disabled: upgrade removed
  the legacy installation and preserved the disabled preference through GUI startup.
- Repeated GUI launches left the original process running and the second process
  exited successfully. Starting the registered startup command after stopping the
  GUI succeeded. Original configuration was restored after testing.

This was a local-machine test, not VM isolation. Windows itself was **not rebooted**,
and tray icons were not counted visually. Login-after-reboot and interactive
installer cancellation remain separate manual checks.
