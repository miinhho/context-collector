use std::sync::Arc;

use context_collector::compaction::scope_summary::{
    ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal,
};
use context_collector::error::TaskFuture;
use context_collector::view::TokenSpace;
use context_collector::{
    InMemoryColdBacking, MessageRole, NoopInfoRefiner, Runtime, RuntimeConfig, ScopeId,
    ScopeReport, TokenCounter, TurnObservation, Watermark,
};

struct Bytes;
impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

struct Summary;
impl ScopeSummarizer for Summary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> TaskFuture<'a, Option<ScopeSummaryProposal>> {
        Box::pin(async move {
            Ok(Some(ScopeSummaryProposal {
                content: "이전 조사에서 갱신 순서를 확인했다".into(),
                references: inputs.iter().map(|input| input.info.id).collect(),
                covered: inputs.iter().map(|input| input.info.id).collect(),
                data: (),
            }))
        })
    }
}

fn runtime() -> Runtime {
    Runtime::with_counter(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 4 }; 5],
            hot_high: 100,
            processing_batch_tokens: 1024,
            max_processing_failures: 2,
        },
        Arc::new(Bytes),
        Arc::new(NoopInfoRefiner),
        Arc::new(Summary),
        Arc::new(InMemoryColdBacking::default()),
    )
    .unwrap()
}

#[tokio::test]
async fn view_keeps_turn_messages_in_role_order_and_counts_rendered_markdown() {
    let runtime = runtime();
    let first = runtime
        .complete_turn(
            "첫 질문".into(),
            "첫 답변".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    let second = runtime
        .complete_turn(
            "둘째 질문".into(),
            "둘째 답변".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    let view = runtime.context_view(TokenSpace(1000), &[]).await.unwrap();
    let actual: Vec<_> = view
        .messages
        .iter()
        .map(|message| (message.id, message.turn, message.role))
        .collect();
    assert_eq!(
        actual,
        vec![
            (first.user, first.turn, MessageRole::User),
            (first.agent, first.turn, MessageRole::Agent),
            (second.user, second.turn, MessageRole::User),
            (second.agent, second.turn, MessageRole::Agent),
        ]
    );
    let markdown = view.markdown();
    assert!(markdown.contains(&format!("사용자 (#{}): 첫 질문", first.user.0)));
    assert!(markdown.contains(&format!("Agent (#{}): 둘째 답변", second.agent.0)));
    assert!(!markdown.contains("Scope"));
    assert!(!markdown.contains("Cold"));
    assert!(!markdown.contains("RawInfo"));
    assert_eq!(view.used_tokens, markdown.len());
}

#[tokio::test]
async fn backed_message_use_is_indexed_and_retrievable_without_moving_it() {
    let runtime = runtime();
    let first = runtime
        .complete_turn(
            "원본 기록".into(),
            "확인함".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("다음".into(), "응답".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn(
            "다른 작업".into(),
            "진행".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Transition),
            },
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("계속".into(), "진행".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    assert!(runtime.zone_of(first.user).await.is_none());
    let receipt = runtime
        .complete_turn(
            "예전 기록 확인".into(),
            "확인 결과".into(),
            TurnObservation {
                uses: Some(vec![first.user]),
                scope: None,
            },
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    assert!(runtime.zone_of(first.user).await.is_none());
    assert!(
        runtime
            .cold_scope_entries(first.scope)
            .await
            .iter()
            .any(|entry| { entry.id == first.user && entry.last_used_turn == Some(receipt.turn) })
    );
    let recent = runtime
        .recent_context(Some(first.scope), 3, TokenSpace(1000))
        .await
        .unwrap();
    assert!(recent.contains(&format!("#{} (사용자): 원본 기록", first.user.0)));
    let opened = runtime
        .open_context_markdown(first.user)
        .await
        .unwrap()
        .unwrap();
    assert!(opened.contains("원본 기록"));
    let scope_index = runtime
        .scope_context_markdown(first.scope, TokenSpace(1000))
        .await
        .unwrap();
    assert!(scope_index.contains("이전 조사에서 갱신 순서를 확인했다"));
    assert!(scope_index.contains(&format!("#{}", first.user.0)));
    let view = runtime.context_view(TokenSpace(1000), &[]).await.unwrap();
    assert!(
        view.notes
            .iter()
            .any(|note| note.content == "이전 조사에서 갱신 순서를 확인했다")
    );
    assert!(!view.messages.iter().any(|message| message.id == first.user));
}

#[tokio::test]
async fn explicit_backed_content_reports_when_it_cannot_fit() {
    let runtime = runtime();
    let first = runtime
        .complete_turn(
            "아주 긴 원본 기록".into(),
            "응답".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("다음".into(), "응답".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn(
            "다른 작업".into(),
            "진행".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Transition),
            },
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("계속".into(), "진행".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    let view = runtime
        .context_view(TokenSpace(1), &[first.user])
        .await
        .unwrap();
    assert_eq!(view.unfulfilled, vec![first.user]);
    assert!(view.used_tokens <= 1);
}
