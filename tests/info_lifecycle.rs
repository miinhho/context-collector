use std::sync::{Arc, Mutex};

use context_collector::compaction::refinement::{
    InfoDraft, InfoRefiner, RefinementInput, RefinementResult,
};
use context_collector::compaction::scope_summary::{
    ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal,
};
use context_collector::error::TaskFuture;
use context_collector::{
    ContextId, InMemoryColdBacking, NoopInfoRefiner, ProcessingFailure, RawInfo, Runtime,
    RuntimeConfig, ScopeId, ScopeReport, SourceSpan, TokenCounter, TurnObservation, Watermark,
    ZoneKind,
};

struct Bytes;
impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

fn config(cold_high: usize) -> RuntimeConfig {
    let mut watermarks = [Watermark { low: 1, high: 4 }; 5];
    watermarks[4] = Watermark {
        low: 1,
        high: cold_high,
    };
    RuntimeConfig {
        watermarks,
        hot_high: 100,
        processing_batch_tokens: 1024,
        max_processing_failures: 2,
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
                content: "Cold scope navigation".into(),
                references: inputs.iter().map(|input| input.info.id).collect(),
                covered: inputs.iter().map(|input| input.info.id).collect(),
                data: (),
            }))
        })
    }
}

struct NoSummary;
impl ScopeSummarizer for NoSummary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        _inputs: &'a [ScopeSummaryInput],
    ) -> TaskFuture<'a, Option<ScopeSummaryProposal>> {
        Box::pin(async { Ok(None) })
    }
}

async fn drive_to_cold(runtime: &Runtime) -> (ContextId, ScopeId) {
    let first = runtime
        .complete_turn(
            "source payload".into(),
            "agent".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("next".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn(
            "other".into(),
            "topic".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Transition),
            },
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn(
            "continued".into(),
            "topic".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    (first.user, first.scope)
}

#[tokio::test]
async fn cold_scope_is_summarized_before_cold_watermark_requires_backing() {
    let runtime = Runtime::with_counter(
        config(1000),
        Arc::new(Bytes),
        Arc::new(NoopInfoRefiner),
        Arc::new(Summary),
        Arc::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let (id, scope) = drive_to_cold(&runtime).await;
    assert_eq!(runtime.zone_of(id).await, Some(ZoneKind::Cold));
    let summaries = runtime.cold_scope_summaries(scope).await;
    assert!(summaries.iter().any(|summary| {
        summary.content == "Cold scope navigation"
            && summary.coverage.iter().any(|(covered, _)| *covered == id)
    }));
    assert!(runtime.read(id).await.unwrap().is_some());
}

#[tokio::test]
async fn repeated_no_summary_is_visible_on_the_exact_backed_info() {
    let runtime = Runtime::with_counter(
        config(4),
        Arc::new(Bytes),
        Arc::new(NoopInfoRefiner),
        Arc::new(NoSummary),
        Arc::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let (id, scope) = drive_to_cold(&runtime).await;
    assert_eq!(runtime.zone_of(id).await, None);
    let item = runtime.read(id).await.unwrap().unwrap();
    assert_eq!(
        item.kind,
        context_collector::InfoKind::Raw(RawInfo::from("source payload"))
    );
    let attempt = &item.processing.cold_summary;
    assert_eq!(attempt.failures, 2);
    assert!(attempt.exhausted);
    assert_eq!(
        attempt.last_failure,
        Some(ProcessingFailure::NoSummaryReturned)
    );
    assert!(runtime.cold_scope_summaries(scope).await.is_empty());
    assert!(
        runtime
            .cold_scope_entries(scope)
            .await
            .iter()
            .any(|entry| { entry.id == id && entry.processing.cold_summary.exhausted })
    );
}

struct SettlingRefiner {
    seen: Mutex<Vec<ContextId>>,
}
impl InfoRefiner for SettlingRefiner {
    fn refine<'a>(&'a self, input: RefinementInput<'a>) -> TaskFuture<'a, RefinementResult> {
        Box::pin(async move {
            let first = &input.raw[0];
            self.seen.lock().unwrap().push(first.id);
            Ok(RefinementResult {
                infos: vec![InfoDraft {
                    content: "refined information".into(),
                    sources: vec![SourceSpan {
                        raw: first.id,
                        revision: first.revision,
                        start: 0,
                        end: first.content.len(),
                    }],
                    data: (),
                }],
                settled: vec![first.id],
            })
        })
    }
}

#[tokio::test]
async fn settled_raw_is_not_refined_again_in_the_same_hot_lifecycle() {
    let refiner = Arc::new(SettlingRefiner {
        seen: Mutex::new(Vec::new()),
    });
    let runtime = Runtime::with_counter(
        config(1000),
        Arc::new(Bytes),
        refiner.clone(),
        Arc::new(Summary),
        Arc::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let first = runtime
        .complete_turn("source".into(), "agent".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("more".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("again".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    assert_eq!(
        refiner
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|id| **id == first.user)
            .count(),
        1
    );
    assert!(
        runtime
            .read(first.user)
            .await
            .unwrap()
            .unwrap()
            .processing
            .hot_refinement
            .completed
    );
    let full_view = runtime
        .context_view(context_collector::TokenSpace(1000), &[])
        .await
        .unwrap();
    assert!(
        full_view
            .notes
            .iter()
            .any(|note| note.content == "refined information")
    );
    assert!(
        full_view
            .messages
            .iter()
            .any(|message| message.id == first.user)
    );
}
