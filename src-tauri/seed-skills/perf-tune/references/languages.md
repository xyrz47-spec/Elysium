# 언어별 자주 쓰는 처방

## 공통
- 알고리즘 먼저: O(n²) → O(n log n)·O(n)이 어떤 미세 최적화보다 큽니다. 중첩 반복 안의 찾기는 해시(Map/Set/dict)로.
- 같은 계산 반복 없애기(반복문 밖으로, 메모이제이션).
- 할당·복사 줄이기, 큰 데이터는 스트리밍(한 번에 다 메모리에 올리지 않기).
- I/O는 묶어서(버퍼, 배치).

## Rust
- 릴리스 빌드로 재기(`--release`, 필요하면 `lto`, `codegen-units=1`).
- 불필요한 `clone()`·`to_string()`·`collect()` 줄이기, 빌림(`&str`, `&[T]`)·`Cow` 사용.
- `Vec::with_capacity`, 반복자 체인, 문자열 이어 붙이기는 `String::with_capacity`/`write!`.
- 잠금 경합: 잠금 범위 줄이기, `RwLock`, 채널, 샤딩. async에서 오래 막는 작업은 `spawn_blocking`.
- 프로파일: cargo flamegraph·samply, 할당은 dhat.

## Python
- 프로파일 먼저(cProfile, py-spy). 뜨거운 반복문은 내장 함수·컴프리헨션·numpy/pandas 벡터화로.
- 문자열 이어 붙이기는 `"".join`, 찾기는 set/dict, 큰 파일은 줄 단위·청크 단위.
- I/O 대기가 많으면 asyncio·스레드, CPU 계산은 multiprocessing(또는 numba·Cython·Rust 확장).
- pandas: `apply` 대신 벡터 연산, 알맞은 dtype(category·int32), 필요한 열만 읽기.

## JavaScript / Node.js
- 이벤트 루프 막지 않기: 무거운 동기 계산은 Worker, 큰 JSON은 스트리밍.
- 객체 모양 일정하게(속성 추가·삭제 반복 피하기), 큰 배열의 `splice` 반복 피하기.
- 찾기는 Map/Set, 여러 번 도는 `filter().map()`은 한 번의 반복으로(뜨거운 경로에서만).
- 메모리: 힙 스냅샷, 지우지 않은 리스너·타이머 확인.

## Go
- pprof로 CPU·힙·블로킹 확인, 할당 줄이기(슬라이스 용량 미리, `strings.Builder`, `sync.Pool`은 측정 후).
- 고루틴 누수(끝나지 않는 고루틴) 확인, 채널 버퍼 크기.

## 빌드·테스트 속도
- 단계별 시간부터(어느 단계가 긴지). 캐시(의존성·증분 빌드) 적중 확인.
- 병렬 실행, 바뀐 것만 빌드·테스트, 무거운 테스트는 따로 표시.
- 빌드 산출물을 동기화 폴더(OneDrive 등) 밖에 두기.
