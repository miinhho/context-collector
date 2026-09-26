# 런타임 계약 검사

`tests/`는 공개 Runtime API에 합성 turn 보고와 사용자 구현
InfoRefiner·ScopeSummarizer·Backing을 주입한다. 실제 Agent 보고나 모델 출력의
의미 품질은 검증하지 않는다.

- `tests/runtime_contract.rs`: 합성 Scope 보고의 turn 배정, RawInfo 접근,
  모르는 `uses` 거부, watermark 이후 Info 생성, 형태별 token 계측,
  Cold 요약의 일부 반영 범위, Catalog·Backing 재호출과 Agent View.
- `tests/failure_contract.rs`: 근거 없는 Info 제안, Scope 밖 요약 근거,
  저장·재호출 실패, 비동기 가공·Cold 준비 중 보호 상태 변경에서 원본 유지.
- `tests/custom_work.rs`: 사용자 구현의 Scope·Zone 입력, 타입 있는 Info,
  중복 생성 방지, Backing·Scope 요약의 데이터 왕복과 외부 오류 출처 보존.
- `tests/info_lifecycle.rs`: Cold watermark 이전 Scope 요약, 반복된 요약 부재의
  실패 상태와 정확한 원본 Backing, 완료된 RawInfo의 중복 Hot 가공 방지.
- `tests/token_counter.rs`: `o200k_base` token 계측과 바이트 길이가 다른 입력.
- `tests/agent_view.rs`: Heap 용량에 따른 Hot 대화의 보존과 역할·turn 순서,
  Zone별 View section 점유, Backing 정보의 `uses` 관측과 Scope 조회,
  명시적 Backing 재호출의 별도 전달량.

검사는 공개 조회 결과와 실패 시 상태 보존을 본다. ScopeBlock의 내부 배치,
Agent 보고의 의미적 타당성, Info 가공의 유용성, Scope 요약의 충실성은 이
검사로 판정하지 않는다. 실제 모델 turn을 이용한 평가는 Agent와 LLM 서비스를
연결했을 때 별도로 수행해야 한다.
