# 엘리시움을 Windows 앱처럼 설치한다(관리자 권한 필요 없음).
#  1) 릴리스 빌드: npx tauri build --no-bundle
#  2) 이 스크립트: 실행 파일을 %LOCALAPPDATA%\Programs\Elysium 에 복사하고
#     시작 메뉴와 바탕화면에 바로 가기를 만든다. 다시 실행하면 새 빌드로 바꿔 넣는다.
# 대화·설정(%APPDATA%\com.hoya.elysium)과 API 키(자격 증명 관리자)는 개발용 빌드와 같이 쓴다.
param([switch]$NoDesktop)
$ErrorActionPreference = 'Stop'

$src = 'C:\Users\HOYA\.elysium-build\target\release\elysium.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Elysium'
$exe = Join-Path $dir 'Elysium.exe'
if (-not (Test-Path $src)) { throw "릴리스 빌드가 없습니다. 먼저 'npx tauri build --no-bundle'을 실행하세요: $src" }

$running = Get-Process -Name Elysium -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }
if ($running) { throw '설치된 엘리시움이 실행 중입니다. 창을 닫고 다시 실행하세요.' }

New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $src $exe -Force

$shell = New-Object -ComObject WScript.Shell
function New-Shortcut([string]$path) {
  $s = $shell.CreateShortcut($path)
  $s.TargetPath = $exe
  $s.WorkingDirectory = $dir
  $s.IconLocation = "$exe,0"
  $s.Description = 'Elysium — 여러 AI를 이어서 쓰는 데스크톱 클라이언트'
  $s.Save()
}
$startMenu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Elysium.lnk'
New-Shortcut $startMenu
$made = @($startMenu)
if (-not $NoDesktop) {
  $desk = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Elysium.lnk'
  New-Shortcut $desk
  $made += $desk
}
"설치함: $exe ({0:N1} MB)" -f ((Get-Item $exe).Length / 1MB)
$made | ForEach-Object { "바로 가기: $_" }
