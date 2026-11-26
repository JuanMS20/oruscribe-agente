param(
    [string]$Bin = 'target\release\orus-shell.exe',
    [int]$EsperaSegundos = 45
)

$ErrorActionPreference = 'Stop'
$titulo = 'Oruscribe Agente'

if (-not (Test-Path -LiteralPath $Bin)) {
    Write-Error "no existe el binario: $Bin"
    exit 2
}

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Win32Humo {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
}
'@

$temporales = Join-Path $env:TEMP ("orus-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporales | Out-Null
$log = Join-Path $temporales 'app.log'
$app = $null

function Fallar($mensaje) {
    Write-Host "FALLO: $mensaje" -ForegroundColor Red
    if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log | ForEach-Object { Write-Host "  $_" } }
    $script:codigo = 1
}

$codigo = 0
try {
    $env:ORUSCRIBE_DATA_DIR = Join-Path $temporales 'datos'
    $app = Start-Process -FilePath (Resolve-Path -LiteralPath $Bin) -PassThru `
        -RedirectStandardError $log -RedirectStandardOutput (Join-Path $temporales 'out.log')

    $limite = (Get-Date).AddSeconds($EsperaSegundos)
    $ventana = [IntPtr]::Zero
    while ((Get-Date) -lt $limite) {
        if ($app.HasExited) { Fallar "la app murió con código $($app.ExitCode) antes de abrir la ventana"; break }
        $app.Refresh()
        if ($app.MainWindowHandle -ne [IntPtr]::Zero -and $app.MainWindowTitle -eq $titulo) {
            $ventana = $app.MainWindowHandle
            break
        }
        Start-Sleep -Milliseconds 500
    }
    if ($codigo -eq 0 -and $ventana -eq [IntPtr]::Zero) { Fallar "no apareció la ventana '$titulo' en $EsperaSegundos s" }

    if ($codigo -eq 0) {
        $webview = $null
        while ((Get-Date) -lt $limite) {
            $webview = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($app.Id) AND Name='msedgewebview2.exe'" |
                Select-Object -First 1
            if ($webview) { break }
            Start-Sleep -Milliseconds 500
        }
        if (-not $webview) { Fallar 'WebView2 no arrancó: no hay msedgewebview2.exe hijo de la app' }
    }

    if ($codigo -eq 0) {
        $colores = 0
        while ((Get-Date) -lt $limite) {
            $r = New-Object Win32Humo+RECT
            [void][Win32Humo]::GetWindowRect($ventana, [ref]$r)
            $ancho = $r.Right - $r.Left
            $alto = $r.Bottom - $r.Top
            if ($ancho -gt 100 -and $alto -gt 100) {
                $bmp = New-Object System.Drawing.Bitmap $ancho, $alto
                $g = [System.Drawing.Graphics]::FromImage($bmp)
                $hdc = $g.GetHdc()
                [void][Win32Humo]::PrintWindow($ventana, $hdc, 2)
                $g.ReleaseHdc($hdc)
                $g.Dispose()
                $vistos = New-Object 'System.Collections.Generic.HashSet[int]'
                for ($y = 0; $y -lt $alto; $y += 7) {
                    for ($x = 0; $x -lt $ancho; $x += 7) { [void]$vistos.Add($bmp.GetPixel($x, $y).ToArgb()) }
                }
                $bmp.Dispose()
                $colores = $vistos.Count
                if ($colores -gt 8) { break }
            }
            Start-Sleep -Milliseconds 700
        }
        if ($colores -le 8) { Fallar "la ventana está vacía: $colores colores (esperaba más de 8)" }
        else { Write-Host "ok: ventana '$titulo' con WebView2 y $colores colores" -ForegroundColor Green }
    }
}
finally {
    if ($app -and -not $app.HasExited) { & taskkill.exe /PID $app.Id /T /F *> $null }
    Remove-Item -LiteralPath $temporales -Recurse -Force -ErrorAction SilentlyContinue
}
exit $codigo
