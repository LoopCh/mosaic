; Mosaic installer hooks.
;
; The kill switch keeps persistent firewall filters (WFP) while it is in force. If Mosaic
; is uninstalled at that moment, lift them first — otherwise the PC would stay offline.
; Removing WFP filters needs administrator rights, hence "runas" (one UAC prompt, only
; when the kill switch is actually in force).

!macro NSIS_HOOK_PREUNINSTALL
  IfFileExists "$APPDATA\Mosaic\killswitch.armed" 0 mosaic_ks_done
    ExecShellWait "runas" "$INSTDIR\mosaic.exe" "--unblock"
  mosaic_ks_done:
!macroend
