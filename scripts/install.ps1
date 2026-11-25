# Compila y deja Oruscribe Agente instalado por usuario, con sus accesos directos.
# Es el atajo para desarrollo; para una instalación de usuario se usa el
# instalador NSIS que deja scripts/release.ps1 en build\release.
#
#   pwsh -File scripts\install.ps1

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\Oruscribe'
$installExe = Join-Path $installDir 'Oruscribe Agente.exe'
$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
$desktop = [Environment]::GetFolderPath('Desktop')

Write-Host 'Compilando…'
Push-Location $repo
try {
    New-Item -ItemType Directory -Force -Path (Join-Path $repo 'build\bin') | Out-Null
    & cargo build --release --locked -p orus-shell
    if ($LASTEXITCODE -ne 0) { throw "cargo build falló con $LASTEXITCODE" }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    Copy-Item (Join-Path $target 'release\orus-shell.exe') (Join-Path $repo 'build\bin\Oruscribe Agente.exe') -Force
} finally {
    Pop-Location
}

$procesos = Get-Process -Name 'Oruscribe Agente' -ErrorAction SilentlyContinue
if ($procesos) {
    $procesos | Stop-Process -Force
    $procesos | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
# Copia atómica con respaldo: Copy-Item directo trunca el destino y, si falla
# a mitad, deja la app instalada sin arrancar. El .old es la vuelta atrás.
$origen = Join-Path $repo 'build\bin\Oruscribe Agente.exe'
$nuevo = "$installExe.nuevo"
Copy-Item $origen $nuevo -Force
if (-not (Test-Path -LiteralPath $nuevo) -or (Get-Item -LiteralPath $nuevo).Length -ne (Get-Item -LiteralPath $origen).Length) {
    throw "la copia quedó incompleta: $nuevo"
}
if (Test-Path -LiteralPath $installExe) {
    Copy-Item -LiteralPath $installExe -Destination "$installExe.old" -Force
}
Move-Item -Force $nuevo $installExe

$wsh = New-Object -ComObject WScript.Shell
foreach ($dir in @($startMenu, $desktop)) {
    $lnk = Join-Path $dir 'Oruscribe Agente.lnk'
    $sc = $wsh.CreateShortcut($lnk)
    $sc.TargetPath = $installExe
    $sc.WorkingDirectory = $installDir
    $sc.IconLocation = "$installExe,0"
    $sc.Description = 'Oruscribe Agente'
    $sc.Save()
}

$folderLnk = Join-Path $desktop 'Oruscribe Agente - codigo.lnk'
$sc = $wsh.CreateShortcut($folderLnk)
$sc.TargetPath = $repo
$sc.Description = 'Código fuente de Oruscribe Agente'
$sc.Save()

Write-Host "Instalado en $installExe"
Write-Host "Accesos: menú Inicio, escritorio y carpeta del código."
