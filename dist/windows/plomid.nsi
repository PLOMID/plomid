; PLOMID Windows installer (NSIS 3).
; Built in CI: makensis /DVERSION=<tag> /DOUTDIR=<dir> dist/windows/plomid.nsi
; Expects dist/stage/plomid.exe + dist/stage/README.txt next to the sources.
; Unsigned: Windows SmartScreen will show an "unknown publisher" prompt on
; first run. To ship signed, sign dist/stage/plomid.exe beforehand (e.g.
; signtool with the WINDOWS_CERT secret) and the installer inherits it.

!include "MUI2.nsh"

!ifndef VERSION
  !error "VERSION must be defined (/DVERSION=v...)"
!endif
!ifndef OUTDIR
  !define OUTDIR "dist/artifacts"
!endif

Name "PLOMID ${VERSION}"
OutFile "${OUTDIR}\plomid-${VERSION}-windows-x64.exe"
InstallDir "$PROGRAMFILES64\PLOMID"
RequestExecutionLevel admin

!ifdef ICON
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!endif
!define MUI_WELCOMEPAGE_TITLE "Welcome to PLOMID ${VERSION}"
!define MUI_WELCOMEPAGE_TEXT "PLOMID is an enterprise universal database platform.$\r$\n$\r$\nThis installer places the PLOMID server (PostgreSQL wire protocol, default port 5432) under your Program Files and adds a Start Menu shortcut.$\r$\n$\r$\nDocs and downloads: https://plomid.in/"
!define MUI_FINISHPAGE_TEXT "PLOMID ${VERSION} is installed.$\r$\n$\r$\nStart it from the Start Menu, or run plomid.exe --help for flags.$\r$\n$\r$\nhttps://plomid.in/"
!define MUI_FINISHPAGE_LINK "PLOMID website"
!define MUI_FINISHPAGE_LINK_LOCATION "https://plomid.in/"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\stage\plomid.exe"
  File "..\stage\README.txt"
  File "..\stage\plomid-emblem-copper.png"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateDirectory "$SMPROGRAMS\PLOMID"
  CreateShortcut "$SMPROGRAMS\PLOMID\PLOMID.lnk" "$INSTDIR\plomid.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\plomid.exe"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\plomid-emblem-copper.png"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$SMPROGRAMS\PLOMID\PLOMID.lnk"
  RMDir "$SMPROGRAMS\PLOMID"
  RMDir "$INSTDIR"
SectionEnd
