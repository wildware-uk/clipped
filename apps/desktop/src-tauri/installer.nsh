; Checks the installer makes before it copies anything.
;
; Hooked into Tauri's generated installer.nsi through
; `bundle > windows > nsis > installerHooks` in tauri.conf.json.
;
; WHY THIS EXISTS (issue #663). An NSIS installer whose File instructions all
; fail still reaches the end of its section and still exits 0. Measured on a
; real machine: the installer was run against an existing installation in
; C:\Program Files\Clipped without elevation, reported success, and changed
; nothing at all - the binaries on disk stayed five days old. Because the
; version had not moved either, the uninstall entry read the same before and
; after, so nothing available to the user said the update had not happened.
;
; That is worse than a failed install. A loud failure is a support question; a
; silent one is somebody concluding the application does not work, which is
; exactly what happened.
;
; Both obstacles are checked here, before anything is written, because a
; refusal that has already copied half a payload is not a refusal.

!macro NSIS_HOOK_PREINSTALL
  ; 1. Can this account write into the install directory at all?
  ;
  ; Tested by writing, not by reading an ACL: the question is whether the
  ; copies about to happen will succeed, and only an attempt answers that.
  ; CreateDirectory first because a fresh install has no directory yet, and a
  ; failure to create one is the same refusal for the same reason.
  CreateDirectory "$INSTDIR"
  ClearErrors
  FileOpen $0 "$INSTDIR\clipped-install-check.tmp" w
  IfErrors clipped_cannot_write
  FileClose $0
  Delete "$INSTDIR\clipped-install-check.tmp"

  ; 2. Is a previous Clipped still holding its own executables?
  ;
  ; Opening the file for writing is how a running process is detected here
  ; rather than by enumerating processes: NSIS cannot list processes without a
  ; plugin, and the thing that actually matters is whether this file can be
  ; replaced. Absent files are skipped - that is a first install.
  IfFileExists "$INSTDIR\clipped-recorder.exe" 0 clipped_check_desktop
  ClearErrors
  FileOpen $0 "$INSTDIR\clipped-recorder.exe" a
  IfErrors clipped_still_running
  FileClose $0

  clipped_check_desktop:
  IfFileExists "$INSTDIR\clipped-desktop.exe" 0 clipped_checks_passed
  ClearErrors
  FileOpen $0 "$INSTDIR\clipped-desktop.exe" a
  IfErrors clipped_still_running
  FileClose $0
  Goto clipped_checks_passed

  clipped_cannot_write:
    SetErrorLevel 1
    IfSilent +2
    MessageBox MB_ICONSTOP "Clipped cannot be installed into:$\r$\n$\r$\n    $INSTDIR$\r$\n$\r$\nThis account cannot write there. Run the installer as an administrator, or install into a folder you own.$\r$\n$\r$\nNothing has been changed."
    Abort "Cannot write to $INSTDIR. Nothing was installed. Run the installer as an administrator, or install into a folder you own."

  clipped_still_running:
    SetErrorLevel 1
    IfSilent +2
    MessageBox MB_ICONSTOP "Clipped is still running.$\r$\n$\r$\nClose the Clipped window and quit it from the notification area, then run this installer again.$\r$\n$\r$\nNothing has been changed."
    Abort "Clipped is still running, so its files cannot be replaced. Nothing was installed. Close Clipped and run this installer again."

  clipped_checks_passed:
!macroend
