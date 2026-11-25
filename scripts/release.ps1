# Compila la release de Windows del shell en Rust, arma el instalador NSIS y
# deja el manifiesto firmado que consulta el updater.
#
#   pwsh -File scripts\release.ps1 -Version 0.6.3 -Notes "Qué cambia"
#
# Deja en build\release\:
#   Oruscribe Agente.exe            el programa (Windows)
#   Oruscribe-Agente-Setup-*.exe    el instalador de Windows (NSIS)
#   latest.json                     el manifiesto
#   latest.json.sig                 firma Ed25519, solo si ORUSCRIBE_SIGNING_KEY
#   SHA256.txt                      las huellas, para comparar a mano
#
# La versión de $Version tiene que coincidir con [workspace.package].version:
# el updater compara contra la versión compilada y una discrepancia ofrece una
# actualización falsa. El binario de Cargo se copia como "Oruscribe Agente.exe"
# porque ese es el nombre que usan orus-install, NSIS y el registro.
#
# El binario de Linux lo compila el job `linux` del CI y se baja como artefacto.
# Pásalo con -LinuxArchive para que el manifiesto ofrezca la actualización en
# los dos sistemas. Windows se compila aquí porque hace falta NSIS.
#
# El instalador necesita makensis (NSIS 3.11 o superior) en el PATH, en
# MAKENSIS, o en la carpeta normal de instalación de NSIS. La firma usa la
# herramienta Rust `signmanifest` de `orus-install` (D203): no se duplica el
# firmante.

param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$BaseUrl = "https://github.com/JuanMS20/oruscribe-agente-descargas/releases/download/v$Version",
    [string]$Notes = '',
    [string]$LinuxArchive = '',
    [string]$VerificarAssets = '',
    [switch]$SoloVerificar,
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$out = Join-Path $repo 'build\release'
$exe = Join-Path $out 'Oruscribe Agente.exe'

# Busca makensis sin depender de que esté en el PATH. MAKENSIS gana para
# pruebas y para una instalación portable.
function Find-Makensis {
    if ($env:MAKENSIS -and (Test-Path -LiteralPath $env:MAKENSIS -PathType Leaf)) {
        return $env:MAKENSIS
    }
    $cmd = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $bases = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, (Join-Path $env:LOCALAPPDATA 'Programs'))
    foreach ($base in $bases) {
        if (-not $base) { continue }
        $p = Join-Path $base 'NSIS\makensis.exe'
        if (Test-Path -LiteralPath $p -PathType Leaf) { return $p }
    }
    return $null
}

# La versión que ve el updater sale de Cargo, no del parámetro. Si no
# coinciden, se corta antes de compilar.
function Get-WorkspaceVersion {
    $texto = Get-Content (Join-Path $repo 'Cargo.toml') -Raw
    $bloque = [regex]::Match($texto, '(?ms)^\[workspace\.package\].*?(?=^\[)')
    if (-not $bloque.Success) { throw 'Cargo.toml no tiene [workspace.package]' }
    $version = [regex]::Match($bloque.Value, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $version.Success) { throw 'Cargo.toml no declara la versión del workspace' }
    return $version.Groups[1].Value
}

function Test-VersionDelArbolLinux {
    param([string]$Archivo, [string]$Esperada)

    $nombre = Split-Path $Archivo -Leaf
    $nombreEsperado = "oruscribe-agente-$Esperada-linux-x86_64.tar.gz"
    if ($nombre -ne $nombreEsperado) {
        throw "El tar.gz se llama $nombre; el manifiesto $Esperada solo puede apuntar a $nombreEsperado"
    }
    if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
        throw 'No hay tar para leer VERSION.txt del tar.gz; no se publica el manifiesto'
    }
    $contenido = (@(& tar -xOf $Archivo VERSION.txt 2>$null) -join "`n").Trim()
    if ($LASTEXITCODE -ne 0 -or $contenido -ne $Esperada) {
        throw "VERSION.txt del tar.gz dice '$contenido'; el manifiesto dice $Esperada; no se publica"
    }
}

function Test-HuellasPublicadas {
    param([string]$Directorio, [string]$Version)

    $archivoHuellas = Join-Path $Directorio 'SHA256.txt'
    if (-not (Test-Path -LiteralPath $archivoHuellas -PathType Leaf)) {
        throw "No hay SHA256.txt en $Directorio"
    }
    $esperadas = @{}
    foreach ($linea in (Get-Content -LiteralPath $archivoHuellas)) {
        if ($linea -match '^\s*(\S+)\s+([0-9a-f]{64})\s*$') {
            $esperadas[$Matches[1]] = $Matches[2]
        }
    }
    $assets = [ordered]@{
        windows   = 'Oruscribe.Agente.exe'
        installer = "Oruscribe-Agente-Setup-$Version.exe"
        linux     = "oruscribe-agente-$Version-linux-x86_64.tar.gz"
    }
    foreach ($etiqueta in $assets.Keys) {
        if (-not $esperadas.Contains($etiqueta)) {
            throw "SHA256.txt no declara la huella de $etiqueta"
        }
        $ruta = Join-Path $Directorio $assets[$etiqueta]
        if (-not (Test-Path -LiteralPath $ruta -PathType Leaf)) {
            throw "Falta el asset $ruta en $Directorio"
        }
        $real = (Get-FileHash -LiteralPath $ruta -Algorithm SHA256).Hash.ToLower()
        if ($real -ne $esperadas[$etiqueta]) {
            throw "${etiqueta}: SHA256.txt dice $($esperadas[$etiqueta]) pero el asset descargado tiene $real"
        }
        Write-Host "  ${etiqueta}: sha256 correcto"
    }
}

function Firmar-Authenticode {
    param([string]$Archivo, [string]$Huella)

    if (-not $Huella) {
        Write-Warning "SIN FIRMA AUTHENTICODE: $Archivo se publica sin firmar. Windows SmartScreen y las políticas de empresa pueden bloquearlo. Para firmarlo, configura ORUSCRIBE_AUTHENTICODE_THUMBPRINT con la huella de un certificado de CurrentUser\My que tenga clave privada."
        return $false
    }
    $limpia = ($Huella -replace '[^0-9A-Fa-f]', '').ToUpperInvariant()
    $cert = Get-Item -LiteralPath "Cert:\CurrentUser\My\$limpia" -ErrorAction Stop
    $firmado = Set-AuthenticodeSignature -FilePath $Archivo -Certificate $cert -ErrorAction Stop
    $comprobado = Get-AuthenticodeSignature -FilePath $Archivo -ErrorAction Stop
    if ($firmado.Status -ne 'Valid' -or $comprobado.Status -ne 'Valid') {
        throw "La firma Authenticode de $Archivo no es válida: $($comprobado.Status)"
    }
    Write-Host "Firmado Authenticode: $Archivo ($($comprobado.SignerCertificate.Subject))"
    return $true
}

Push-Location $repo
try {
    $workspaceVersion = Get-WorkspaceVersion
    if ($Version -ne $workspaceVersion) {
        throw "La versión pedida ($Version) no coincide con la del workspace ($workspaceVersion); el updater compara contra la compilada"
    }
    if ($SoloVerificar) {
        if (-not $VerificarAssets) {
            throw '-SoloVerificar necesita -VerificarAssets <carpeta con los assets descargados de la release>'
        }
        Test-HuellasPublicadas -Directorio $VerificarAssets -Version $Version
        $archivoLinux = Join-Path $VerificarAssets "oruscribe-agente-$Version-linux-x86_64.tar.gz"
        if (Test-Path -LiteralPath $archivoLinux -PathType Leaf) {
            Test-VersionDelArbolLinux -Archivo $archivoLinux -Esperada $Version
        }
        Write-Host 'OK: SHA256.txt coincide con los tres assets y el tar.gz anuncia la versión del manifiesto'
        return
    }

    # Igual que el release de Go: las llaves embebidas se comprueban siempre,
    # también con -SkipTests. Si están mal, el updater dejaría de exigir firma
    # y aceptaría cualquier manifiesto.
    Write-Host 'Llaves de firma...'
    & cargo test --locked -p orus-install --lib llaves_embebidas_estan_bien_formadas
    if ($LASTEXITCODE -ne 0) { throw 'las llaves de firma embebidas no estan bien; no se publica' }

    if (-not $SkipTests) {
        Write-Host 'Pruebas del instalador y del shell...'
        & cargo test --locked -p orus-install -p orus-shell
        if ($LASTEXITCODE -ne 0) { throw 'las pruebas no pasan; no se publica' }
    }

    Write-Host "Compilando Windows $Version..."
    if (-not $env:ORUSCRIBE_SENTRY_DSN) {
        Write-Warning 'ORUSCRIBE_SENTRY_DSN no está definido: este binario NO enviará telemetría a Sentry. Defínelo en el entorno antes de publicar (ver docs\telemetria.md).'
    }
    & cargo build --release --locked -p orus-shell
    if ($LASTEXITCODE -ne 0) { throw "cargo build falló con $LASTEXITCODE" }

    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    $built = Join-Path $target 'release\orus-shell.exe'
    if (-not (Test-Path -LiteralPath $built -PathType Leaf)) {
        throw "no encuentro el binario compilado en $built"
    }

    New-Item -ItemType Directory -Force -Path $out | Out-Null
    Copy-Item -LiteralPath $built -Destination $exe -Force
    $pdb = Join-Path $target 'release\orus_shell.pdb'
    if (Test-Path -LiteralPath $pdb -PathType Leaf) {
        Copy-Item -LiteralPath $pdb -Destination (Join-Path $out "orus_shell-$Version.pdb") -Force
    }

    $makensis = Find-Makensis
    if (-not $makensis) {
        throw 'no encontré makensis.exe. Instala NSIS 3.11 o apunta MAKENSIS a makensis.exe; el instalador de Windows es obligatorio para publicar'
    }
    $installer = Join-Path $out "Oruscribe-Agente-Setup-$Version.exe"
    Write-Host 'Empaquetando el instalador de Windows (NSIS)...'
    & $makensis "/DVERSION=$Version" "/DAPP_FILE=$exe" "/DOUT_FILE=$installer" "/DICON_FILE=$(Join-Path $repo 'build\windows\icon.ico')" (Join-Path $repo 'build\windows\installer.nsi')
    if ($LASTEXITCODE -ne 0) { throw "makensis falló con $LASTEXITCODE" }
    if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw 'makensis no dejó el instalador de Windows' }

    $huella = $env:ORUSCRIBE_AUTHENTICODE_THUMBPRINT
    $null = Firmar-Authenticode -Archivo $exe -Huella $huella
    $null = Firmar-Authenticode -Archivo $installer -Huella $huella

    $hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLower()
    $installerHash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLower()

    # Linux se publica solo si se bajó el artefacto del CI; el updater de ahí
    # consulta `linux` y `linux_sha256` en vez de `windows` y `sha256`.
    if ($LinuxArchive -and -not (Test-Path -LiteralPath $LinuxArchive -PathType Leaf)) {
        throw "-LinuxArchive no existe: $LinuxArchive"
    }
    $linuxHash = ''
    $linuxUrl = ''
    if ($LinuxArchive -and (Test-Path -LiteralPath $LinuxArchive -PathType Leaf)) {
        Test-VersionDelArbolLinux -Archivo $LinuxArchive -Esperada $Version
        $linuxHash = (Get-FileHash -LiteralPath $LinuxArchive -Algorithm SHA256).Hash.ToLower()
        $linuxUrl = "$BaseUrl/$(Split-Path $LinuxArchive -Leaf)"
    }

    # El manifiesto conserva el nombre de asset que ya usa el updater de Go.
    # `sha256` es la huella de Windows; `windows_sha256` no existe en el
    # Manifest de Rust y se ignora si queda en un latest.json viejo.
    $manifest = [ordered]@{
        version       = $Version
        notes         = $Notes
        windows       = "$BaseUrl/Oruscribe.Agente.exe"
        sha256        = $hash
        linux         = $linuxUrl
        linux_sha256  = $linuxHash
    }
    # LF fijo y sin BOM: la firma es sobre los bytes exactos, y git con autocrlf cambia CRLF.
    $json = ($manifest | ConvertTo-Json) -replace "`r`n", "`n"
    $latest = Join-Path $out 'latest.json'
    [System.IO.File]::WriteAllText($latest, $json + "`n", (New-Object System.Text.UTF8Encoding $false))

    if ($env:ORUSCRIBE_SIGNING_KEY -and (Test-Path -LiteralPath $env:ORUSCRIBE_SIGNING_KEY -PathType Leaf)) {
        Write-Host 'Firmando el manifiesto...'
        & cargo run --quiet --release --locked -p orus-install --bin signmanifest -- -key $env:ORUSCRIBE_SIGNING_KEY -sign $latest
        if ($LASTEXITCODE -ne 0) { throw 'no se pudo firmar el manifiesto; no se publica' }

        # Una firma bien formada puede venir de una llave que el updater no
        # acepta (check.rs:73, keys.rs:59); la comprobamos antes de seguir.
        Write-Host 'Verificando la firma contra las llaves embebidas...'
        $keysFile = Join-Path $repo 'crates\orus-install\src\update\keys.rs'
        $keysText = Get-Content -LiteralPath $keysFile -Raw
        $declaracion = [regex]::Match($keysText, 'TRUSTED_KEYS_B64\s*:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\]', 'Singleline')
        if (-not $declaracion.Success) { throw 'no encuentro TRUSTED_KEYS_B64 en keys.rs; no se publica' }
        $llaves = @([regex]::Matches($declaracion.Groups[1].Value, '"([^"]+)"') | ForEach-Object { $_.Groups[1].Value })
        if ($llaves.Count -eq 0) { throw 'TRUSTED_KEYS_B64 no tiene llaves; no se publica' }
        $verificada = $false
        foreach ($publica in $llaves) {
            & cargo run --quiet --release --locked -p orus-install --bin signmanifest -- -verify $latest -pub $publica 2>$null
            if ($LASTEXITCODE -eq 0) { $verificada = $true; break }
        }
        if (-not $verificada) {
            throw 'la llave de firma no coincide con las llaves embebidas; el updater rechazaría latest.json.sig; no se publica'
        }
    } else {
        # Un .sig viejo junto a un manifiesto nuevo es peor que no tener firma:
        # el cliente lo rechaza y parece que la release está rota. Al publicar
        # sin firma, el archivo anterior no debe sobrevivir.
        Remove-Item -LiteralPath (Join-Path $out 'latest.json.sig') -ErrorAction SilentlyContinue
        Write-Warning 'SIN FIRMA: las instalaciones con llave embebida rechazarán esta versión'
    }

    $huellas = "windows   $hash`ninstaller $installerHash`n"
    if ($linuxHash) { $huellas += "linux     $linuxHash`n" }
    $archivoHuellas = Join-Path $out 'SHA256.txt'
    Set-Content -LiteralPath $archivoHuellas -Value $huellas -Encoding ascii
    if ($VerificarAssets) {
        Test-HuellasPublicadas -Directorio $VerificarAssets -Version $Version
    }

    Write-Host ''
    Write-Host "Listo: $exe"
    Write-Host "  $([int]((Get-Item $exe).Length / 1MB)) MB  sha256 $hash"
    Write-Host "Listo: $installer"
    Write-Host "  $([int]((Get-Item $installer).Length / 1MB)) MB  sha256 $installerHash"
    if ($linuxHash) {
        Write-Host "Listo: $LinuxArchive"
        Write-Host "  sha256 $linuxHash"
    }
    Write-Host "  manifiesto: $latest"
    $sig = Join-Path $out 'latest.json.sig'
    if (Test-Path -LiteralPath $sig -PathType Leaf) {
        Write-Host "  firma:      $sig"
    } else {
        Write-Host '  firma:      ninguna (release sin firmar)'
    }
    Write-Host ''
    Write-Host "Sube el exe como Oruscribe.Agente.exe, el instalador, el tar.gz de Linux, latest.json (con latest.json.sig si existe) y el archivo generado $archivoHuellas como SHA256.txt a la release v$Version de JuanMS20/oruscribe-agente-descargas; no reescribas las huellas a mano. El manifiesto va a la rama main de ese repositorio. Después verifica los assets descargados con: pwsh -File scripts\release.ps1 -Version $Version -SoloVerificar -VerificarAssets <carpeta con los assets>."
} finally {
    Pop-Location
}
