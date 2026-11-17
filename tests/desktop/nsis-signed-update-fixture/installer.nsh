!macro NSIS_HOOK_PREINSTALL
  IfFileExists "$LOCALAPPDATA\@IDENTIFIER@\fixture-fail-install.flag" 0 +5
    FileOpen $R0 "$LOCALAPPDATA\@IDENTIFIER@\fixture-install-aborted.txt" w
    FileWrite $R0 "aborted-before-copy"
    FileClose $R0
    Abort
!macroend
