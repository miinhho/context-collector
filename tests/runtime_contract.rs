use context_collector::error::ExternalError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use context_collector::cold::{CatalogLocation, ColdBacking};
use context_collector::compaction::refinement::{
    InfoDraft, InfoRefiner, RefinementInput, RefinementResult,
};
use context_collector::compaction::scope_summary::{
    ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal,
};
use context_collector::{
    ContextId, InMemoryColdBacking, InfoKind, NoopInfoRefiner, Runtime, RuntimeConfig, ScopeId,
    ScopeReport, SourceSpan, TokenCounter, TokenSpace, TurnObservation, Watermark, ZoneKind,
};

struct Bytes;
impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

struct FirstSpan;
impl InfoRefiner for FirstSpan {
    fn refine<'a>(
        &'a self,
        input: RefinementInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<RefinementResult, ExternalError>> + Send + 'a>> {
        Box::pin(async move {
            let Some(first) = input.raw.iter().find(|input| !input.content.is_empty()) else {
                return Ok(Vec::new().into());
            };
            Ok(vec![InfoDraft {
                content: "fact".into(),
                data: (),
                sources: vec![SourceSpan {
                    raw: first.id,
                    revision: first.revision,
                    start: 0,
                    end: first.content.chars().next().unwrap().len_utf8(),
                }],
            }]
            .into())
        })
    }
}

struct FixedSummary;
impl ScopeSummarizer for FixedSummary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, ExternalError>> + Send + 'a>,
    > {
        Box::pin(async move {
            Ok(Some(ScopeSummaryProposal {
                content: "opaque scope summary".into(),
                references: inputs.iter().map(|item| item.info.id).collect(),
                covered: inputs.iter().map(|item| item.info.id).collect(),
                data: (),
            }))
        })
    }
}

fn config() -> RuntimeConfig {
    RuntimeConfig {
        watermarks: [Watermark { low: 1, high: 4 }; 5],
        hot_high: 100,
        processing_batch_tokens: 1024,
        max_processing_failures: 3,
    }
}

fn runtime(
    refiner: Arc<dyn InfoRefiner>,
    summarizer: Arc<dyn ScopeSummarizer>,
    backing: Arc<dyn ColdBacking>,
) -> Runtime {
    Runtime::with_counter(config(), Arc::new(Bytes), refiner, summarizer, backing).unwrap()
}

async fn settle(runtime: &Runtime) {
    runtime.drain_maintenance().await;
    assert!(runtime.maintenance_errors().await.is_empty());
}

#[tokio::test]
async fn turn_reports_assign_scope_without_changing_prior_ownership() {
    let runtime = runtime(
        Arc::new(NoopInfoRefiner),
        Arc::new(FixedSummary),
        Arc::new(InMemoryColdBacking::default()),
    );
    let first = runtime
        .complete_turn("user".into(), "agent".into(), TurnObservation::default())
        .await
        .unwrap();
    settle(&runtime).await;
    let second = runtime
        .complete_turn(
            "new".into(),
            "work".into(),
            TurnObservation {
                uses: Some(vec![first.user]),
                scope: Some(ScopeReport::Transition),
            },
        )
        .await
        .unwrap();
    settle(&runtime).await;
    assert_ne!(first.scope, second.scope);
    assert_eq!(runtime.current_scope().await, second.scope);
    assert_eq!(
        runtime.read(first.user).await.unwrap().unwrap().kind,
        InfoKind::Raw("user".into())
    );
    assert_eq!(
        runtime.read(second.user).await.unwrap().unwrap().kind,
        InfoKind::Raw("new".into())
    );
    runtime.select_scope(first.scope).await.unwrap();
    assert_eq!(runtime.current_scope().await, first.scope);
    let third = runtime
        .complete_turn(
            "more".into(),
            "answer".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Uncertain),
            },
        )
        .await
        .unwrap();
    assert_eq!(third.scope, first.scope);
}

#[tokio::test]
async fn info_refinement_keeps_raw_exact_and_tracks_both_token_kinds() {
    let runtime = runtime(
        Arc::new(FirstSpan),
        Arc::new(FixedSummary),
        Arc::new(InMemoryColdBacking::default()),
    );
    let first = runtime
        .complete_turn("αbc".into(), "long".into(), TurnObservation::default())
        .await
        .unwrap();
    settle(&runtime).await;
    runtime
        .complete_turn("more".into(), "text".into(), TurnObservation::default())
        .await
        .unwrap();
    settle(&runtime).await;
    let original = runtime.read(first.user).await.unwrap().unwrap();
    assert_eq!(original.kind, InfoKind::Raw("αbc".into()));
    let view = runtime.context_view(TokenSpace(300), &[]).await.unwrap();
    let extracted: Vec<_> = view
        .notes
        .iter()
        .filter(|note| note.sources.contains(&first.user))
        .collect();
    assert!(!extracted.is_empty());
    assert!(extracted.iter().all(|note| note.scope == first.scope));
    for note in extracted {
        let zone = runtime.zone_of(note.id.unwrap()).await.unwrap();
        let usage = runtime.zone_usage(zone).await;
        assert!(usage.raw > 0 && usage.info > 0);
    }
}

#[tokio::test]
async fn cold_catalog_and_backing_preserve_exact_raw_and_scoped_summary() {
    let backing = Arc::new(InMemoryColdBacking::default());
    let runtime = runtime(
        Arc::new(NoopInfoRefiner),
        Arc::new(FixedSummary),
        backing.clone(),
    );
    let first = runtime
        .complete_turn(
            "source payload".into(),
            "agent".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    settle(&runtime).await;
    runtime
        .complete_turn("next".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    settle(&runtime).await;
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
    settle(&runtime).await;
    runtime
        .complete_turn(
            "continued".into(),
            "topic".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    settle(&runtime).await;
    let entries = runtime.cold_scope_entries(first.scope).await;
    assert!(
        entries
            .iter()
            .any(|entry| entry.id == first.user && entry.location == CatalogLocation::Backing),
        "entries: {entries:?}"
    );
    assert_eq!(runtime.zone_of(first.user).await, None);
    assert_eq!(
        runtime.read(first.user).await.unwrap().unwrap().kind,
        InfoKind::Raw("source payload".into())
    );
    assert_eq!(
        backing.load(first.user).unwrap().unwrap().kind,
        InfoKind::Raw("source payload".into())
    );
    runtime.select_scope(first.scope).await.unwrap();
    let view = runtime
        .context_view(TokenSpace(500), &[first.user])
        .await
        .unwrap();
    assert!(view.notes.iter().any(|note| note.id == Some(first.user)));
    assert!(
        view.notes
            .iter()
            .any(|note| note.content == "opaque scope summary"
                && note.coverage.contains(&first.user))
    );
    assert!(view.used_tokens <= 500);
}

#[tokio::test]
async fn unknown_uses_are_rejected_before_turn_is_recorded() {
    let runtime = runtime(
        Arc::new(NoopInfoRefiner),
        Arc::new(FixedSummary),
        Arc::new(InMemoryColdBacking::default()),
    );
    let error = runtime
        .complete_turn(
            "u".into(),
            "a".into(),
            TurnObservation {
                uses: Some(vec![ContextId(999)]),
                scope: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        context_collector::RuntimeError::UnknownContext(ContextId(999))
    ));
    assert_eq!(runtime.turn().await, 0);
}

#[tokio::test]
async fn synthetic_scope_report_sequences_preserve_turn_assignment_and_raw() {
    let reports = [
        None,
        Some(ScopeReport::Continue),
        Some(ScopeReport::Transition),
        Some(ScopeReport::Uncertain),
    ];
    for first_report in reports {
        for second_report in reports {
            for third_report in reports {
                let runtime = Runtime::with_counter(
                    RuntimeConfig {
                        watermarks: [Watermark { low: 1, high: 1000 }; 5],
                        hot_high: 10_000,
                        processing_batch_tokens: 1024,
                        max_processing_failures: 3,
                    },
                    Arc::new(Bytes),
                    Arc::new(NoopInfoRefiner),
                    Arc::new(FixedSummary),
                    Arc::new(InMemoryColdBacking::default()),
                )
                .unwrap();
                let mut previous_scope = runtime.current_scope().await;
                let mut receipts = Vec::new();
                for (index, report) in [first_report, second_report, third_report]
                    .into_iter()
                    .enumerate()
                {
                    let user = format!("user-{index}");
                    let agent = format!("agent-{index}");
                    let receipt = runtime
                        .complete_turn(
                            user.clone(),
                            agent.clone(),
                            TurnObservation {
                                uses: receipts.first().map(
                                    |first: &context_collector::runtime::TurnReceipt| {
                                        vec![first.user]
                                    },
                                ),
                                scope: report,
                            },
                        )
                        .await
                        .unwrap();
                    if report == Some(ScopeReport::Transition) {
                        assert_ne!(receipt.scope, previous_scope);
                    } else {
                        assert_eq!(receipt.scope, previous_scope);
                    }
                    assert_eq!(
                        runtime.read(receipt.user).await.unwrap().unwrap().kind,
                        InfoKind::Raw(user.into())
                    );
                    assert_eq!(
                        runtime.read(receipt.agent).await.unwrap().unwrap().kind,
                        InfoKind::Raw(agent.into())
                    );
                    previous_scope = receipt.scope;
                    receipts.push(receipt);
                }
                assert_eq!(runtime.turn().await, 3);
                assert_eq!(runtime.current_scope().await, previous_scope);
                runtime.select_scope(receipts[0].scope).await.unwrap();
                assert_eq!(runtime.current_scope().await, receipts[0].scope);
                assert_eq!(
                    runtime.zone_usage(ZoneKind::Eden).await.raw,
                    3 * ("user-0".len() + "agent-0".len())
                );
            }
        }
    }
}

struct FirstOnlySummary {
    input_sizes: std::sync::Mutex<Vec<usize>>,
}

impl ScopeSummarizer for FirstOnlySummary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, ExternalError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.input_sizes.lock().unwrap().push(inputs.len());
            let id = inputs[0].info.id;
            Ok(Some(ScopeSummaryProposal {
                content: "first info only".into(),
                references: vec![id],
                covered: vec![id],
                data: (),
            }))
        })
    }
}

#[tokio::test]
async fn cold_summary_coverage_tracks_declared_subset_of_selected_infos() {
    let summarizer = Arc::new(FirstOnlySummary {
        input_sizes: std::sync::Mutex::new(Vec::new()),
    });
    let runtime = runtime(
        Arc::new(NoopInfoRefiner),
        summarizer.clone(),
        Arc::new(InMemoryColdBacking::default()),
    );
    let first = runtime
        .complete_turn(
            "source payload".into(),
            "agent".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    settle(&runtime).await;
    runtime
        .complete_turn("next".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    settle(&runtime).await;
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
    settle(&runtime).await;
    runtime
        .complete_turn(
            "continued".into(),
            "topic".into(),
            TurnObservation::default(),
        )
        .await
        .unwrap();
    settle(&runtime).await;
    assert!(
        summarizer
            .input_sizes
            .lock()
            .unwrap()
            .iter()
            .any(|size| *size > 1)
    );
    let summaries = runtime.cold_scope_summaries(first.scope).await;
    assert!(!summaries.is_empty());
    assert!(summaries.iter().all(
        |summary| summary.coverage.len() == 1 && summary.coverage[0].0 == summary.references[0]
    ));
}
