# Local presubmit: run before every commit. A fast subset of CI for the host CPU
$ErrorActionPreference = "Stop"

function Step($name, [scriptblock]$cmd) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { Write-Host "FAILED: $name" -ForegroundColor Red; exit 1 }
}

Step "format"  { cargo fmt --all -- --check }
Step "clippy"  { cargo clippy --all-targets -- -D warnings }
# Only lints the PEXT code when the host target has BMI2; otherwise it repeats the magic run.
Step "clippy (pext)" { cargo clippy --all-targets --features pext -- -D warnings }
Step "test"    { cargo test --all-targets }
Step "test (pext)" { cargo test --all-targets --features pext }
Step "test (release-checked)" { cargo test --profile release-checked }
Step "doctest" { cargo test --doc }
Step "docs" {
    $previous = $env:RUSTDOCFLAGS
    $env:RUSTDOCFLAGS = "-D warnings"
    try { cargo doc --no-deps --document-private-items } finally { $env:RUSTDOCFLAGS = $previous }
}

Write-Host "All checks passed." -ForegroundColor Green
