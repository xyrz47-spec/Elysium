# Elysium

여러 AI(CLI: Claude Code·Codex·Gemini CLI, API: Claude·OpenAI·Gemini·NVIDIA)를 엮어 완성도 높은 답과 결과를 내는 데스크톱 AI 클라이언트입니다(Tauri 2 + Rust + React/TS, Windows 11).
2026-10-17에 JARVIS에서 방향을 바꿨습니다. 음성·로컬 LLM 기능과 예전 코드·모델은 정리했습니다(2026-10-17 휴지통으로 이동).

## 실행

```powershell
npm install
npm run tauri dev                      # 개발 실행
npx tauri build --debug --no-bundle    # 개발용 실행 파일: C:\Users\HOYA\.elysium-build\target\debug\elysium.exe
cd src-tauri; cargo test               # 단위 테스트 39개(폰 앱: mobile/src-tauri 1개)
```

**Windows 앱으로 설치**(관리자 권한 필요 없음):

```powershell
npx tauri build --no-bundle                                        # 릴리스 빌드(콘솔 창 없음)
powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1  # %LOCALAPPDATA%\Programs\Elysium + 시작 메뉴·바탕화면 바로 가기
```

새로 빌드한 뒤 스크립트를 다시 실행하면 설치본을 바꿔 넣습니다(설치된 앱은 닫아 두세요). 대화·설정·API 키는 개발용 빌드와 같이 씁니다. 설치 파일(.msi/.exe)로 묶으려면 `npx tauri build`가 WiX·NSIS 도구를 내려받아야 합니다.

AI CLI를 설치해 로그인하거나, 설정 → API 키에 키를 넣으세요(둘 다 써도 됩니다). API 키는 Windows 자격 증명 관리자에만 저장합니다(`ANTHROPIC_API_KEY`·`OPENAI_API_KEY`·`GEMINI_API_KEY`·`NVIDIA_API_KEY`·`TYPESAFE_API_KEY` 환경 변수도 읽음).

| CLI | 설치 | 상태 |
| --- | --- | --- |
| Claude Code | https://claude.com/claude-code | 이 PC에서 실제 출력으로 확인(2.1.299) |
| Codex | `npm i -g @openai/codex` | 공개 형식 기준으로 연결, 미검증 |
| Gemini CLI | `npm i -g @google/gemini-cli` | 공개 형식 기준으로 연결, 미검증 |

| Claude API · OpenAI API · Gemini API · NVIDIA API | 설정 → API 키 | 키가 없어 실제 호출 미검증(스트림 해석은 단위 테스트) |

새로 설치했다면 설정 → CLI → "다시 찾기". API는 파일을 다루지 못하므로 대화 모드에서만 쓰고(작업 모드는 CLI만), 빠른 답과 검토에 씁니다. 모델 이름은 설정에서 바꿀 수 있습니다(기본: claude-sonnet-5-5, gpt-5, gemini-2.5-flash, nvidia/nemotron-3-nano-omni-30b-a3b-reasoning).

## 하는 일

요청 하나가 이렇게 처리됩니다(`src-tauri/src/orchestrator.rs`).

1. **스킬**: `/이름`으로 부르거나, 스킬에 적힌 낱말이 요청에 있으면 자동으로 지시문을 붙입니다.
2. **JEV 판단**(설정에서 켰을 때): TypeSafe Jev가 요청의 종류(코드·글쓰기·조사·분석·대화·번역), 난이도(0~3), 정확도 중요도(0~3: 틀리면 얼마나 곤란한지)를 판단합니다. 확신도 50% 미만이거나 실패하면 규칙으로 정합니다. 요청 글이 TypeSafe로 전송됩니다(실측 약 0.2초, 요청당 약 $0.00002).
3. **AI 고르기**: 입력창에서 "자동"이면 쓸 수 있는 AI 중에서 종류별 순서로 고릅니다(코드: Claude Code → Codex → Gemini CLI → API들, 대화·번역: Gemini API → NVIDIA → … 처럼 빠른 API 먼저, 조사: Gemini 먼저). 첫 AI가 실패하면 다음 AI로 넘어갑니다. 직접 고를 수도 있습니다.
4. **작업 모드면 허락 받기**: 파일을 고치는 요청은 대화창에 허락 카드를 띄우고, 허락한 뒤에만 편집 도구를 켭니다(10분 안에 답이 없으면 취소).
5. **초안**: 고른 CLI가 답을 쓰고, 화면에 글자·도구 사용이 실시간으로 나옵니다.
6. **노력치**(JEV를 켰을 때): JEV 난이도로 단계를 정해 토큰을 아낍니다. 확신이 낮으면 각 엔진의 기본값을 씁니다.

   | 단계 | 조건 | Claude Code | 노력치(Claude Code·Codex·OpenAI·Gemini·Claude API) | API 최대 출력 |
   | --- | --- | --- | --- | --- |
   | 가볍게 | 난이도 < 1.0(대화·번역은 < 1.5), 작업 모드 제외 | `--model haiku` | low | 1,500 |
   | 보통 | 난이도 < 2.0 | `--model sonnet` | medium | 4,000 |
   | 깊게 | 그 밖 | 기본 모델 | high | 8,192 |

   실측(인사 한 문장, Claude Code 2.1.299): haiku·low $0.0007·1.2초, sonnet·medium $0.0103·3.0초, 기본 모델 약 $0.03. NVIDIA에는 노력치 대신 Nemotron 모델의 생각 단계를 켜고 끕니다(`chat_template_kwargs.enable_thinking`, 가볍게=끔). Codex(`-c model_reasoning_effort`)와 API의 노력치는 이 PC에서 실제로 확인하지 못했습니다.
7. **검토**(설정: 자동·항상·토론·끄기): JEV 난이도 2.0 이상(또는 규칙상 길거나 어려운 요청)이면 **다른 회사의** AI가 초안을 검토해 최종 답을 씁니다. 없으면 다른 엔진, 그것도 없으면 같은 엔진이 새 세션에서 검토합니다. 답 아래 "과정"을 펼치면 단계·시간·토큰·추정 비용·검토 전 초안이 보입니다.

8. **토론**(정확도가 중요할 때, `src-tauri/src/orchestrator.rs`): JEV 정확도 중요도 2.0 이상(JEV가 없으면 "정확히·검증·팩트·근거·출처" 같은 낱말), `debate: true` 스킬, 또는 설정의 "토론"이면 검토 대신 토론합니다.
   - 서로 다른 회사의 두 AI가 **동시에 따로** 답합니다(1차, 3분 한도).
   - 보통·깊게 단계면 서로의 답을 읽고 **반론**한 뒤 고친 답을 냅니다(2분 한도, 늦은 쪽은 기다리지 않음).
   - **판정 AI**(남은 세 번째 회사, 없으면 첫 AI)가 사실·숫자·계산을 확인해 하나의 답으로 정리하고, 갈린 쟁점은 끝에 적습니다.
   - 한쪽이 실패하면 다른 쪽 답을 씁니다. 리본에 1차·반론·판정이 보이고, "과정"에서 각 AI의 답을 펼쳐 볼 수 있습니다.
   - 실측: "17²³ mod 1000"(Claude Code·NVIDIA, 판정 Claude Code) → 713(정답), 약 5만 토큰·$0.22. 가장 정확하지만 호출이 4~5배입니다.

**세션 메모리**(`src-tauri/src/memory.rs`): 같은 세션(대화)에서는 모든 AI(초안·검토·토론·판정, CLI·API)가 같은 기억을 씁니다.
- 매 실행마다 [세션 요약] + [최근 대화 12개(누가 답했는지 포함)]를 요청 앞에 붙입니다.
- Claude Code처럼 자기 세션을 이어 쓰는(`--resume`) 엔진에는 그 엔진이 아직 못 본 차례(다른 AI가 답한 부분)만 붙입니다.
- 오래된 대화가 쌓이면 작업이 끝난 뒤 가장 싼 AI(빠른 API → Claude Code haiku·low)가 요약에 접어 넣습니다(1,400자 이내, 결정·선호·사실·남은 일).
- 세션 머리의 **기억** 버튼에서 요약을 보고 지울 수 있습니다.
- 실측: Claude Code에 이름·색을 알려 주고 같은 세션에서 NVIDIA에 물으니 정확히 답했고, 다시 Claude Code에 "앞에서 다른 AI가 뭐라고 했지"를 물으니 NVIDIA의 답을 그대로 말했습니다.

### 안전

- 요청 본문은 CLI **표준 입력**으로만 넘깁니다(명령줄 인자·셸에 넣지 않음).
- 대화 모드의 Claude Code 도구는 `Read, Grep, Glob, WebFetch, WebSearch`뿐입니다. 작업 모드는 여기에 `Edit, Write`를 더하고 `acceptEdits`로 실행합니다. 명령 실행(Bash)은 어느 모드에서도 켜지 않습니다. Codex는 `read-only`·`workspace-write` 샌드박스를 씁니다.
- CLI는 **세션의 작업 폴더**에서 실행합니다. 세션마다 폴더를 고를 수 있고(입력창·세션 머리의 폴더 칩 → Windows 폴더 선택 창), 고르지 않으면 기본 작업 폴더(`문서\Elysium`, 설정에서 변경)를 씁니다. 폴더를 바꾸면 그 세션의 CLI 세션은 새로 시작합니다(Claude Code 세션이 폴더에 묶여 있기 때문).
- Claude Code는 사용자 전역 설정(훅·플러그인·MCP)을 불러오지 않습니다(`--setting-sources project --strict-mcp-config`). 실측으로 한 요청이 약 5만 토큰에서 약 8천 토큰으로 줄었습니다.
- 답은 마크다운으로 그리되 HTML은 실행하지 않습니다. 표·제목·굵게·구분선과 수식(KaTeX: `$…$`, `$$…$$`, `\(…\)`, `\[…\]`)을 지원하고, 숫자 앞 `$`(금액)는 글자로 둡니다. 링크는 http·https만 기본 브라우저로 엽니다.

## 스킬 · 플러그인

형식은 Claude Code와 같습니다. 데이터 폴더(`%APPDATA%\com.hoya.elysium`)에 있고, 앱의 "스킬 · 플러그인"에서 켜고 끄고, 폴더를 열고, 새 스킬을 만들 수 있습니다.

- **스킬**: `skills/<이름>/SKILL.md`. 머리말 `name`, `description`에 더해 Elysium이 쓰는 `engine`(claude·codex·gemini), `review`(true/false), `debate`(true면 토론), `tier`(light·standard·deep: 노력 단계 고정), `mode`(chat·work), `triggers`(자동 적용 낱말, 쉼표 구분)를 넣을 수 있습니다. 예시로 summarize·code-review·report·translate가 들어 있습니다.
- **기본 스킬**(`src-tauri/seed-skills/`, 앱에 넣어 두고 스킬 폴더에 만듦). 여러 공개 스킬에서 필요한 부분만 골라 한국어로 다시 썼습니다(원문을 옮기지 않음, 출처는 각 SKILL.md 끝).
  - 짧은 핵심 절차는 `SKILL.md`에, 자세한 점검표는 `references/*.md`에 둡니다. 스킬을 쓰면 참고 파일 목록이 지시문에 붙고, CLI(Claude Code `--add-dir`, Gemini CLI `--include-directories`)는 **필요한 파일만** 열어 읽습니다. API 엔진은 본문만 씁니다.
  - 판이 올라가면(현재 2판) 사용자가 고치지 않은 이전 판만 새 판으로 바꾸고, 고친 것은 그대로, 지운 것은 다시 만들지 않습니다(`skills/.seeded`).
  - `ui-craft`(참고 8개: 방향·토큰, 접근성 WCAG 2.2, 상호작용·모션, 글·상태·폼, 컴포넌트·라이브러리(shadcn·daisyUI·Magic UI·채팅·표), 숫자·차트, 반응형·성능, 리뷰·화면 확인): Anthropic frontend-design·theme-factory·webapp-testing·web-artifacts-builder, Vercel Web Interface Guidelines·React Best Practices, UI/UX Pro Max, Interface Design, shadcn·Magic UI·daisyUI 스킬, web-quality-skills, dataviz 원칙.
  - `perf-tune`(깊게, 참고 5개: 재는 방법, 웹, 서버·DB, 언어별, 보고 틀): 숫자로 정하기 → 기준선 → 병목(비중) → 하나씩 고치고 같은 방법으로 다시 재기.
  - `fact-check`(토론, 참고 4개: 주장 나누기, 근거 규칙, 토론 규칙, 결과 틀): 주장마다 1차 출처·반증 조건·자기 점검, 맞음/틀림/부분/불확실.
  - `root-cause`(깊게): 원인 조사 → 패턴 비교 → 가설 하나 → 실패하는 테스트 먼저 → 고친 뒤 원래 증상으로 증명(superpowers systematic-debugging·verification·TDD).
  - `token-saver`(가볍게, 한 파일): 결론부터, 줄이면 안 되는 것(코드·숫자·경고) 지키기, 바뀌는 코드만, 사용량 줄이는 엘리시움 설정 안내.
- **검토 안전장치**: 검토자에게 초안 AI가 확인한 자료(읽은 파일·검색)를 알려 주고, 볼 수 없는 자료를 이유로 지우지 말라고 지시합니다. 검토 답이 초안보다 지나치게 짧거나 '모름'만이면 버리고 초안을 씁니다(실측: 참고 파일에서 찾은 정답을 NVIDIA 검토가 '모름'으로 바꾸던 문제를 막음).
- **판정자**: 토론 판정은 후보(세 번째 회사 + 두 참가자) 중 가장 강한 AI가 맡습니다(사용자가 약한 AI를 골라도 판정은 강한 쪽).
- **API 과부하**: 200 응답 스트림 안에 온 오류(NVIDIA 무료 서버의 `ResourceExhausted` 등)를 읽어 "서버가 바쁩니다"로 알리고, 글자를 받기 전이면 2초·4초 뒤 다시 시도합니다(최대 3번).
- **플러그인**: `plugins/<이름>/.claude-plugin/plugin.json` + `skills/…`(+ commands·agents 등). 켠 플러그인은 Claude Code에 `--plugin-dir`로 넘겨지고, 그 안의 스킬은 다른 CLI에서도 쓸 수 있습니다. 직접 넣은 플러그인은 **꺼진 채로** 시작하고, 켤 때 확인을 받습니다(훅·MCP로 명령을 실행할 수 있기 때문). 예시 플러그인 `elysium-starter`(brainstorm 스킬)가 들어 있습니다.

## 화면

- **구조**: 창 전체가 입자 구의 공간입니다. 구·둘레 먼지·성운(WebGL, 약 12,000개 입자)이 창 뒤 전체에 깔리고, 도크·세션 패널·본문이 그 위에 떠 있습니다. 첫 화면에서는 구가 제목 위 빈 자리에 머물고, 대화를 열면 본문 뒤로 미끄러져 커지며 옅어집니다. AI가 일하면 구가 소용돌이치고, 허락을 기다리면 호박색으로 바뀝니다.
- **도크**(왼쪽 아이콘 기둥): 새 세션, 세션 목록 펼치기/접기(Ctrl+B), 사용량, 스킬·플러그인, 테마, 설정.
- **가운데 중심**: 본문과 구는 창의 가운데에 섭니다(오른쪽에도 도크 폭만큼 비움). 세션 패널은 본문을 밀지 않고 위에 떠 있고, 1560px보다 좁은 창에서는 세션을 고르면 접힙니다.
- **오른쪽 도구**(왼쪽 도크와 대칭인 도구 기둥 + 위에 떠 있는 패널, Ctrl+J로 열고 닫기, Esc로 닫기):
  - 세션 정보: 작업 폴더(열기·바꾸기), 질문 수·토큰·추정 비용·AI 시간, AI별 호출과 역할, 세션 기억 요약.
  - 목차: 이 세션의 질문 목록. 누르면 그 답 카드로 이동해 잠깐 빛나고, 맨 위·맨 아래 이동.
  - 찾기(Ctrl+F): 질문과 답에서 찾고, 찾은 낱말을 표시, Enter로 첫 결과로 이동.
  - 빠른 문구: 자주 쓰는 요청을 저장해 입력창에 넣기(기본: 핵심 요약, /fact-check, /root-cause, /perf-tune, /ui-craft). 이미 쓴 글은 지우지 않고, 스킬은 앞에·문장은 다음 줄에 붙습니다(이 PC에 저장).
  - 내보내기: 세션(질문·답·누가 답했는지·세션 요약)을 마크다운으로 복사하거나, 그 세션의 작업 폴더 안 "Elysium 내보내기"에 .md로 저장하고 탐색기에서 보여 줍니다.
  - 단축키(Ctrl+/): 단축키 목록.
- **세션 패널**: 세션마다 카드 한 장, 카드에 그 세션의 폴더가 보입니다. 작업 중인 세션은 빛이 흐릅니다. 삭제는 앱 안의 확인 창(무엇이 지워지는지 안내, 기본 초점은 취소)을 거칩니다.
- **사용량**(도크의 막대 아이콘): 오늘·7일·30일·전체 기간을 고르면 CLI·API·JEV 엔진마다 호출 수(초안/검토), 입력·출력 토큰, 추정 비용, 평균 시간, 실패, 마지막 사용이 보이고, 하루 단위 호출 막대가 있습니다. CLI 비용은 CLI가 알려 준 추정치입니다(구독 요금제면 실제 청구액과 다름). 기간은 현지 시각 0시 기준입니다.
- **라디오**(대화/작업, 검토, 기간, 스킬/플러그인 탭): 고른 칸 뒤의 판이 새로 고른 칸으로 미끄러져 갑니다. 왼쪽·오른쪽 화살표로도 고릅니다.
- **결과 카드**: 말풍선 대신 요청(머리) → **릴레이 리본** → 결과(본문) → 과정·복사(꼬리). 리본은 JEV 판단 → 초안 AI → 검토 AI를 노드로 잇고, 진행 중이면 선이 흐르고 노드가 맥박칩니다.
- **입력 캡슐**: AI 고르기(CLI·API 묶음), 대화/작업, 폴더 칩, `/` 스킬 자동 완성.
- 색: 회색 바탕에 청록(`#79b8b3`) 한 가지 강조. 화이트 모드는 바탕을 중간 회색(`#dfe2e6`)으로 두고 카드·패널·칩을 흰색으로 띄워 경계를 분명하게 했습니다. **다크·화이트 모드**(도크의 테마 버튼: 시스템 따라가기 → 다크 → 화이트, 이 PC에 기억). 구는 다크에서 가산 합성으로 빛나고 화이트에서 일반 합성으로 그립니다. 세션 머리·결과 카드·입력창은 같은 820px 기준선에 서고 컨트롤 높이는 32px로 맞췄습니다. 첫 화면에는 JEV → 초안 → 검토가 차례로 켜지는 릴레이 미리보기가 있습니다. 레일·카드·리본·메뉴·창·버튼·토글·알림 모두 등장·누름·전환 애니메이션이 있습니다(시스템의 "애니메이션 줄이기"를 켜면 멈춤).
- 앱 아이콘: `scripts/gen-icon.mjs`(남색 바탕 + 청록 구체 + 빛나는 아쿠아 작은 구) → `npx tauri icon app-icon.png`.

## 폰으로 원격 제어 (Elysium Remote, Android)

폰 앱에서 이 PC의 세션을 보고, 질문을 보내고(PC에서 실행되고 PC 화면에도 같이 보임), 멈추고, 작업 모드를 허락할 수 있습니다.

1. PC: 설정 → **폰 연결** → 원격 제어 켜기. 처음 켤 때 Windows 방화벽이 물으면 **개인 네트워크**를 허용하세요.
2. 폰: `elysium-remote.apk`를 설치하고 열어 **QR 찍기** → PC에서 **폰 연결하기**로 띄운 QR을 찍습니다.
3. 밖에서 쓰려면 PC와 폰에 [Tailscale](https://tailscale.com)을 설치해 같은 계정으로 로그인하세요(페어링할 때 Tailscale이 켜져 있으면 그 주소도 QR에 들어갑니다). 공유기 포트 포워딩(TCP 47820)도 됩니다.

보안
- 기본은 꺼짐. 켜면 TLS로만 받습니다. 인증서는 PC가 처음 만든 자체 서명 인증서이고, 폰은 QR에 담긴 SHA-256 지문과 같은 인증서만 믿습니다(지문이 다르면 연결하지 않음).
- 페어링 코드는 100비트 무작위, 5분 동안 한 번만 쓸 수 있고 8번 틀리면 닫힙니다. 기기마다 256비트 토큰을 주고 PC에는 해시만 둡니다. 설정에서 기기를 끊으면 그 토큰은 바로 막힙니다.
- 폰의 작업 모드 허락은 설정에서 끌 수 있습니다. 원격 요청·허락·페어링은 모두 감사 기록에 남습니다.

API(폰 앱이 쓰는 것): `POST /pair`, `GET /api/info|engines|conversations|approvals|usage`, `GET /api/conversations/{id}/messages`, `POST /api/send`, `POST /api/tasks/{id}/cancel`, `POST /api/approvals/{id}`, `GET /api/events`(WebSocket, PC 화면과 같은 이벤트).

폰 앱 빌드(`mobile/`, Tauri 2 Android, 상위 폴더의 node_modules를 같이 씀):

```powershell
$env:JAVA_HOME = 'C:\Program Files\Android\Android Studio\jbr'
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME = "$env:ANDROID_HOME\ndk\28.2.13676358"
cd mobile
# CLI는 절대 경로로 부른다(Gradle이 안쪽 폴더에서 같은 명령을 다시 부르기 때문)
node "$PWD\..\node_modules\@tauri-apps\cli\tauri.js" android build --apk --target aarch64
```

서명 키는 `C:\Users\HOYA\.elysium-build\keys`(OneDrive 밖, 저장소에 넣지 않음)에 있습니다. 빌드 산출물도 `.elysium-build`(mobile-target, android) 아래에 둡니다.

## 설정

CLI(확인·다시 찾기), API 키(저장·지우기·모델·확인), JEV(켜기·TypeSafe 키·판단 시험), 기본 작업 폴더, 검토 방식, 모든 세션 삭제.

## 구조

| 경로 | 내용 |
| --- | --- |
| `src-tauri/src/engines.rs` | 엔진 목록, CLI 찾기·실행, 출력 해석(Claude stream-json, Codex·Gemini JSON) |
| `src-tauri/src/apis.rs`, `secrets.rs` | API 엔진(OpenAI 호환·Anthropic 스트리밍), 키 보관(자격 증명 관리자) |
| `src-tauri/src/jev.rs` | TypeSafe Jev 클라이언트 |
| `src-tauri/src/orchestrator.rs` | 스킬·JEV 판단·AI 고르기, 허락, 초안 → 검토, 저장 |
| `src-tauri/src/skills.rs` | 스킬·플러그인 읽기·예시 넣기·새 스킬 틀 |
| `src-tauri/src/store.rs`, `db.rs` | SQLite: 대화·메시지·CLI 세션·승인·사용량·켜기/끄기 |
| `src-tauri/src/commands.rs` | 프런트엔드 명령 |
| `src/components/` | 도크·세션 패널(`SessionRail.tsx`), 결과 카드·릴레이 리본(`Thread.tsx`), 입력 캡슐, 설정, 스킬·플러그인 창, 사용량 창, 미끄러지는 라디오(`Segmented.tsx`), 창 전체 입자 배경(`ParticleOrb.tsx`) |

## 실측(2026-10-17, 앱에서 Claude Code 2.1.299)

| 시험 | 결과 |
| --- | --- |
| 짧은 질문 | 8.1초, 입력 6,819·출력 373 토큰, 추정 $0.031(구독 요금제에서는 실제 청구액 아님) |
| 작업 모드 | 허락 카드 → 허락 → `문서\Elysium\elysium-test.txt` 생성 확인(시험 뒤 삭제), 같은 대화 세션 이어 쓰기 확인 |
| 검토 | "Rust와 Go 비교": 초안 8.2초 + 검토 10.4초, 검토 전 초안 보존 |
| 노력치 | 위 표의 실측값. 앱에서 "안녕! 한 줄로 인사해 줘" → JEV 대화·난이도 0.0 → 가볍게 → Claude Code haiku·low 3.0초 $0.0015 |
| 세션 폴더 + JEV | 시험 폴더(파일 2개)로 세션 시작 → 두 파일을 읽고 비교, JEV "분석 95%·난이도 2.0" → 검토까지 |

## 남은 일

- Codex·Gemini CLI는 설치되지 않아 실제 실행을 확인하지 못했습니다(출력 형식은 단위 테스트만).
- 작업 모드에서 명령 실행(Bash) 허용은 넣지 않았습니다.
- API: Claude·OpenAI·Gemini는 키가 없어 실제 호출을 확인하지 못했습니다. NVIDIA는 사용자 키로 `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning`(기본, 실제 대화 확인·첫 호출 2~23초·가끔 503 과부하)과 `deepseek-ai/deepseek-v4.1-flash` 호출을 확인했습니다. 처음 저장된 키는 403이었고 다시 넣은 키는 정상, Gemma 4·Nemotron 3.5는 30초 안에 응답이 없었고 Nemotron 3 Ultra(`Function id … not found` 404, 같은 키·주소로 다른 모델은 정상이라 NVIDIA 쪽 문제로 보임)·Kimi K2.6은 이 계정에서 404였습니다. 거절된(401·403) API는 키를 다시 넣을 때까지 자동 선택에서 빠집니다.
- 예전 JARVIS의 `%USERPROFILE%\.jarvis-models`(17GB), `com.hoya.jarvis` 앱 데이터, `_archive`는 2026-10-17 휴지통으로 옮겼습니다.
- 프로젝트 폴더 이름은 아직 `Desktop\JARVIS`입니다.
#   E l y s i u m  
 