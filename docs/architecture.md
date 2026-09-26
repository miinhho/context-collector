# ContextCollector 런타임 아키텍처

## 목적과 경계

ContextCollector는 장기 실행되는 Agent의 작업 Context를 관리하는 Runtime이다. 새 Context를 성급하게 변형하지 않고, 사용과 수명을 관찰한 뒤 표현과 운영 위치를 조정한다. 현재 작업의 연속성을 지키면서 누적된 Context를 정확히 다시 찾고 불러올 수 있게 하는 것이 목적이다.

ContextCollector는 Context의 Memory 가치나 Memory identity를 결정하지 않는다. 모든 내용의 임베딩 검색, 의미 관계 추론, 일반적인 Agent 도구 결과 보관도 코어 도메인의 책임이 아니다.

하나의 Context에는 서로 독립적인 세 관점이 있다.

| 관점 | 의미 |
| --- | --- |
| Scope | 어느 논리적 작업에 속하는가 |
| Lifecycle | 현재 어느 운영 위치에 있는가 |
| Representation | Raw인가 Structured인가 |

한 관점의 변화가 다른 관점의 변화를 뜻하지 않는다. 수명이 길거나 Cold에 있다는 사실은 의미적 중요도나 Memory 가치를 뜻하지 않는다.

## 런타임 객체

```text
ContextHeap
  ├─ EdenZone
  ├─ SurvivorZone
  ├─ MatureZone
  ├─ CoolingZone
  └─ ColdZone

각 Zone: ContextHeapSpace 공통 기반
  └─ ScopeBlock들 → ContextObject들(RawObject / StructuredObject)

Scope: Zone들을 가로지르는 ContextObject의 논리적 소속

Zone 점유와 turn 관측 → CollectionScheduler
  ├─ Collection: Zone 사이의 이동
  └─ Compaction: Objectization과 Cold 보관 정리
```

### ContextHeap과 Zone

`ContextHeap`은 다섯 Zone을 묶는 런타임 작업공간이다. 객체별 이동 정책이나 Objectization 판단을 중앙에서 소유하지 않는다.

`ContextHeapSpace`는 Zone의 공통 기반이다. 각 Zone은 현재 배치된 객체를 보관하고 조회하며, Scope별 객체 묶음과 token 점유를 계측한다. Zone은 watermark 압력을 알리지만 스스로 Collection이나 Compaction을 실행하지 않는다. 객체의 현재 Zone은 Zone의 소속 관계로 표현하며, Heap과 ContextObject에 별도의 변경 가능한 배치 목록을 두지 않는다.

### Scope와 ScopeBlock

`Scope`는 논리적 작업의 소유 경계이며, 그 작업에 속한 ContextObject들의 묶음을 가진다. 한 Scope의 객체는 여러 Zone에 걸쳐 있을 수 있다. 다른 Scope에서 객체를 참조하거나 사용해도 원래 Scope 소속은 바뀌지 않는다. Runtime의 현재 Scope 전환은 명시적으로 처리하며 기존 객체의 소속을 자동으로 바꾸지 않는다.

각 Zone은 같은 Scope의 객체를 인접하게 탐색할 수 있도록 `ScopeBlock`으로 묶는다. ScopeBlock은 Zone 내부의 배치·탐색 구조다. 한 Scope가 한 Zone에 여러 Block을 가질 수 있고, 작업은 Block 안의 일부 객체만 처리할 수 있다. Block은 독립된 도메인 소유권, lifecycle, 영속적 identity 또는 원자적 이동 단위가 아니다.

Scope는 Collection과 Compaction이 후보를 모으는 공통 경계다. Scope 전환은 후보 선정의 근거이지만, 전환만으로 Scope 전체를 Cooling으로 옮기지는 않는다.

### ContextObject

`ContextObject`는 Runtime이 관리하는 정보 단위다. 내용과 식별, revision을 가지며 자신의 Zone 이동을 결정하거나 실행하지 않는다.

- `RawObject`는 Runtime이 받아들인 정확한 원본이다. 사용자·Agent의 새 상호작용은 일반적으로 Raw로 Eden에 들어간다. 원본 payload는 compaction이나 Zone 이동으로 잘리거나 덮어써지지 않고 정확히 다시 읽을 수 있다.
- `StructuredObject`는 충분히 관찰된 Raw에서 추출한, 독립적으로 활용할 수 있는 작업 객체다. 단순한 축약문이 아니며 근거가 된 Raw 부분과 revision을 추적한다.

새 출력 직후에는 무엇을 작업 객체로 추출할지 알기 어렵다. 매 출력마다 연속된 Objectizer 호출로 즉시 구조화하면 비용이 들고 정보가 빠질 수 있다. Raw는 먼저 수명을 거쳐 관찰된다. Structured가 생겨도 Raw는 정확히 보존되며, 부분 추출이나 보류가 나머지 Raw를 대체하지 않는다.

Raw와 Structured는 각각 Hot 또는 Cold에 있을 수 있다. 둘 다 Cooling 후보가 될 수 있고, 함께 생성됐다는 이유로 동시에 이동하지 않는다.

## Lifecycle과 배치

```text
Eden → Survivor → Mature → Cooling → Cold
|---------------- Hot ----------------|
```

| Zone | 의미 |
| --- | --- |
| Eden | 새 Context를 원본 그대로 받아들이는 구간 |
| Survivor | Minor collection 뒤에도 Hot에 남아 수명을 더 관찰하는 구간 |
| Mature | 여러 collection을 거치며 장기 작업에 계속 필요한 것으로 관측된 Hot 구간 |
| Cooling | Cold 이동을 준비하지만 계속 읽을 수 있는 Hot 구간 |
| Cold | 기본 Agent view에서는 빠지지만 같은 Workspace에서 탐색·재호출할 수 있는 구간 |

Cooling의 준비 작업이 실패하거나 오래 걸려도 기존 Context 접근은 끊기지 않는다. Cold는 삭제나 Memory 승격이 아니다.

## Agent 보고와 Context view

Runtime은 요청마다 `TokenSpace` 안에서 `ContextView`를 구성한다. 현재 Scope의 필요한 Hot Raw·Structured, 최근 또는 명시적으로 사용된 Context, 간략한 Cold catalog, 명시적으로 불러온 Cold Context를 포함할 수 있다. Raw는 실제 payload로, Structured는 객체화된 표현과 출처로 전달한다. TokenSpace는 한 요청의 view 크기이며 Zone별 token 점유와 다르다.

Agent는 메시지와 함께 turn 단위의 `uses`와 `scope`를 보고한다. Runtime은 이를 `TurnObservation`으로 받아들인다. 이 보고는 추가 Objectizer 호출 없이 얻는 관측이며 lifecycle 이동 명령은 아니다.

- `uses`는 실제 사용했다고 보고한 ContextObject의 식별이다. 다른 Scope의 객체를 사용해도 소유권은 바뀌지 않는다. 목록에 없거나 보고가 없다는 사실만으로 미사용이라고 판단하지 않는다.
- `scope`는 이번 turn과 요청 시작 시점의 현재 Scope 사이의 관계다. `CONTINUE`는 같은 논리적 작업의 연속, `TRANSITION`은 다른 논리적 작업으로 넘어가는 명시적인 경계 관측, `UNCERTAIN`은 판단하기 어려움을 뜻한다. Agent는 Scope ID를 정하지 않는다. 전환 보고만으로 기존 객체의 소속을 옮기지 않고, 불확실하거나 보고가 없다는 이유로 전환을 추정하지 않는다.

보고는 응답과 함께 도착하므로 이미 구성된 이번 요청의 view에는 소급 적용되지 않는다. Runtime은 사용자·Agent의 Raw를 수용하고 관측을 갱신한 뒤, 다음 view와 Collection·Compaction 판단에 보고를 반영한다.

## Watermark와 작업

각 Zone은 Raw와 Structured를 포함한 token 점유와 `high`·`low` watermark를 가진다. Raw·Structured별 점유도 계측해 어떤 작업이 압력을 해소할 수 있는지 판단한다. `high` 도달은 작업 실행을 검토할 신호이고, `low`는 안전한 후보가 있을 때의 목표다. Scheduler는 Zone 점유와 Runtime 상태를 보고 Collection 또는 Compaction을 예약한다. 고정된 turn 번호나 Scope 전환만으로 작업을 시작하지 않는다.

Hot 내부의 이동은 한 Zone의 점유를 낮춰도 Hot 전체 점유를 줄이지 않을 수 있다. Scheduler는 각 Zone의 압력과 Hot Zone 합계도 관찰한다. 안전한 후보가 없거나 준비가 실패하면 `low`에 도달하지 못할 수 있다. 그 이유로 원본을 손실시키거나 보호 중인 Context를 강제로 이동시키지 않는다.

Collection과 Compaction은 ScopeBlock으로 같은 Scope의 후보를 탐색하지만 Block 전체를 한꺼번에 처리할 의무는 없다.

### Minor collection

Minor collection은 Eden과 Survivor의 Context를 구조적 사실에 따라 Survivor 또는 Mature로 이동시킨다. 생존 기간, 명시적인 사용, 보호 중인 접근을 이용하며 새로운 의미를 만들지 않는다. Objectizer 호출은 필요하지 않다.

### 공통 Objectization과 Hot compaction

`Objectization`은 Raw에서 Structured를 추출하는 공통 작업이다. 교체 가능한 외부 Objectizer가 제안한 결과의 Raw 출처와 현재 revision을 검증한다. 여러 객체로 나눈 결과, 일부만 처리한 결과, 전체 보류를 허용한다. 오래되거나 근거가 맞지 않는 결과는 canonical Context로 받아들이지 않는다.

Hot compaction은 Hot 압력이 커지고 충분한 수명을 거친 Raw가 있을 때, Scope를 기준으로 Raw 집합을 골라 Objectization을 사용한다. 받아들인 Structured는 같은 Scope의 Hot Context가 된다. Objectization 자체는 Zone 이동이나 Memory 가치 판정을 맡지 않는다.

### Scope를 기준으로 한 Cooling 이동

Hot Zone의 watermark로 이동 작업을 검토할 때 Collection은 Scope의 객체 묶음과 Zone의 ScopeBlock을 함께 본다. Scope 전환 보고, 명시적인 `uses`, Context의 수명, 보호 중인 접근, Objectization 결과를 후보 판단의 근거로 사용한다.

RawObject와 StructuredObject 모두 Cooling으로 이동할 수 있다. `uses`의 부재만으로 대상을 정하지 않으며 현재 작업에 필요한 Context 접근을 유지한다. Scope의 일부 객체만 이동할 수 있고 Scope 소속은 유지된다.

### Major collection

Major collection은 Cooling Context의 정확한 payload를 Cold에서 접근할 수 있게 준비하고 Hot 점유를 해소한다. 의미를 생성하지 않는다. 준비 중에는 기존 Cooling 객체가 계속 읽혀야 하며 안전하게 완료된 시점에만 Cold 이동을 확정한다.

### Cold compaction과 ColdBacking

Cold compaction은 ColdZone의 watermark를 계기로, Zone이 직접 보유하는 payload의 점유를 관리한다. `ColdCompactor`는 Cold Context를 교체 가능한 `ColdBacking`에 보관하고 catalog를 통해 탐색·재호출할 경로를 유지한다. 정확한 보관과 재호출이 준비되기 전에는 기존 Cold 접근을 해제하지 않는다. Agent의 현재 응답을 막지 않는 비동기 작업이다.

ColdBacking의 첫 구현은 in-memory map이다. 이는 저장·탐색 계약을 안정적으로 구체화하고 구현을 교체할 수 있게 하기 위한 선택이다. 파일 기반 map 등으로 바뀌어도 정확한 payload 보존과 재호출 규칙은 같다. Backing에 보관해도 Context의 lifecycle은 Cold이고, 같은 Workspace에서 찾을 수 있다.

ColdCompactor는 필요하면 공통 Objectization으로 Cold Raw에서 Structured를 추출해 탐색 가능성을 개선할 수 있다. Raw 원본은 계속 보존한다. Catalog나 backing을 만드는 일은 장기 Memory 시스템으로 승격하거나 Memory 가치를 판단하는 일이 아니다.

## 외부 경계와 불변식

Objectizer 구현, token 계산기, Agent/provider adapter, ColdBacking 구현, catalog 검색 방식, Context wire format은 교체 가능한 외부 경계다. 외부 구현은 결과를 제안하거나 payload를 보관할 수 있지만 Scope 소속과 lifecycle을 임의로 변경하지 않는다.

- Raw payload는 정확하게 다시 읽을 수 있다.
- 새 Raw를 선제적으로 요약하거나 객체화하지 않는다.
- Scope 소속은 다른 Scope의 참조나 Zone 이동으로 바뀌지 않는다.
- `uses` 부재를 미사용의 증거로 취급하지 않는다.
- Scope 전환이나 watermark 도달만으로 Context를 강제로 Cooling으로 보내지 않는다.
- Structured는 정확한 Raw 근거를 가지며 부분 추출은 전체 Raw를 대체하지 않는다.
- 비동기 준비가 끝나기 전에 canonical Context의 접근이나 lifecycle을 바꾸지 않는다.
- Cold Context는 catalog로 탐색하고 정확한 payload를 다시 불러올 수 있다.
- ContextCollector는 Memory 가치와 Memory identity를 결정하지 않는다.
