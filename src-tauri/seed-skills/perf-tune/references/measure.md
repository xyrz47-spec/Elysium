# 재는 방법

## 좋은 측정의 조건
- **같은 조건 반복**: 같은 데이터, 같은 기기·전원 모드, 다른 무거운 프로그램 끄기, 같은 빌드 종류(최적화 켠 릴리스 빌드).
- **데우기**: 첫 실행(캐시·JIT·디스크 캐시)은 따로 기록하고 버리거나 "콜드"로 따로 보고.
- **여러 번**: 최소 5번. 중간값(p50)과 꼬리(p95·p99), 최소·최대. 평균만 쓰지 않기.
- **편차 확인**: (최대−최소)/중간값이 10%를 넘으면 원인을 먼저 줄이기. 개선 폭이 편차보다 작으면 결론 내리지 않기.
- **현장 vs 실험실**: 현장(실제 사용자) 데이터는 영향의 크기를, 실험실(프로파일러·추적)은 원인을 알려 줍니다. 실험실 한 번은 사용자 경험을 대표하지 않습니다.
- **처리량과 지연을 구분**: "초당 몇 건"과 "한 건에 몇 ms"는 다른 문제입니다. 부하를 올리며 지연이 꺾이는 지점을 찾기.

## 도구(분야별)
| 분야 | 시간 | 원인 찾기 |
|---|---|---|
| 웹 화면 | Lighthouse, WebPageTest, `performance.now()`, web-vitals 라이브러리(현장) | Chrome DevTools Performance(긴 작업·레이아웃), React Profiler, 번들 분석기 |
| Node.js | `console.time`, autocannon·k6(부하) | `--cpu-prof`, clinic.js, 힙 스냅샷 |
| Python | `time.perf_counter`, `timeit`, pytest-benchmark | cProfile + snakeviz, py-spy(실행 중 표본), memray·tracemalloc(메모리) |
| Rust | criterion(벤치), `std::time::Instant` | cargo flamegraph, samply, `perf`, dhat(할당) |
| Go | `testing.B` | pprof(cpu·heap·block) |
| DB | 쿼리 실행 시간 로그, 느린 쿼리 로그 | `EXPLAIN ANALYZE`(Postgres), `EXPLAIN QUERY PLAN`(SQLite), 인덱스 사용 여부 |
| API·서비스 | k6, hey, wrk, 분산 추적(OpenTelemetry) | 구간별 타이머(요청 → 인증 → DB → 직렬화 → 응답) |
| 빌드 | 단계별 시간(`--timings`, `--profile`) | 캐시 적중률, 병렬도, 가장 긴 단계 |

## 여러 부품이 이어진 시스템에서
- 부품 경계마다 들어오고 나가는 시간(그리고 크기)을 한 번 찍어서 **어디서** 시간이 쓰이는지 먼저 알아내세요. 그다음 그 부품 안을 프로파일.
- 네트워크 왕복 횟수를 세세요. 왕복 20번 × 50ms = 1초.

## 기록 틀
```
측정 대상: …            조건: 데이터 …, 기기 …, 빌드 …
방법: (명령/스크립트)    반복: 데우기 2회 + 측정 7회
결과: p50 … / p95 … / 최소 … / 최대 …
```
