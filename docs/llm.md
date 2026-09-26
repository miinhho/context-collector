# 사용자 정보 가공 구현

Runtime은 LLM 제공자, 모델, 프롬프트, 응답 형식이나 JSON Schema를 설정하지
않는다. 호출자는 `InfoRefiner<Data>`와
`ScopeSummarizer<Data, SummaryData>`를 구현해 `Runtime::new` 또는
`Runtime::with_counter`에 주입한다. 각각 별도의 작업이며 client, 모델 선택,
프롬프트, 응답 해석과 작업 내부 상태를 구현체가 소유한다. Client 공유 여부도
호출자가 결정한다. LLM을 쓰지 않는 구현도 같은 계약을 따른다.

## RawInfo에서 Info로

`InfoRefiner::refine`은 같은 Scope·Zone에서 선정된 `RawInfoInput`들의 ID,
revision, 내용, 생성 turn과 마지막 사용 turn을 받는다. 결과의 `infos`에는
각 Info의 표시 내용, 정확한 RawInfo 근거 구간과 사용자 정의 타입 `data`를
제안한다. `settled`에는 이번 Zone에서 더 가공할 필요가 없는 RawInfo ID를
명시한다. 빈 `infos`만으로 작업 완료를 뜻하지 않으며, `settled`에 없는
RawInfo는 보류되어 다음 관측 이후 다시 후보가 될 수 있다.

Runtime은 원본 RawInfo를 유지한다. 제안의 근거 구간, revision, Scope, Zone과
보호 상태를 검증한 뒤 같은 Scope·Zone에 Info를 만든다. 제안이 실패하거나
근거가 바뀌면 canonical 정보에 확정하지 않고 정보별 가공 실패를 기록한다.
Info의 `data`는 Runtime이 해석하지 않으며 Zone 이동과 Backing에서 보존한다.
기본 Agent View에서는 제외하고 `Runtime::read`에서 확인할 수 있다.

## Cold Scope 요약

`ScopeSummarizer::summarize`는 Cold에 도착한 같은 Scope의 정보 후보를 받는다.
Cold watermark와 관계없이 실행할 수 있다. `ScopeSummaryProposal`은 내용,
인용할 `references`, 실제 반영한 `covered`와 사용자 정의 `data`를 담는다.
Runtime은 두 집합이 선정된 후보에 속하는지 확인하고 반영 범위를
`ColdCatalog`에 기록한다. 일부만 반영했다면 그 부분만 완료로 표시한다.

선정된 후보가 있는데 `None`을 반환하면 요약 부재 오류로 드러난다.
외부 오류, 유효하지 않은 제안과 요약 부재는 각 정보의 시도·실패 상태에
기록된다. 설정한 실패 한도에 도달하면 소진 상태가 된다. Cold watermark에
도달한 뒤에는 처리 완료 또는 소진 상태를 보존한 정확한 정보를 Backing으로
내보낼 수 있다. Backing은 요약을 수행하지 않는다.

다음처럼 호출자가 두 작업을 조립한다.

```rust,ignore
struct MyInfoRefiner {
    client: Arc<MyClient>,
    model_router: MyModelRouter,
    prompt: MyRefinementPrompt,
    parser: MyInfoParser,
}

struct MyScopeSummarizer {
    client: Arc<MyClient>,
    model: String,
    prompt: MySummaryPrompt,
    parser: MySummaryParser,
}

let runtime = Runtime::<MyInfoData, MySummaryData>::new(
    config,
    Arc::new(MyInfoRefiner { /* caller-owned fields */ }),
    Arc::new(MyScopeSummarizer { /* caller-owned fields */ }),
    backing,
)?;
```

컴파일되는 사용자 구현과 데이터 왕복 검사는 `tests/custom_work.rs`에 있다.
제공자의 구조화 출력 기능을 쓰는 구현은 자체 요청·응답 타입을 정의한다.
외부 작업과 Backing 오류는 `ExternalError`로 반환한다. 테스트의 구현은 실제
모델을 호출하지 않으므로 모델 출력의 의미 품질은 검증되지 않았다.
