# ContextCollector 런타임 아키텍처

## 목적과 경계

ContextCollector는 장기 실행 Agent의 작업 Context를 관리한다.
새 정보를 성급하게 가공하지 않고, 사용과 수명을 관찰한 뒤 정보의 표현과
운영 위치를 조정한다. 현재 작업의 연속성을 지키면서 누적된 정보를 정확히
다시 찾고 읽을 수 있게 한다.

ContextCollector는 Memory 가치나 identity를 결정하지 않는다. 임베딩 검색,
의미 관계 추론, 일반적인 도구 결과 저장은 코어 도메인의 책임이 아니다.

하나의 정보에는 독립적인 세 관점이 있다.

| 관점 | 의미 |
| --- | --- |
| Scope | 어느 논리적 작업에 속하는가 |
| Lifecycle | 현재 어느 운영 위치에 있는가 |
| 정보 형태 | 수용한 원본 RawInfo인가, RawInfo에서 가공한 Info인가 |

한 관점의 변화가 다른 관점의 변화를 뜻하지 않는다. Cold에 있다는 사실만으로
Memory 가치가 생기지 않으며, Info가 생겨도 원본 RawInfo가 소멸하지 않는다.

## 런타임 관계

```text
ContextHeap
  ├─ EdenZone
  ├─ SurvivorZone
  ├─ MatureZone
  ├─ CoolingZone
  └─ ColdZone
각 Zone: ContextHeapSpace 공통 기반
  └─ ScopeBlock들 → ContextItem들(RawInfo / Info)

Scope: Zone들을 가로지르는 정보의 논리적 소속
ColdCatalog: Cold 정보의 위치, 처리 상태, Scope 요약과 근거 범위
ColdBacking: Heap 바깥의 보관·재호출 경계

Zone 점유와 turn 관측 → CollectionScheduler
  ├─ Collection: Zone 사이의 이동
  ├─ Refinement: RawInfo에서 Info 가공
  ├─ Cold Scope 요약
  └─ ColdCompactor: ColdZone → ColdBacking
```

`ContextHeap`은 다섯 Zone을 묶는 런타임 작업공간이다. Zone별 정책과 가공
판단을 중앙에서 소유하지 않는다. `ContextHeapSpace`는 각 Zone의 공통 기반이다.
Zone은 배치된 정보를 보관·조회하고 Scope별 묶음과 RawInfo·Info의 token 점유를
계측한다. Watermark 압력을 알리지만 Collection이나 Compaction을 직접 실행하지
않는다. Backing으로 내보낸 정보는 어느 Zone에도 남지 않는다.

`Scope`는 논리적 작업의 소유 경계다. 한 Scope의 정보는 여러 Zone에 걸칠 수
있고, 다른 Scope에서 참조해도 원래 소속은 바뀌지 않는다. 각 Zone의
`ScopeBlock`은 같은 Scope의 정보를 인접하게 탐색하는 내부 구조다. 한 Scope가
한 Zone에 여러 Block을 가질 수 있으며, Block은 독립된 소유권·lifecycle·
영속적 identity·원자적 이동 단위가 아니다.

`ContextItem`은 정보 한 단위의 ID, revision, 정보 형태와 가공 상태를 담는
런타임 기록이다. 자신의 Zone 이동이나 가공을 결정하지 않는다.

- `RawInfo`는 Runtime이 수용한 정확한 원본이다. 새 사용자·Agent 출력은
  일반적으로 Eden에 RawInfo로 들어간다. 이동과 가공 뒤에도 원본 payload를
  정확하게 다시 읽을 수 있다. 대화 메시지의 역할과 turn 순서는 정보와 함께
  보존되어 Zone 이동과 Backing 뒤에도 복원할 수 있다.
- `Info`는 RawInfo의 근거 구간에서 사용자가 가공한 정보다. 짧은 요약,
  추출, 재표현 등 목적에 맞는 결과가 될 수 있다. 독립적인 Memory 객체나
  정해진 필드 추출 스키마를 요구하지 않는다.

Info에는 표시할 내용, RawInfo의 근거와 revision, 사용자 정의 타입의 `data`가
있다. Runtime은 `data`의 의미를 해석하지 않고 Zone 이동·Backing·명시적
재호출에서 보존한다. `data`는 Scope 소속이나 lifecycle을 바꾸는 권한이 아니다.
기본 Agent View에는 `data`를 싣지 않는다.

새 출력 직후에는 무엇을 가공할지 알기 어렵다. 매 출력마다 연속된 LLM 호출로
정보를 즉시 추출하면 비용이 들고 원본의 정보가 빠질 수 있다. RawInfo를 먼저
관찰하고, 유리한 시점에 Info를 만든다. 부분 가공이나 보류는 원본의 나머지를
대체하지 않는다. RawInfo와 Info는 각각 Hot 또는 Cold에 있을 수 있고,
생성 관계만으로 함께 이동하지 않는다.

## Lifecycle과 Agent 관측

```text
Eden → Survivor → Mature → Cooling → Cold → ColdBacking
|---------------- Hot ----------------|
```

| Zone | 의미 |
| --- | --- |
| Eden | 새 원본을 받아들이는 구간 |
| Survivor | Minor collection 뒤 수명을 더 관찰하는 Hot 구간 |
| Mature | 여러 collection을 거친 Hot 구간 |
| Cooling | Cold 이동을 준비하지만 계속 읽을 수 있는 Hot 구간 |
| Cold | 기본 View에서는 빠지고 탐색·재호출할 수 있는 구간 |

Backing은 Zone이 아니다. ColdZone 밖으로 내보낸 payload의 보관·재호출 경계다.
준비 작업이 실패하거나 오래 걸려도 기존 정보의 접근은 끊기지 않는다.

Agent는 메시지와 함께 turn 단위의 `uses`와 `scope`를 보고한다. Runtime은 이를
`TurnObservation`으로 수용한다. 보고는 관측 근거이며 이동 명령이 아니다.

- `uses`는 실제 사용했다고 보고한 정보 ID다. 목록에 없거나 보고가 없다는
  사실만으로 미사용이라고 판단하지 않는다. 다른 Scope의 정보를 사용해도
  소유권은 바뀌지 않는다.
- `scope`는 이번 사용자 입력과 Agent 응답으로 이뤄진 turn과 현재 Scope의
  관계다. `CONTINUE`는 현재 Scope를 유지한다. `TRANSITION`은 Runtime이 새
  Scope를 만들어 이번 RawInfo를 소속시킨다. `UNCERTAIN`은 현재 Scope를
  유지하되 Cooling 선정의 긍정적 근거로 쓰지 않는다. 보고 부재로 전환을
  추정하지 않으며 Agent가 Scope ID를 정하지 않는다.

보고는 응답과 함께 도착하므로 이번 요청의 View에 소급 적용되지 않는다.
다음 View와 Collection·Compaction 판단에 반영한다.

### Agent에게 전달하는 Context

View는 Runtime의 Zone·정보 형태·Scope 항목을 그대로 나열하지 않는다.
선정된 원본 대화 메시지는 역할과 turn 순서를 지켜 전달하고, 관찰 뒤 얻은 Info와
Cold 부분의 Scope 요약은 이전 대화를 이어주는 Markdown 내용으로 제공한다.
표시된 정보와 근거에는 `#ID` 참조를 붙여 정확한 원본을 다시 읽을 수 있게 한다.
현재 입력은 호출자가 요청에 함께 전달하며, 완료된 turn의 사용자·Agent 출력은
Runtime의 RawInfo에서 다음 View를 구성한다.

Scope 소속과 명시적 `uses`는 관련 내용을 선정하는 근거다. Hot·Cold와 Backing
위치는 접근 경로에 사용하며 Agent용 문구에 노출하지 않는다.
Info의 근거 구간이나 Scope 요약의 반영 범위가 원본 전체의 의미적 대체를
보증하지는 않는다. 원본이 View에 실리지 않아도 정확히 조회할 수 있어야 한다.
Cold와 Backing에 있는 정보의 `uses`도 ColdCatalog에 관측으로 기록하지만
보고했다는 이유만으로 Zone을 바꾸거나 본문을 다음 View에 강제로 포함하지 않는다.
정보의 token 점유와 압력은 Heap의 Zone별 watermark와
Collection·Compaction이 관리한다. View는 별도의 예산으로 Hot 내용을
제외하거나 저장 상태를 바꾸지 않는다.
각 Zone의 정보가 기본 View에서 차지하는 내용은 대응하는 View section에서
계산한다. 선택된 Info가 RawInfo를 참조하면 기본 View에는 Info와 원문 참조를
두고, 정확한 RawInfo 본문은 조회할 때 읽는다. Section의 token 점유는 실제
Markdown 표현으로 계측하며 같은 시점의 Zone 점유와 watermark를 함께 볼 수
있다. 여러 Zone의 대화는 최종 View에서 turn과 역할
순서로 합친다. Section은 내부 전달 계측이며 Agent에게 Zone 이름으로 노출되지
않는다. Backing에서 명시적으로 재호출한 내용의 전달량은 별도로 계측한다.
Scheduler는 Heap의 watermark와 안전한 후보를 기준으로 동작한다.
호출자는 최근 사용 정보, Scope의 요약과 정보 참조, 개별 정보 및 Info의 원문
근거를 필요할 때 조회할 수 있다. 조회 결과도 저장 위치를 드러내지 않는
Markdown으로 표현하며, 원본 내용과 사용자 정의 데이터의 정확한 조회 계약은
별도로 유지한다.

## Watermark와 실행 책임

각 Zone은 RawInfo와 Info를 합한 token 점유와 `high`·`low` watermark를 가진다.
형태별 점유도 계측한다. `high`는 작업을 검토하는 신호이고 `low`는 안전한
후보가 있을 때의 목표다. Scheduler는 Zone 점유와 관측 상태로 작업을 예약한다.
고정된 turn 번호나 Scope 전환만으로 이동하지 않는다. Hot 내부 이동은 Hot 전체
점유를 낮추지 않을 수 있으므로 Zone별 압력과 Hot 합계도 본다.

안전한 후보가 없거나 준비가 실패하면 `low`에 도달하지 못할 수 있다. 그 이유로
원본을 버리거나 보호 중인 정보를 강제로 이동시키지 않는다. 작업 뒤에는
변경된 점유를 보고 후속 작업을 다시 검토한다. ScopeBlock은 같은 Scope의 후보를
찾는 수단이며 Block 전체를 처리할 의무는 없다.

### Collection과 Hot 정보 가공

Minor collection은 Eden과 Survivor의 정보를 구조적 사실에 따라 Survivor 또는
Mature로 이동한다. 생존 기간, 명시적 사용과 보호 중인 접근을 이용하고 새 의미를
만들지 않는다.

Hot watermark 압력이 커지면 Refinement가 수명을 거친 같은 Scope·Zone의
RawInfo 후보를 고른다. 호출자가 구현한 `InfoRefiner`가 Info 내용, 원본 근거와
사용자 데이터를 제안한다. Runtime은 근거 구간, revision, Scope, Zone과 보호
상태를 확인하고 출처와 같은 Scope·Zone에 Info를 만든다. Refiner는 선정된
RawInfo마다 이번 Zone에서 가공을 마쳤는지도 명시할 수 있다. 보류된 RawInfo는
원본으로 남으며, 다음 관측 이후 다시 후보가 될 수 있다.

Cooling 이동은 Hot watermark, Scope의 정보 묶음, 명시적 `uses`, 수명, 보호 중인
접근과 가공 결과를 함께 본다. RawInfo와 Info 모두 후보가 될 수 있다. Scope
전환이나 `uses`의 부재만으로 이동하지 않으며 Scope의 일부만 이동할 수 있다.
Major collection은 Cooling payload를 Cold에서 접근할 수 있게 준비하고 안전하게
완료한 뒤 이동을 확정한다. 의미를 생성하지 않는다.

### Cold 가공, Scope 요약과 Backing

Cold에 도착한 같은 Scope의 RawInfo는 공통 Refinement 계약으로 Info가 될 수
있다. Cold Scope 요약은 Cold watermark 초과를 기다리지 않고, Cold에 안정적으로
도착한 같은 Scope의 정보 묶음에 대해 생성한다. 요약은 작업의 연속성과 탐색을
위한 파생 정보이며 RawInfo나 Info를 대체하지 않는다. 한 Scope의 일부만 Cold에
있으면 요약도 그 일부만 대표한다.

사용자가 구현한 `ScopeSummarizer`는 선정된 정보에서 요약 내용, 참조,
실제 반영 범위와 사용자 데이터를 제안한다. Runtime은 근거와 반영 범위를
검증하고 `ColdCatalog`에 보존한다. 선정된 정보가 있는데 요약이 없거나
작업이 실패하면 실패를 드러내고 정보별 시도·실패 상태를 기록한다. 보류와
실패를 구별하며, 설정한 실패 한도에 도달하면 소진 상태를 기록한다.
가공 실패가 원본을 삭제하거나 잘못된 요약으로 대체하지는 않는다.

`ColdCatalog`는 Cold 정보의 Scope, 식별, 위치, 가공 상태와 Scope 요약 및 반영
범위를 조회하는 인덱스다. Payload의 보관 공간이나 Backing 구현이 아니다.
외부 Memory 계층은 Catalog의 근거 범위와 Backing에서 재호출한 정보를 이용할
수 있으나 Memory 가치와 identity는 스스로 결정한다.

`ColdCompactor`는 Cold watermark 압력에서 준비된 정보의 정확한 payload를
`ColdBacking`에 저장하고 재호출을 확인한 뒤 ColdZone에서 제거한다. 정상적으로
요약·가공된 정보뿐 아니라 실패 한도에 도달한 정보도 처리 상태를 붙여
내보낼 수 있다. 준비 중에는 Cold 접근을 유지한다. Backing 구현은 in-memory
map, file map 등으로 교체할 수 있다. Backing으로 나간 정보는 더 이상 Zone의
가공 대상이 아니다. Cold 가공과 요약, Backing 준비는 Agent 응답과 분리된
비동기 작업이다.

## 외부 경계와 불변식

`InfoRefiner`, `ScopeSummarizer`, token 계산기, Agent/provider adapter,
`ColdBacking`, Context wire format은 교체 가능한 외부 경계다. 호출자가 LLM
client, 모델, 프롬프트, 응답 해석을 소유한다. 코어는 제공자용 JSON Schema를
요구하지 않는다. 외부 구현은 결과를 제안하거나 payload를 보관할 수 있지만
Scope 소속과 lifecycle을 임의로 변경하지 않는다.

- RawInfo payload는 정확하게 다시 읽을 수 있다.
- 새 RawInfo를 선제적으로 가공하지 않는다.
- Info는 정확한 RawInfo 근거를 가지며 부분 가공은 원본 전체를 대체하지 않는다.
- Scope 소속은 참조나 Zone 이동으로 바뀌지 않는다.
- `uses` 부재를 미사용의 증거로 취급하지 않는다.
- Scope 전환이나 watermark만으로 Context를 강제로 Cooling으로 보내지 않는다.
- 비동기 준비가 끝나기 전에 기존 정보의 접근이나 lifecycle을 바꾸지 않는다.
- Cold와 Backing의 정보는 Catalog로 탐색하고 정확한 payload를 재호출한다.
- ContextCollector는 Memory 가치와 identity를 결정하지 않는다.
