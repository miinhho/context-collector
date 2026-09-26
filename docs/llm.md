# LLM 작업 연결

Objectization과 Cold Scope 요약은 Scheduler가 watermark와 수명을 검토해 예약한 뒤에만 호출한다. 새 Raw가 들어온 직후나 Agent의 `uses`·`scope` 보고를 처리할 때 추가 LLM 호출을 만들지 않는다.

Runtime은 `Objectizer`와 `ScopeSummarizer` 계약만 받는다. 각 작업의 모델, 지시문, 입력 구성, JSON schema, 응답 해석은 작업별 `ObjectizationCall`과 `ScopeSummaryCall`이 소유한다. 기본 JSON 구현에서는 `task`와 `instructions`를 설정할 수 있다. 요청 구성이나 응답 형식까지 바꾸려면 각 Call trait을 구현한다. 제공된 `LlmClient` 대신 다른 Client를 구현하거나, LLM 작업 자체를 다른 `Objectizer`·`ScopeSummarizer`로 대체할 수도 있다.

```rust
use std::{sync::Arc, time::Duration};
use context_collector::{InMemoryColdBacking, Runtime, RuntimeConfig};
use context_collector::compaction::llm::{
    JsonObjectizationCall, JsonScopeSummaryCall, LlmObjectizer, LlmScopeSummarizer,
};
use context_collector::llm::{LlmTaskConfig, OpenAiClientConfig, OpenAiResponsesClient};

# fn example(config: RuntimeConfig) -> Result<(), Box<dyn std::error::Error>> {
let client = Arc::new(OpenAiResponsesClient::new(OpenAiClientConfig {
    api_key: std::env::var("OPENAI_API_KEY")?,
    endpoint: "https://api.openai.com/v1/responses".into(),
    timeout: Duration::from_secs(60),
})?);
let mut extraction = JsonObjectizationCall::new(LlmTaskConfig {
    model: "objectization-model".into(),
    max_input_tokens: 8_000,
    max_output_tokens: 1_000,
})?;
extraction.instructions = "Extract grounded facts from the supplied Raw content. Cite source spans.".into();
let summary = JsonScopeSummaryCall::new(LlmTaskConfig {
    model: "summary-model".into(),
    max_input_tokens: 8_000,
    max_output_tokens: 1_000,
})?;
let _runtime = Runtime::new(
    config,
    Arc::new(LlmObjectizer::new(client.clone(), Arc::new(extraction))),
    Arc::new(LlmScopeSummarizer::new(client, Arc::new(summary))),
    Arc::new(InMemoryColdBacking::default()),
)?;
# Ok(())
# }
```

모델 이름은 자리표시자다. 사용자가 선택한 모델로 바꿔야 한다. API key는 호출자가 환경 변수나 비밀 저장소에서 읽어 전달한다.

LLM adapter는 제안만 생성한다. Runtime은 Objectization의 Raw 출처·revision·Scope·Zone과 요약의 Scope 근거 범위를 다시 확인한다. 외부 호출 중에는 Runtime 상태 잠금을 놓고, 실패하거나 결과가 오래됐으면 기존 Raw와 Cold payload를 유지한다. 입력 상한을 넘거나 응답이 잘못되면 작업은 오류로 기록한다.
