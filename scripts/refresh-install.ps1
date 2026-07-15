param(
  [string]$Task
)

$root = git rev-parse --show-toplevel 2>$null
if (-not $root) { $root = (Get-Location).Path }

# If .git is a directory, we're at the main repo root (not inside a worktree).
if (Test-Path (Join-Path $root ".git") -PathType Container) {
  $lines = git worktree list --porcelain
  $worktrees = @()
  $current = $null
  foreach ($line in $lines) {
    if ($line -like "worktree *") {
      if ($current) { $worktrees += $current }
      $current = [PSCustomObject]@{ Path = $line.Substring(9); Branch = "(detached)" }
    } elseif ($line -like "branch *") {
      if ($current) { $current.Branch = $line.Substring(7) -replace '^refs/heads/', '' }
    }
  }
  if ($current) { $worktrees += $current }

  if (-not $worktrees) {
    Write-Error "[refresh-install] No worktrees found under $root."
    exit 1
  }

  if ($Task) {
    $match = @($worktrees | Where-Object {
      $_.Branch -eq $Task -or
      (Split-Path $_.Path -Leaf) -eq $Task -or
      (($_.Path -replace '\\', '/') -like "*/$Task")
    })
    if ($match.Count -eq 0) {
      $list = $worktrees | ForEach-Object { "  $($_.Path)  [$($_.Branch)]" }
      Write-Error "[refresh-install] No worktree matching -Task '$Task'. Available:`n$($list -join "`n")"
      exit 1
    }
    if ($match.Count -gt 1) {
      $list = $match | ForEach-Object { "  $($_.Path)  [$($_.Branch)]" }
      Write-Error "[refresh-install] -Task '$Task' matched multiple worktrees:`n$($list -join "`n")"
      exit 1
    }
    $root = $match[0].Path
  } else {
    $list = $worktrees | ForEach-Object { "  $($_.Path)  [$($_.Branch)]" }
    Write-Error "[refresh-install] Run from inside a worktree, or pass -Task <name/branch> to pick one. Available:`n$($list -join "`n")"
    exit 1
  }
}

Write-Host "[refresh-install] Installing from $root"
cargo install --path "$root" --force
