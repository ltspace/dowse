; PATH 钩子（issue #13）：装完把安装目录写进用户 PATH，让 `dowse` 命令
; （sidecar 打进来的 CLI）开箱可用；卸载时对应移除。
;
; 用 PowerShell 而不是 NSIS 原生字符串操作：一是绕开 NSIS 字符串长度上限
; 截断超长 PATH 的风险，二是 .NET 的 SetEnvironmentVariable(User) 写完会
; 自动广播 WM_SETTINGCHANGE，新开的终端立即能看到（已开着的终端要重开）。
; 写 HKCU 用户级 PATH，与 per-user 安装模式一致，不需要管理员权限。

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Adding $INSTDIR to user PATH"
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$$p=[Environment]::GetEnvironmentVariable($\'Path$\',$\'User$\'); $$l=@($$p -split $\';$\' | Where-Object {$$_}); if($$l -notcontains $\'$INSTDIR$\'){[Environment]::SetEnvironmentVariable($\'Path$\', (($$l+$\'$INSTDIR$\') -join $\';$\'), $\'User$\')}"'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing $INSTDIR from user PATH"
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$$p=[Environment]::GetEnvironmentVariable($\'Path$\',$\'User$\'); $$l=@($$p -split $\';$\' | Where-Object {$$_ -and $$_ -ne $\'$INSTDIR$\'}); [Environment]::SetEnvironmentVariable($\'Path$\', ($$l -join $\';$\'), $\'User$\')"'
!macroend
