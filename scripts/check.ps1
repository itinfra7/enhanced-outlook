# 형식·린트·테스트를 한 번에 실행한다. -Fix: cargo fmt로 고친 뒤 검사.
param([switch]$Fix)

# 성공 여부는 종료 코드로 판정한다. 'Stop'이면 PowerShell 5.1이 cargo의 stderr 진행 출력을 오류로 바꾼다.
Set-Location (Split-Path $PSScriptRoot -Parent)

function Step([string]$name, [scriptblock]$cmd) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { Write-Host "실패: $name" -ForegroundColor Red; exit $LASTEXITCODE }
}

if ($Fix) { Step 'cargo fmt' { cargo fmt --all } }
Step 'cargo fmt --check' { cargo fmt --all -- --check }
Step 'cargo clippy -D warnings' { cargo clippy --workspace --all-targets -- -D warnings }
Step 'cargo test' { cargo test --workspace }
Step 'svelte-check' { npm --prefix app run check }

Write-Host '모두 통과' -ForegroundColor Green
