# 성능 회귀 점검(계획서 10장 8단계: 3·4단계 측정 자동화).
#   1) 검색 벤치마크(합성 10만 통, 성능·정확성 수용 기준) — search-bench가 스스로 판정한다.
#   2) 화면 측정(enhanced-outlook.exe --perf-test, 합성 저장소) — perf-baseline.json의 한도와 비교한다.
# 메일함·서버와는 무관하다(합성 데이터만). 실패가 하나라도 있으면 종료 코드 1.
#   -Build      : 먼저 포터블 실행 파일을 빌드한다(npm run tauri build -- --no-bundle).
#   -Synth 경로 : 합성 저장소가 있는 폴더(그 안에 EnhancedOutlook\db.sqlite). 없으면 만드는 명령을 알려 준다.
#   -SkipBench  : 검색 벤치마크를 건너뛴다.
param([switch]$Build, [string]$Synth = 'target\synth-100k', [switch]$SkipBench)

Set-Location (Split-Path $PSScriptRoot -Parent)
$fails = @()

if ($Build) {
    Write-Host '==> 포터블 실행 파일 빌드' -ForegroundColor Cyan
    Push-Location app
    npm run tauri build -- --no-bundle
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { Write-Host '실패: 빌드' -ForegroundColor Red; exit $code }
}

if (-not $SkipBench) {
    Write-Host '==> 검색 벤치마크(3단계, target\search-bench 색인 재사용)' -ForegroundColor Cyan
    cargo run --release --quiet -p eo-search --bin search-bench
    if ($LASTEXITCODE -ne 0) { $fails += '검색 벤치마크' }
}

Write-Host '==> 화면 측정(4단계, --perf-test)' -ForegroundColor Cyan
$exe = 'target\release\enhanced-outlook.exe'
$db = Join-Path $Synth 'EnhancedOutlook\db.sqlite'
if (-not (Test-Path $exe)) { Write-Host "실행 파일이 없습니다: $exe (-Build로 빌드)" -ForegroundColor Red; exit 1 }
if (-not (Test-Path $db)) {
    Write-Host "합성 저장소가 없습니다: $db" -ForegroundColor Red
    Write-Host "만들기: cargo run --release -p owa-cli -- store --root $Synth\EnhancedOutlook synth" -ForegroundColor Yellow
    exit 1
}
Copy-Item $exe $Synth -Force
$log = Join-Path $Synth 'EnhancedOutlook\logs\perf.log'
$before = if (Test-Path $log) { (Get-Item $log).Length } else { 0 }
$p = Start-Process -FilePath (Resolve-Path (Join-Path $Synth 'enhanced-outlook.exe')) -ArgumentList '--perf-test' -PassThru
if (-not $p.WaitForExit(300000)) {
    Stop-Process -Id $p.Id -Confirm:$false
    Write-Host '실패: 5분 안에 끝나지 않음' -ForegroundColor Red
    exit 1
}
if (-not (Test-Path $log)) { Write-Host "측정 기록이 없습니다: $log" -ForegroundColor Red; exit 1 }
$bytes = [IO.File]::ReadAllBytes((Resolve-Path $log).Path)
if ($bytes.Length -lt $before) { $before = 0 } # 기록 파일이 돌려졌다
$text = [Text.Encoding]::UTF8.GetString($bytes, $before, $bytes.Length - $before)
Write-Host $text
$line = ($text -split "`r?`n") | Where-Object { $_ -like '결과 JSON: *' } | Select-Object -Last 1
if (-not $line) { Write-Host '실패: 결과 JSON이 없습니다' -ForegroundColor Red; exit 1 }
$result = $line.Substring('결과 JSON: '.Length) | ConvertFrom-Json
$baseline = [IO.File]::ReadAllText((Resolve-Path 'scripts\perf-baseline.json').Path, [Text.Encoding]::UTF8) | ConvertFrom-Json

$rows = foreach ($it in $baseline.items) {
    $v = $result.($it.key)
    if ($null -eq $v) {
        $verdict = '값 없음'
        $fails += $it.name
    } elseif (($null -ne $it.max -and $v -gt $it.max) -or ($null -ne $it.min -and $v -lt $it.min)) {
        $verdict = '회귀'
        $fails += $it.name
    } else {
        $verdict = '통과'
    }
    $limit = if ($null -ne $it.max) { "≤ $($it.max)" } else { "≥ $($it.min)" }
    [pscustomobject]@{ 항목 = $it.name; 이번 = "$v $($it.unit)"; 기준선 = "$($it.baseline) $($it.unit)"; 한도 = "$limit $($it.unit)"; 판정 = $verdict }
}
$rows | Format-Table -AutoSize | Out-String | Write-Host

if ($fails.Count) {
    Write-Host "판정: 실패 — $($fails -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host '판정: 성능 회귀 없음' -ForegroundColor Green
