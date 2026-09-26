use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use context_collector::compaction::llm::{
    JsonObjectizationCall, JsonScopeSummaryCall, LlmObjectizer, LlmScopeSummarizer,
    ObjectizationCall,
};
use context_collector::compaction::objectization::{Objectizer, RawInput};
use context_collector::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput};
use context_collector::llm::{
    LlmClient, LlmRequest, LlmTaskConfig, OpenAiClientConfig, OpenAiResponsesClient,
};
use context_collector::{
    ContextId, ContextObject, InMemoryColdBacking, Runtime, RuntimeConfig, ScopeId,
    TurnObservation, Watermark,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct FakeClient {
    requests: Mutex<Vec<LlmRequest>>,
    outputs: Mutex<VecDeque<String>>,
}

impl FakeClient {
    fn new(outputs: &[&str]) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            outputs: Mutex::new(outputs.iter().map(|s| (*s).to_owned()).collect()),
        }
    }
}

impl LlmClient for FakeClient {
    fn complete<'a>(
        &'a self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("no fake response".into())
        })
    }
}

fn task() -> LlmTaskConfig {
    LlmTaskConfig {
        model: "test-model".into(),
        max_input_tokens: 4096,
        max_output_tokens: 256,
    }
}

#[tokio::test]
async fn llm_adapters_pass_task_settings_and_parse_grounded_proposals() {
    let client = Arc::new(FakeClient::new(&[
        r#"{"objects":[{"content":"fact","sources":[{"raw":7,"revision":1,"start":0,"end":4}]}]}"#,
        r#"{"content":"scope summary","references":[7]}"#,
    ]));
    let objectizer = LlmObjectizer::new(
        client.clone(),
        Arc::new(JsonObjectizationCall::new(task()).unwrap()),
    );
    let proposals = objectizer
        .extract(&[RawInput {
            id: ContextId(7),
            revision: 1,
            content: "fact line".into(),
        }])
        .await
        .unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].sources[0].raw, ContextId(7));
    let summarizer = LlmScopeSummarizer::new(
        client.clone(),
        Arc::new(JsonScopeSummaryCall::new(task()).unwrap()),
    );
    let summary = summarizer
        .summarize(
            ScopeId(3),
            &[ScopeSummaryInput {
                object: ContextObject::raw(ContextId(7), "fact line".into()),
            }],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.references, vec![ContextId(7)]);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].task.model, "test-model");
    assert_eq!(requests[0].schema_name, "context_objects");
    assert_eq!(requests[1].schema_name, "scope_summary");
    assert!(requests[0].input.contains("fact line"));
    assert!(requests[1].input.contains("\"scope\":3"));
}

#[tokio::test]
async fn malformed_llm_output_is_rejected_without_fabricating_objects() {
    let client = Arc::new(FakeClient::new(&["not json"]));
    let objectizer = LlmObjectizer::new(
        client,
        Arc::new(JsonObjectizationCall::new(task()).unwrap()),
    );
    assert!(
        objectizer
            .extract(&[RawInput {
                id: ContextId(1),
                revision: 1,
                content: "raw".into()
            }])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn openai_client_sends_configured_responses_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let header = String::from_utf8_lossy(&request[..header_end]);
        assert!(header.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(
            header
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        let length: usize = header
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap();
        while request.len() - header_end < length {
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
        }
        let body: Value =
            serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["store"], false);
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["strict"], true);
        let response = json!({"status":"completed","output":[{"content":[{"type":"output_text","text":"{\"objects\":[]}"}]}]}).to_string();
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });
    let client = OpenAiResponsesClient::new(OpenAiClientConfig {
        api_key: "test-key".into(),
        endpoint: format!("http://{address}/v1/responses"),
        timeout: Duration::from_secs(3),
    })
    .unwrap();
    let text = client
        .complete(LlmRequest {
            task: task(),
            instructions: "instruction".into(),
            input: "input".into(),
            schema_name: "context_objects".into(),
            schema: json!({"type":"object"}),
        })
        .await
        .unwrap();
    assert_eq!(text, r#"{"objects":[]}"#);
    server.await.unwrap();
}

#[test]
fn openai_client_rejects_insecure_remote_endpoint() {
    let result = OpenAiResponsesClient::new(OpenAiClientConfig {
        api_key: "test-key".into(),
        endpoint: "http://example.com/v1/responses".into(),
        timeout: Duration::from_secs(1),
    });
    assert!(result.is_err());
}

#[tokio::test]
async fn openai_client_rejects_requests_over_configured_input_limit_before_network() {
    let client = OpenAiResponsesClient::new(OpenAiClientConfig {
        api_key: "test-key".into(),
        endpoint: "https://api.openai.com/v1/responses".into(),
        timeout: Duration::from_secs(1),
    })
    .unwrap();
    let mut limited = task();
    limited.max_input_tokens = 1;
    let result = client
        .complete(LlmRequest {
            task: limited,
            instructions: "instruction".into(),
            input: "multiple input tokens".into(),
            schema_name: "context_objects".into(),
            schema: json!({"type":"object"}),
        })
        .await;
    assert_eq!(
        result.unwrap_err(),
        "LLM request exceeds configured input token limit"
    );
}

#[tokio::test]
async fn runtime_calls_llm_only_after_aged_raw_reaches_compaction_pressure() {
    let client = Arc::new(FakeClient::new(&[
        r#"{"objects":[]}"#,
        r#"{"objects":[]}"#,
        r#"{"objects":[]}"#,
    ]));
    let runtime = Runtime::new(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 2 }; 5],
            hot_high: 3,
        },
        Arc::new(LlmObjectizer::new(
            client.clone(),
            Arc::new(JsonObjectizationCall::new(task()).unwrap()),
        )),
        Arc::new(LlmScopeSummarizer::new(
            client.clone(),
            Arc::new(JsonScopeSummaryCall::new(task()).unwrap()),
        )),
        Arc::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    runtime
        .complete_turn("first".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    assert!(client.requests.lock().unwrap().is_empty());
    runtime
        .complete_turn("second".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    let completed = runtime.drain_maintenance().await;
    assert!(
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.schema_name == "context_objects"),
        "completed={completed:?}, pending={}, eden={:?}, survivor={:?}, mature={:?}, errors={:?}",
        runtime.pending_jobs().await,
        runtime.zone_usage(context_collector::ZoneKind::Eden).await,
        runtime
            .zone_usage(context_collector::ZoneKind::Survivor)
            .await,
        runtime
            .zone_usage(context_collector::ZoneKind::Mature)
            .await,
        runtime.maintenance_errors().await
    );
    assert!(runtime.maintenance_errors().await.is_empty());
}

struct CustomObjectizationCall;

impl ObjectizationCall for CustomObjectizationCall {
    fn request(&self, inputs: &[RawInput]) -> Result<LlmRequest, String> {
        Ok(LlmRequest {
            task: LlmTaskConfig {
                model: format!("model-for-{}", inputs[0].id.0),
                max_input_tokens: 4096,
                max_output_tokens: 128,
            },
            instructions: "custom extraction instructions".into(),
            input: inputs[0].content.clone(),
            schema_name: "custom_result".into(),
            schema: serde_json::from_str(r#"{"type":"object"}"#).unwrap(),
        })
    }

    fn parse(&self, response: &str) -> Result<Vec<context_collector::StructuredProposal>, String> {
        #[derive(serde::Deserialize)]
        struct CustomResult {
            fact: String,
        }
        let parsed: CustomResult =
            serde_json::from_str(response).map_err(|error| error.to_string())?;
        Ok(vec![context_collector::StructuredProposal {
            content: parsed.fact,
            sources: vec![context_collector::SourceSpan {
                raw: ContextId(7),
                revision: 1,
                start: 0,
                end: 4,
            }],
        }])
    }
}

#[tokio::test]
async fn custom_call_controls_per_request_model_prompt_and_parser() {
    let client = Arc::new(FakeClient::new(&[r#"{"fact":"custom fact"}"#]));
    let objectizer = LlmObjectizer::new(client.clone(), Arc::new(CustomObjectizationCall));
    let proposals = objectizer
        .extract(&[RawInput {
            id: ContextId(7),
            revision: 1,
            content: "fact line".into(),
        }])
        .await
        .unwrap();
    assert_eq!(proposals[0].content, "custom fact");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].task.model, "model-for-7");
    assert_eq!(requests[0].instructions, "custom extraction instructions");
    assert_eq!(requests[0].schema_name, "custom_result");
}
