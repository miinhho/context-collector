# 런타임 동작 검증

## 판정 범위

Runtime 테스트는 합성 입력과 상태 전환으로 판정할 수 있는 계약을 검사한다. 실제 모델을 호출하거나 Agent가 수행한 turn을 재현하지 않는다. Agent의 `scope` 보고가 의미상 옳은지, Structured 추출이 유용한지, Scope 요약문이 충실한지는 Runtime이 알 수 없다. Runtime은 주입된 값의 시점, 출처, revision, Scope 경계, 보관·재호출을 검증한다. 의미 품질 평가는 실제 Agent·Objectizer·Summarizer를 연결할 때 별도 평가 대상으로 둔다.

## 공통 검사기

`src/runtime/invariant_tests.rs`의 검사기는 주요 작업 전후에 다음 조건을 동일하게 적용한다.

- 객체 ID는 정확히 한 Zone 또는 Backing에 존재하고, 정확히 한 Scope에 소속된다.
- Zone의 ScopeBlock과 실제 객체 집합이 같으며 Raw·Structured별 token 합계가 Zone 계측과 같다.
- ColdZone 객체는 ColdCatalog에 ColdZone 위치로 등록되고, Backing 항목은 어떤 Zone에도 남지 않으며 정확히 다시 읽힌다.
- Scope 요약의 근거 ID와 revision은 해당 Scope의 Catalog 항목과 일치한다. 요약 문장의 의미는 판정하지 않는다.

## 결정적 시나리오

| 경계 | 입력·조작 | 기계적으로 확인할 결과 |
| --- | --- | --- |
| 수동 주입한 보고와 Scope | 가짜 사용자·Agent 문자열과 `CONTINUE`·`TRANSITION`·`UNCERTAIN`·보고 없음의 3회 호출 조합 64개 | 보고 값에 따른 Scope 배정, 이전 객체 소속 불변, 작업 뒤 유일한 위치와 원본 payload |
| watermark와 이동 | 낮은 watermark로 예약 작업을 순서대로 실행 | 보호된 객체 보존, 대상 Zone 계측, 다음 작업 예약, 유한한 작업 종료 |
| Representation | 고정된 Structured 제안을 수용 | Raw 보존, 같은 Scope·Zone 생성, Raw·Structured token 각각 반영 |
| 늦은 Objectization | 제안 준비 뒤 출처 객체 이동 | 오래된 제안 거부, 기존 객체 접근 유지 |
| Cold 후보 선정 | 서로 다른 두 Scope를 Cold에 배치 | Scope별 batch의 ID가 섞이지 않고 한 Scope 이관이 다른 Scope를 변경하지 않음 |
| 요약 결과 수용 | 고정 문자열과 근거 ID를 반환하는 가짜 Summarizer | 선택된 cohort 밖의 근거 거부, 수용한 문자열·근거·revision 범위의 정확한 전달 |
| 비동기 확정 | 준비 뒤 보호 상태 변경, 저장 실패 또는 불일치 재호출 | 확정 거부, ColdZone payload와 Catalog 위치 유지 |
| Agent용 view | token 예산과 명시적 Cold ID 제공 | 예산 초과 없음, Scope 단위 투영, 원본의 정확한 재호출 |

고정 문자열을 반환하는 가짜 Summarizer는 요약 내용의 품질을 검증하는 수단이 아니다. 요약 결과가 잘못된 Scope나 revision에 연결되지 않는지만 확인한다. 전체 Agent workflow 검증은 이 기계적 계약 위에서 별도로 수행한다.
