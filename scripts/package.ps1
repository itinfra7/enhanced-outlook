# 포터블 배포판 만들기: target\dist\EnhancedOutlook-{버전}\EnhancedOutlook.exe 하나만.
# README·zip은 만들지 않는다(사용자가 직접 비밀번호를 걸어 압축해 배포한다).
# 첫 실행 때 실행 파일 옆에 EnhancedOutlook\(메일·색인·설정·로그·WebView2 데이터)이 생긴다. 설치·레지스트리 없음.
#   -Build : 먼저 포터블 실행 파일을 빌드한다(npm run tauri build -- --no-bundle).
param([switch]$Build)

Set-Location (Split-Path $PSScriptRoot -Parent)

if ($Build) {
    Write-Host '==> 포터블 실행 파일 빌드' -ForegroundColor Cyan
    Push-Location app
    npm run tauri build -- --no-bundle
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { Write-Host '실패: 빌드' -ForegroundColor Red; exit $code }
}

$exe = 'target\release\enhanced-outlook.exe'
if (-not (Test-Path $exe)) { Write-Host "실행 파일이 없습니다: $exe (-Build로 빌드)" -ForegroundColor Red; exit 1 }
$conf = [IO.File]::ReadAllText((Resolve-Path 'app\src-tauri\tauri.conf.json').Path, [Text.Encoding]::UTF8) | ConvertFrom-Json
$version = $conf.version
$dir = Join-Path 'target\dist' "EnhancedOutlook-$version"

# 이 스크립트가 만든 EnhancedOutlook.exe만 덮어쓴다. 아무것도 지우지 않는다.
if (Test-Path (Join-Path $dir 'EnhancedOutlook')) {
    Write-Host "$dir 안에 EnhancedOutlook 데이터 폴더가 있습니다(배포판으로 실행해 본 흔적). 배포판에 섞이지 않게 먼저 치워 주세요." -ForegroundColor Red
    exit 1
}
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $exe (Join-Path $dir 'EnhancedOutlook.exe') -Force

$others = @(Get-ChildItem $dir | Where-Object { $_.Name -ne 'EnhancedOutlook.exe' })
if ($others.Count -gt 0) {
    Write-Host ("주의: 배포 폴더에 다른 파일이 있습니다(지우지 않음): {0}" -f (($others | ForEach-Object { $_.Name }) -join ', ')) -ForegroundColor Yellow
}
$size = (Get-Item (Join-Path $dir 'EnhancedOutlook.exe')).Length / 1MB
Write-Host ("배포판: {0}\EnhancedOutlook.exe ({1:N1} MB)" -f (Resolve-Path $dir).Path, $size) -ForegroundColor Green
