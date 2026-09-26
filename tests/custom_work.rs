use context_collector::error::ExternalError;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use context_collector::compaction::refinement::{
    InfoDraft, InfoRefiner, RefinementInput, RefinementResult,
};
use context_collector::compaction::scope_summary::{
    ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal,
};
use context_collector::{
    ColdBacking, InMemoryColdBacking, InfoKind, Runtime, RuntimeConfig, ScopeId, ScopeReport,
    SourceSpan, TokenCounter, TurnObservation, Watermark,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct FactData {
    category: String,
    model_used: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SummaryData {
    navigation_tag: String,
}

struct Bytes;
impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

// All call settings and state belong to the application's work implementation.
struct UserInfoRefiner {
    model: String,
    prompt: String,
    observed: Mutex<Vec<(ScopeId, context_collector::ZoneKind)>>,
}

impl InfoRefiner<FactData> for UserInfoRefiner {
    fn refine<'a>(
        &'a self,
        input: RefinementInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<RefinementResult<FactData>, ExternalError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.observed
                .lock()
                .unwrap()
                .push((input.scope, input.zone));
            let Some(raw) = input.raw.iter().find(|item| !item.content.is_empty()) else {
                return Ok(Vec::new().into());
            };
            Ok(vec![InfoDraft {
                content: format!("{}: {}", self.prompt, raw.content),
                sources: vec![SourceSpan {
                    raw: raw.id,
                    revision: raw.revision,
                    start: 0,
                    end: raw.content.chars().next().unwrap().len_utf8(),
                }],
                data: FactData {
                    category: "user-defined".into(),
                    model_used: self.model.clone(),
                },
            }]
            .into())
        })
    }
}

struct UserSummarizer {
    tag: String,
}
impl ScopeSummarizer<FactData, SummaryData> for UserSummarizer {
    fn summarize<'a>(
        &'a self,
        scope: ScopeId,
        inputs: &'a [ScopeSummaryInput<FactData>],
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<ScopeSummaryProposal<SummaryData>>, ExternalError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(Some(ScopeSummaryProposal {
                content: format!("scope {} summary", scope.0),
                references: inputs.iter().map(|input| input.info.id).collect(),
                covered: inputs.iter().map(|input| input.info.id).collect(),
                data: SummaryData {
                    navigation_tag: self.tag.clone(),
                },
            }))
        })
    }
}

#[tokio::test]
async fn user_data_survives_refinement_cold_backing_and_catalog_retrieval() {
    let refiner = Arc::new(UserInfoRefiner {
        model: "chosen-by-user".into(),
        prompt: "extract".into(),
        observed: Mutex::new(Vec::new()),
    });
    let backing = Arc::new(InMemoryColdBacking::<FactData>::default());
    let runtime = Runtime::<FactData, SummaryData>::with_counter(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 4 }; 5],
            hot_high: 100,
            processing_batch_tokens: 1024,
            max_processing_failures: 3,
        },
        Arc::new(Bytes),
        refiner.clone(),
        Arc::new(UserSummarizer {
            tag: "memory-navigation".into(),
        }),
        backing.clone(),
    )
    .unwrap();
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
    assert!(!refiner.observed.lock().unwrap().is_empty());
    let info_id = runtime
        .context_view(&[])
        .await
        .unwrap()
        .notes
        .into_iter()
        .find(|note| note.sources.contains(&first.user))
        .and_then(|note| note.id)
        .expect("user typed proposal was accepted");
    let evidence = runtime.evidence_markdown(info_id).await.unwrap().unwrap();
    assert!(evidence.contains(&format!("근거 #{}", first.user.0)));
    assert!(evidence.contains("> s"));
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
    let info_item = runtime.read(info_id).await.unwrap().unwrap();
    let InfoKind::Info(info) = info_item.kind else {
        panic!("expected Info");
    };
    let data = info.data;
    assert_eq!(data.category, "user-defined");
    assert_eq!(data.model_used, "chosen-by-user");
    assert_eq!(runtime.zone_of(info_id).await, None);
    let stored = backing.load(info_id).unwrap().unwrap();
    assert_eq!(stored.id, info_id);
    assert!(matches!(stored.kind,
        InfoKind::Info(context_collector::Info { data: FactData { model_used, .. }, .. })
            if model_used == "chosen-by-user"));
    let summaries = runtime.cold_scope_summaries(first.scope).await;
    assert!(
        summaries
            .iter()
            .any(|summary| summary.data.navigation_tag == "memory-navigation"
                && summary.references.contains(&first.user))
    );
    assert!(runtime.maintenance_errors().await.is_empty());
}

#[derive(Default)]
struct CorruptDataBacking {
    stored: Mutex<
        std::collections::BTreeMap<
            context_collector::ContextId,
            context_collector::ContextItem<FactData>,
        >,
    >,
}

impl ColdBacking<FactData> for CorruptDataBacking {
    fn store(
        &self,
        object: &context_collector::ContextItem<FactData>,
    ) -> Result<(), ExternalError> {
        self.stored
            .lock()
            .unwrap()
            .insert(object.id, object.clone());
        Ok(())
    }

    fn load(
        &self,
        id: context_collector::ContextId,
    ) -> Result<Option<context_collector::ContextItem<FactData>>, ExternalError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .map(|mut object| {
                if let InfoKind::Info(info) = &mut object.kind {
                    info.data.category = "changed after store".into();
                }
                object
            }))
    }
}

#[tokio::test]
async fn changed_user_data_fails_exact_reload_before_cold_removal() {
    let runtime = Runtime::<FactData, SummaryData>::with_counter(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 4 }; 5],
            hot_high: 100,
            processing_batch_tokens: 1024,
            max_processing_failures: 3,
        },
        Arc::new(Bytes),
        Arc::new(UserInfoRefiner {
            model: "chosen-by-user".into(),
            prompt: "extract".into(),
            observed: Mutex::new(Vec::new()),
        }),
        Arc::new(UserSummarizer {
            tag: "memory-navigation".into(),
        }),
        Arc::new(CorruptDataBacking::default()),
    )
    .unwrap();
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
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                context_collector::RuntimeError::ColdCompaction(
                    context_collector::cold::ColdCompactorError::Backing(_)
                )
            ))
    );
    assert!(
        runtime
            .cold_scope_entries(first.scope)
            .await
            .iter()
            .any(|entry| entry.location == context_collector::cold::CatalogLocation::ColdZone)
    );
}

#[derive(Debug, thiserror::Error)]
#[error("application model call failed")]
struct UserTaskError;

struct FailingInfoRefiner;
impl InfoRefiner<FactData> for FailingInfoRefiner {
    fn refine<'a>(
        &'a self,
        _input: RefinementInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<RefinementResult<FactData>, ExternalError>> + Send + 'a>>
    {
        Box::pin(async { Err(Arc::new(UserTaskError) as ExternalError) })
    }
}

#[tokio::test]
async fn task_failure_retains_application_error_as_source() {
    let runtime = Runtime::<FactData, SummaryData>::with_counter(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 4 }; 5],
            hot_high: 100,
            processing_batch_tokens: 1024,
            max_processing_failures: 3,
        },
        Arc::new(Bytes),
        Arc::new(FailingInfoRefiner),
        Arc::new(context_collector::NoopScopeSummarizer),
        Arc::new(InMemoryColdBacking::<FactData>::default()),
    )
    .unwrap();
    runtime
        .complete_turn("source".into(), "agent".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("next".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                context_collector::RuntimeError::Refinement(
                    context_collector::compaction::RefinementError::InfoRefiner(source)
                ) if source.downcast_ref::<UserTaskError>().is_some()
            ))
    );
}

struct ChangingDataInfoRefiner {
    calls: std::sync::atomic::AtomicUsize,
}

impl InfoRefiner<FactData> for ChangingDataInfoRefiner {
    fn refine<'a>(
        &'a self,
        input: RefinementInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<RefinementResult<FactData>, ExternalError>> + Send + 'a>>
    {
        Box::pin(async move {
            let raw = &input.raw[0];
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![InfoDraft {
                content: "same grounded fact".into(),
                sources: vec![SourceSpan {
                    raw: raw.id,
                    revision: raw.revision,
                    start: 0,
                    end: 1,
                }],
                data: FactData {
                    category: "same".into(),
                    model_used: format!("call-{call}"),
                },
            }]
            .into())
        })
    }
}

#[tokio::test]
async fn changing_user_metadata_does_not_duplicate_the_same_grounded_info() {
    let refiner = Arc::new(ChangingDataInfoRefiner {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let runtime = Runtime::<FactData, SummaryData>::with_counter(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 4 }; 5],
            hot_high: 100,
            processing_batch_tokens: 1024,
            max_processing_failures: 3,
        },
        Arc::new(Bytes),
        refiner.clone(),
        Arc::new(context_collector::NoopScopeSummarizer),
        Arc::new(InMemoryColdBacking::<FactData>::default()),
    )
    .unwrap();
    let first = runtime
        .complete_turn("source".into(), "agent".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("next".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    runtime
        .complete_turn("again".into(), "reply".into(), TurnObservation::default())
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    assert!(refiner.calls.load(std::sync::atomic::Ordering::SeqCst) > 1);
    let same_fact_count = runtime
        .context_view(&[])
        .await
        .unwrap()
        .notes
        .into_iter()
        .filter(|note| note.content == "same grounded fact" && note.sources.contains(&first.user))
        .count();
    assert_eq!(same_fact_count, 1);
}
