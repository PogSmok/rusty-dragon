# Local presubmit: run before every commit. Mirrors the CI pipeline.
$ErrorActionPreference = "Stop"

function Step($name, [scriptblock]$cmd) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { Write-Host "FAILED: $name" -ForegroundColor Red; exit 1 }
}

Step "format"  { cargo fmt --all -- --check }
Step "clippy"  { cargo clippy --all-targets --all-features -- -D warnings }
Step "test"    { cargo test --all-targets }
Step "doctest" { cargo test --doc }
Step "docs"    { $env:RUSTDOCFLAGS = "-D warnings"; cargo doc --no-deps }

Write-Host "All checks passed." -ForegroundColor Green
