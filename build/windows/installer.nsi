; Instalador de Oruscribe Agente para Windows.
;
; Lo compila scripts/release.ps1 con makensis y estos defines:
;   /DVERSION=0.6.0
;   /DAPP_FILE=<ruta al exe compilado>
;   /DOUT_FILE=<ruta del instalador>
;   /DICON_FILE=<ruta del icono>
;
; Instala por usuario en %LOCALAPPDATA%\Programs\Oruscribe, sin pedir
; administrador. Crea los accesos del escritorio y del menú Inicio, escribe
; una sola entrada de Desinstalar y ofrece abrir la app al terminar.
;
; La clave canónica es la de NSIS (D201). La clave que dejó el instalador Go
; anterior se borra en la instalación y en la desinstalación para que Agregar
; o quitar programas muestre una sola entrada.

Unicode true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef APP_FILE
  !error "Falta /DAPP_FILE=<ruta al ejecutable>"
!endif
!ifndef OUT_FILE
  !error "Falta /DOUT_FILE=<ruta del instalador>"
!endif
!ifndef ICON_FILE
  !define ICON_FILE "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!endif

!define APP_NAME "Oruscribe Agente"
!define APP_EXE "Oruscribe Agente.exe"
!define COMPANY "Oruscribe"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Oruscribe Agente"
!define LEGACY_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\OruscribeAgente"
!define APP_KEY "Software\Oruscribe\Oruscribe Agente"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define AUTOSTART_VALUE "oruscribe-agente"

Name "${APP_NAME}"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Oruscribe"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
RequestExecutionLevel user
BrandingText "${APP_NAME} ${VERSION}"
ShowInstDetails show
ShowUninstDetails show

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "CompanyName" "${COMPANY}"
VIAddVersionKey "FileDescription" "Instalador de ${APP_NAME}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "© 2026, ${COMPANY}"

!define MUI_ABORTWARNING
!define MUI_ICON "${ICON_FILE}"
!define MUI_UNICON "${ICON_FILE}"
!define MUI_WELCOMEPAGE_TITLE "Instalar ${APP_NAME}"
!define MUI_WELCOMEPAGE_TEXT "Oruscribe Agente se instala en tu cuenta, sin pedir permiso de administrador.$\r$\n$\r$\nCrea los accesos del escritorio y del menú Inicio. Al terminar, puedes abrirlo desde aquí mismo."
!define MUI_FINISHPAGE_TITLE "${APP_NAME} está listo"
!define MUI_FINISHPAGE_TEXT "La app quedó instalada. Puedes abrirla ahora o buscarla en el menú Inicio."
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Abrir ${APP_NAME}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "Spanish"

; Si la app está abierta, Windows no deja reemplazar ni borrar el ejecutable.
; Se ofrece cerrarla antes de seguir; si la persona dice que no, no se toca
; nada. El mismo cierre corre al desinstalar.
;
; NSIS compila dos espacios de nombres: el instalador y el desinstalador. La
; función normal no se puede llamar desde la sección Uninstall; por eso el
; cuerpo vive en una macro y cada lado tiene su `Function` (`un.` incluido).
!macro CerrarAppCuerpo
  nsExec::ExecToStack '"$SYSDIR\tasklist.exe" /FI "IMAGENAME eq ${APP_EXE}" /NH /FO CSV'
  Pop $0
  Pop $1
  StrCpy $2 $1 22
  ${If} $2 == '"${APP_EXE}"'
    ; En silencio (`/S`, el QuietUninstallString) no hay nadie que conteste:
    ; se cierra sin preguntar. Con ventana, la persona decide.
    IfSilent cerrar
    MessageBox MB_YESNO|MB_ICONQUESTION "${APP_NAME} está abierto. Para continuar hay que cerrarlo. ¿Lo cierro ahora?" IDYES cerrar
    Abort
    cerrar:
      nsExec::Exec '"$SYSDIR\taskkill.exe" /IM "${APP_EXE}" /F'
      Sleep 1000
  ${EndIf}
!macroend

Function CerrarApp
  !insertmacro CerrarAppCuerpo
FunctionEnd

Function un.CerrarApp
  !insertmacro CerrarAppCuerpo
FunctionEnd

Function .onInit
  Call CerrarApp
  Call VerificarWebView2
FunctionEnd

; Sin el runtime Evergreen de WebView2 el Setup termina bien y después la app
; no abre. Se comprueba antes de copiar nada y se dice qué falta.
Function VerificarWebView2
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} $0 == ""
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
    ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
    MessageBox MB_ICONSTOP|MB_OK "${APP_NAME} necesita Microsoft Edge WebView2 Runtime y este equipo no lo tiene. Instálalo desde https://go.microsoft.com/fwlink/p/?LinkId=2124703 y vuelve a ejecutar este instalador."
    Abort
  ${EndIf}
FunctionEnd

Section "Instalar" SecInstall
  SetOutPath "$INSTDIR"
  ; release.ps1 copia el binario de Cargo como "Oruscribe Agente.exe"; NSIS
  ; conserva ese nombre, que es el que usan las rutas de orus-install.
  File "${APP_FILE}"
  ; El .old es la copia de rescate de una actualización fallida: no se borra
  ; al instalar encima. Solo se limpian los temporales de la copia atómica.
  Delete "$INSTDIR\${APP_EXE}.new"
  Delete "$INSTDIR\${APP_EXE}.nuevo"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortCut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
  CreateShortCut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0

  ; Una sola entrada: la canónica de NSIS. La de Go anterior se borra.
  DeleteRegKey HKCU "${LEGACY_UNINST_KEY}"
  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${COMPANY}"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE},0"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://app.oruscribe.com"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" 16000

  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Section "Uninstall"
  Call un.CerrarApp
  Delete "$DESKTOP\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\${APP_EXE}.old"
  Delete "$INSTDIR\${APP_EXE}.new"
  Delete "$INSTDIR\${APP_EXE}.nuevo"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$LOCALAPPDATA\Oruscribe\session.json"
  ; Las conversaciones son datos de la persona: desinstalar no las borra.
  ; El token de Windows vive en el Administrador de credenciales y este
  ; desinstalador no ejecuta el `--uninstall` de Rust, así que se borra aquí.
  nsExec::Exec '"$SYSDIR\cmdkey.exe" /delete:oruscribe-agent:session'
  nsExec::Exec '"$SYSDIR\cmdkey.exe" /delete:oruscribe-agent:agente-clave'
  RMDir /r "$INSTDIR"
  DeleteRegValue HKCU "${RUN_KEY}" "${AUTOSTART_VALUE}"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${LEGACY_UNINST_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd
