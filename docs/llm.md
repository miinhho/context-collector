# 사용자 작업 구현

Runtime은 LLM 제공자, 프롬프트, 모델, JSON Schema를 설정하지 않는다. 호출자는 `Objectizer<Data>`와 `ScopeSummarizer<Data, SummaryData>`를 구현한 객체를 `Runtime::new` 또는 `Runtime::with_counter`에 전달한다. 두 객체는 서로 다른 작업이며 각각 자체 client, 모델 선택, 프롬프트, 응답 해석, 재시도 상태를 필드로 소유할 수 있다. 같은 client를 공유하려면 호출자가 `Arc`를 복제해 두 구현체에 넣으면 된다.

Objectizer는 한 Scope·Zone에서 선정된 Raw와 출처 revision을 입력으로 받는다. Structured 제안은 `content`, Raw `sources`, 호출자가 정한 타입의 `data`를 포함한다. 빈 제안은 추출 보류를 나타낼 수 있다. Runtime은 원본 Raw를 유지하고, 제안된 출처와 현재 revision·Scope·Zone을 확인한 뒤 같은 Scope·Zone에 Structured를 생성한다. 호출 횟수와 호출 방식은 Objectizer 구현이 결정한다. 기본 Agent view에는 사용자 `data`를 싣지 않으며, `Runtime::read`로 객체 전체를 조회할 수 있다.

ScopeSummarizer는 ColdCompactor가 선정한 같은 Scope의 객체를 받는다. 제안은 `content`, 인용할 객체 `references`, 실제 요약에 반영한 객체 `covered`, 호출자가 정한 타입의 `data`를 포함한다. 요약하지 않을 때는 `None`을 반환한다. ColdCompactor는 참조와 반영 범위가 선정된 객체 안에 있는지 확인하고, Backing에서 객체를 정확히 다시 읽은 뒤에만 요약의 반영 범위와 위치를 ColdCatalog에 확정한다. Cold의 Raw Objectization은 별도로 같은 Objectizer 계약을 사용한다.

다음 형태로 호출자가 작업 구현체를 조립한다.

```rust,ignore
struct MyObjectizer {
    client: Arc<MyClient>,
    model_router: MyModelRouter,
    prompt: MyPromptBuilder,
    parser: MyObjectParser,
}

struct MyScopeSummarizer {
    client: Arc<MyClient>,
    model: String,
    prompt: MySummaryPrompt,
    parser: MySummaryParser,
}

let runtime = Runtime::<MyStructuredData, MySummaryData>::new(
    config,
    Arc::new(MyObjectizer { /* caller-owned fields */ }),
    Arc::new(MyScopeSummarizer { /* caller-owned fields */ }),
    backing,
)?;
```

컴파일되는 사용자 구현과 데이터 왕복 검사는 `tests/custom_work.rs`에 있다. 코어가 요구하는 것은 제안 타입과 근거 계약이며 제공자용 JSON Schema가 아니다. 사용자 구현이 특정 제공자의 구조화 출력 기능을 쓰려면 그 구현 내부에서 해당 제공자의 요청·응답 타입을 정의한다.

외부 작업 또는 Backing의 오류는 `ExternalError`로 반환한다. Runtime은 원본 오류를 source chain에 보존하고, 제안이나 Backing 확인이 실패한 작업을 canonical Context에 확정하지 않는다. 테스트의 구현은 실제 모델을 호출하지 않으므로 모델 출력의 의미 품질은 검증되지 않았다.
