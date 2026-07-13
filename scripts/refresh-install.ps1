$root = git rev-parse --show-toplevel 2>$null
if (-not $root) { $root = (Get-Location).Path }
# If .git is a directory (non-worktree repo root), look for worktrees
if (Test-Path (Join-Path $root ".git") -PathType Container) {
  $wtDir = Join-Path $root ".worktrees"
  if (Test-Path $wtDir) {
    $wt = Get-ChildItem $wtDir -Directory | Select-Object -First 1
    if ($wt) { $root = $wt.FullName }
  }
}
Write-Host "[refresh-install] Installing from $root"
cargo install --path "$root" --force
