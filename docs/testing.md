# 런타임 동작 검증

## 판정 범위

`tests/`는 공개 Runtime API에 합성 turn 보고와 가짜 Objectizer·Summarizer·Backing을 주입한다. 실제 Agent가 생성한 보고나 실제 모델 출력의 의미 품질은 검증하지 않는다. `tests/llm_client.rs`의 HTTP 검사는 로컬 서버의 고정 응답만 사용하며 외부 LLM을 호출하지 않는다.

## 기계적 계약

| 검사 파일 | 확인하는 경계 |
| --- | --- |
| `tests/runtime_contract.rs` | 64개 합성 Scope 보고 시퀀스의 turn 배정과 Raw 접근, 모르는 `uses` 거부, watermark 이후 Structured 생성, Raw·Structured 계측, ColdCatalog와 Backing을 통한 재호출 및 Agent용 View |
| `tests/failure_contract.rs` | 근거 없는 Structured 제안, Scope 밖의 요약 근거, 저장·정확 재호출 실패, 비동기 준비 중 보호 상태 변경에서 canonical payload 유지 |
| `tests/llm_client.rs` | 작업별 모델·토큰 설정 전달, LLM JSON 결과 파싱 실패, OpenAI Responses 형식의 HTTP 요청, 즉시 Objectization을 호출하지 않는 시점 |
| `tests/token_counter.rs` | `o200k_base` 토큰 계측이 바이트 길이와 다른 입력 |

불변식 검사는 공개 조회 결과와 실패 시 상태 보존을 기준으로 한다. ScopeBlock의 내부 배열 배치, Agent 보고의 의미적 타당성, Structured 추출의 유용성, Scope 요약의 충실성은 이 검사로 판정하지 않는다. 실제 모델 turn을 이용한 평가는 Agent와 LLM 서비스를 연결했을 때 별도로 수행해야 한다.
