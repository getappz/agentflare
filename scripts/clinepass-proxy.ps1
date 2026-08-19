param(
    [string]$Default = "clinepass/cline-pass/kimi-k3",
    [string]$Opus = "clinepass/cline-pass/qwen3.8-max",
    [string]$Sonnet = "clinepass/cline-pass/kimi-k3",
    [string]$Haiku = "clinepass/cline-pass/deepseek-v4-flash",
    [string]$Bin = "agentflare",
    [string]$Claude = "claude",
    [int]$Port = 35273
)

$env:MODEL = $Default
$env:MODEL_OPUS = $Opus
$env:MODEL_SONNET = $Sonnet
$env:MODEL_HAIKU = $Haiku

Write-Host "flare-proxy -> ClinePass multi-model"
Write-Host "  default : $Default"
Write-Host "  opus    : $Opus"
Write-Host "  sonnet  : $Sonnet"
Write-Host "  haiku   : $Haiku"
Write-Host "  bin     : $Bin"
Write-Host "  port    : $Port"
Write-Host ""

& $Bin daemon restart | Out-Host

$base = "http://127.0.0.1:$Port/proxy"
$ready = $false
for ($i = 0; $i -lt 30; $i++) {
    try {
        $tcp = [System.Net.Sockets.TcpClient]::new()
        $tcp.Connect("127.0.0.1", $Port)
        $tcp.Close()
        $ready = $true
        break
    } catch {
        Start-Sleep -Milliseconds 1000
    }
}
if (-not $ready) {
    Write-Error "proxy not up on port $Port after 30s; check 'agentflare daemon logs'"
    exit 1
}

Write-Host "proxy ready at $base"
Write-Host "launching $Claude with ANTHROPIC_BASE_URL=$base"
Write-Host ""

$env:ANTHROPIC_BASE_URL = $base
& $Claude @args