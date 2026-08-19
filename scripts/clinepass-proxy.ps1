param(
    [string]$Default = "clinepass/cline-pass/kimi-k3",
    [string]$Opus = "clinepass/cline-pass/qwen3.8-max",
    [string]$Sonnet = "clinepass/cline-pass/kimi-k3",
    [string]$Haiku = "clinepass/cline-pass/deepseek-v4-flash",
    [string]$Bin = "agentflare"
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
Write-Host ""

& $Bin serve @args