# Mede RAM e CPU dos processos sala-* (PowerShell 5.1 compativel).
# Uso: .\measure.ps1 [-Seconds 30] [-Interval 1] [-ProcessId 1234,5678] [-Json]
# CPU e calculada pela variacao de TotalProcessorTime entre amostras:
#   cpu_maquina = delta_cpu / (delta_tempo * nProcessadoresLogicos)  (% da maquina inteira)
#   cpu_nucleo  = delta_cpu / delta_tempo                           (% de um nucleo)
param(
    [int]$Seconds = 30,
    [double]$Interval = 1,
    [int[]]$ProcessId = @(),
    [switch]$Json
)

$ErrorActionPreference = 'Stop'
$MB = 1MB
$nLogicos = [Environment]::ProcessorCount

# Seleciona os processos alvo: por PID, ou todos os que comecam com "sala-".
function Get-Alvos {
    if ($ProcessId.Count -gt 0) {
        return @(Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)
    }
    return @(Get-Process -Name 'sala-*' -ErrorAction SilentlyContinue)
}

$alvos = Get-Alvos
if ($alvos.Count -eq 0) {
    Write-Warning 'Nenhum processo sala-* encontrado.'
    if ($Json) { '[]' }
    exit 0
}

# Estado por PID: nome, ultima leitura de CPU/tempo, somas e picos.
$estado = @{}
function Amostrar($proc, $agora) {
    $proc.Refresh()
    $id = $proc.Id
    if (-not $estado.ContainsKey($id)) {
        $estado[$id] = @{
            Nome = $proc.ProcessName; N = 0; WsSoma = 0.0; WsPico = 0.0
            PbSoma = 0.0; PbPico = 0.0; CpuAnt = $null; TAnt = $null
            CpuTotalSeg = 0.0; TTotalSeg = 0.0
        }
    }
    $e = $estado[$id]
    $ws = $proc.WorkingSet64 / $MB
    $pb = $proc.PrivateMemorySize64 / $MB
    $e.N++
    $e.WsSoma += $ws; if ($ws -gt $e.WsPico) { $e.WsPico = $ws }
    $e.PbSoma += $pb; if ($pb -gt $e.PbPico) { $e.PbPico = $pb }
    $cpu = $proc.TotalProcessorTime.TotalSeconds
    if ($null -ne $e.CpuAnt) {
        $e.CpuTotalSeg += ($cpu - $e.CpuAnt)
        $e.TTotalSeg += ($agora - $e.TAnt).TotalSeconds
    }
    $e.CpuAnt = $cpu; $e.TAnt = $agora
}

$fim = (Get-Date).AddSeconds($Seconds)
$procs = $alvos
while ($true) {
    $agora = Get-Date
    foreach ($p in $procs) {
        try { Amostrar $p $agora } catch { }   # processo pode ter encerrado
    }
    if ((Get-Date) -ge $fim) { break }
    Start-Sleep -Milliseconds ([int]($Interval * 1000))
}

$linhas = @()
foreach ($id in ($estado.Keys | Sort-Object)) {
    $e = $estado[$id]
    $nucleo = 0.0
    if ($e.TTotalSeg -gt 0) { $nucleo = 100.0 * $e.CpuTotalSeg / $e.TTotalSeg }
    $linhas += [pscustomobject]@{
        Processo      = $e.Nome
        PID           = $id
        Amostras      = $e.N
        WS_medio_MB   = [math]::Round($e.WsSoma / $e.N, 1)
        WS_pico_MB    = [math]::Round($e.WsPico, 1)
        Priv_medio_MB = [math]::Round($e.PbSoma / $e.N, 1)
        Priv_pico_MB  = [math]::Round($e.PbPico, 1)
        CPU_maquina_pct = [math]::Round($nucleo / $nLogicos, 2)
        CPU_nucleo_pct  = [math]::Round($nucleo, 2)
    }
}

if ($Json) {
    $linhas | ConvertTo-Json -Depth 3
} else {
    Write-Host ("Janela: {0}s, intervalo {1}s, processadores logicos: {2}" -f $Seconds, $Interval, $nLogicos)
    $linhas | Format-Table -AutoSize | Out-String | Write-Host
}
exit 0
