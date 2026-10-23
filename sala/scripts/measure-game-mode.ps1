# Roteiro reproduzivel: mede o Concord core no modo jogo (T-014, spec secao 8).
# Uso: .\measure-game-mode.ps1 [-Seconds 60] [-Settle 5] [-NoBuild]
# Sobe os perfis a e b (audio sintetico), manda EnterGameMode ao perfil a e mede o nucleo a.
param(
    [int]$Seconds = 60,
    [int]$Settle = 5,
    [switch]$NoBuild,
    [switch]$NoCall   # sem chamada: mede o nucleo ocioso no modo jogo
)

$ErrorActionPreference = 'Stop'
$raiz = Split-Path -Parent $PSScriptRoot          # sala/
$rel = Join-Path $raiz 'target\release'
$core = Join-Path $rel 'concord.exe'
$fakeUi = Join-Path $rel 'examples\fake_ui.exe'
$iniciados = @()
$backups = @{}   # config.toml original de cada perfil (restaurado no finally)

function Encerrar-Tudo {
    # Tudo que o Concord deixou vivo: nucleos, UI de mentira e o que subimos.
    foreach ($p in $iniciados) { try { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } catch { } }
    Get-Process -Name 'concord*', 'fake_ui' -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
}

function Esperar-Pipe([string]$nome, [int]$timeoutSeg) {
    $limite = (Get-Date).AddSeconds($timeoutSeg)
    while ((Get-Date) -lt $limite) {
        if (Test-Path "\\.\pipe\$nome") { return $true }
        Start-Sleep -Milliseconds 250
    }
    return $false
}

# Desliga a deteccao automatica de jogo nos perfis a e b: o terminal maximizado conta como jogo
# e faria o nucleo entrar no modo jogo sozinho, antes do EnterGameMode explicito.
function Desligar-AutoDetect {
    foreach ($perfil in 'a', 'b') {
        $cfg = Join-Path $env:APPDATA "Concord\profiles\$perfil\config.toml"
        if (-not (Test-Path $cfg)) { continue }   # primeira execucao: o padrao vale
        $original = [IO.File]::ReadAllText($cfg)
        $backups[$cfg] = $original
        [IO.File]::WriteAllText($cfg, ($original -replace '(?m)^auto_detect\s*=\s*true', 'auto_detect = false'))
    }
}

try {
    Desligar-AutoDetect
    if (-not $NoBuild) {
        Push-Location $raiz
        try {
            & cargo build --release -p sala-core --bin concord --example fake_ui
            if ($LASTEXITCODE -ne 0) { throw "cargo build falhou ($LASTEXITCODE)" }
        } finally { Pop-Location }
    }
    foreach ($f in @($core, $fakeUi)) { if (-not (Test-Path $f)) { throw "nao encontrado: $f" } }

    Encerrar-Tudo
    $env:SALA_UI_EXE = $fakeUi
    $usuario = $env:USERNAME -replace '[^A-Za-z0-9_.-]', '_'   # sanitize igual ao ipc.rs (D-010)

    foreach ($perfil in 'a', 'b') {
        $iniciados += Start-Process -FilePath $core -ArgumentList '--profile', $perfil, '--synthetic-audio' `
            -WindowStyle Hidden -PassThru
    }
    $pipeA = "sala-core-$usuario-a"
    foreach ($perfil in 'a', 'b') {
        if (-not (Esperar-Pipe "sala-core-$usuario-$perfil" 30)) { throw "pipe do perfil $perfil nao apareceu" }
    }
    # Espera os dois fake_ui (um por nucleo) aparecerem e conectarem, para provar que o modo jogo os encerra.
    $limite = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $limite -and @(Get-Process -Name 'fake_ui' -ErrorAction SilentlyContinue).Count -lt 2) {
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Seconds 2
    Write-Host "Antes do modo jogo:"
    Get-Process -Name 'concord*', 'fake_ui' -ErrorAction SilentlyContinue | Format-Table Id, ProcessName -AutoSize | Out-String | Write-Host

    # Uma linha JSON por mensagem no pipe do perfil a.
    $cli = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeA, [System.IO.Pipes.PipeDirection]::InOut)
    $cli.Connect(5000)
    function Enviar([string]$json) {
        $bytes = [Text.Encoding]::UTF8.GetBytes($json + "`n")
        $cli.Write($bytes, 0, $bytes.Length)
        $cli.Flush()
    }
    if (-not $NoCall) {
        # Os perfis a e b ja sao amigos um do outro (config padrao): a liga para b.
        Enviar '{"type":"Call","friend":"b"}'
        Write-Host 'Call a->b enviado; aguardando a chamada estabilizar (5 s)...'
        Start-Sleep -Seconds 5
    }
    Enviar '{"type":"EnterGameMode"}'
    Write-Host "EnterGameMode enviado a $pipeA; aguardando $Settle s..."
    Start-Sleep -Seconds $Settle

    $nucleoA = $iniciados[0]
    if ($nucleoA.HasExited) { throw 'nucleo a encerrou antes da medicao' }
    & (Join-Path $PSScriptRoot 'measure.ps1') -Seconds $Seconds -Interval 1 -ProcessId $nucleoA.Id

    Write-Host "Processos vivos (Concord / fake_ui) apos a medicao:"
    $vivos = @(Get-Process -Name 'concord*', 'fake_ui' -ErrorAction SilentlyContinue)
    $vivos | Format-Table Id, ProcessName -AutoSize | Out-String | Write-Host
    # Processos da instancia a: o proprio nucleo + filhos dele (a UI e lancada pelo nucleo).
    $filhos = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($nucleoA.Id)" -ErrorAction SilentlyContinue)
    $nomesA = @('concord') + @($filhos | Where-Object { $_.Name -ne 'conhost.exe' } | ForEach-Object { $_.Name -replace '\.exe$', '' })
    Write-Host ("Processos da instancia a no modo jogo: {0} ({1})" -f $nomesA.Count, ($nomesA -join ', '))
    $cli.Dispose()
} finally {
    Encerrar-Tudo
    Remove-Item Env:\SALA_UI_EXE -ErrorAction SilentlyContinue
    foreach ($cfg in $backups.Keys) { [IO.File]::WriteAllText($cfg, $backups[$cfg]) }
}
exit 0
