use context_collector::error::ExternalError;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use context_collector::cold::{CatalogLocation, ColdBacking};
use context_collector::compaction::objectization::{
    ObjectizationInput, Objectizer, StructuredProposal,
};
use context_collector::compaction::scope_summary::{
    ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal,
};
use context_collector::{
    ContextId, ContextObject, NoopObjectizer, Representation, Runtime, RuntimeConfig, RuntimeError,
    ScopeId, ScopeReport, SourceSpan, TokenCounter, TurnObservation, Watermark, ZoneKind,
};
use tokio::sync::Notify;

struct Bytes;
impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

fn config() -> RuntimeConfig {
    RuntimeConfig {
        watermarks: [Watermark { low: 1, high: 4 }; 5],
        hot_high: 100,
    }
}

struct FailingBacking;
impl ColdBacking for FailingBacking {
    fn store(&self, _object: &ContextObject) -> Result<(), ExternalError> {
        Err(Arc::new(std::io::Error::other("store failed")))
    }
    fn load(&self, _id: ContextId) -> Result<Option<ContextObject>, ExternalError> {
        Ok(None)
    }
}

struct NoSummary;
impl ScopeSummarizer for NoSummary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        _inputs: &'a [ScopeSummaryInput],
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, ExternalError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(None) })
    }
}

struct PausedSummary {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl ScopeSummarizer for PausedSummary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        inputs: &'a [ScopeSummaryInput],
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, ExternalError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(Some(ScopeSummaryProposal {
                content: "summary".into(),
                references: inputs.iter().map(|input| input.object.id).collect(),
                covered: inputs.iter().map(|input| input.object.id).collect(),
                data: (),
            }))
        })
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
    (first.user, first.scope)
}

#[tokio::test]
async fn backing_failure_keeps_cold_payload_and_catalog_location() {
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(NoopObjectizer),
        Arc::new(NoSummary),
        Arc::new(FailingBacking),
    )
    .unwrap();
    let (id, scope) = drive_to_cold(&runtime).await;
    runtime.drain_maintenance().await;
    assert_eq!(runtime.zone_of(id).await, Some(ZoneKind::Cold));
    assert_eq!(
        runtime.read(id).await.unwrap().unwrap().representation,
        Representation::Raw("source payload".into())
    );
    let entries = runtime.cold_scope_entries(scope).await;
    assert!(
        entries
            .iter()
            .any(|entry| entry.id == id && entry.location == CatalogLocation::ColdZone)
    );
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                RuntimeError::ColdCompaction(context_collector::cold::ColdCompactorError::Backing(
                    _
                ))
            ))
    );
}

#[tokio::test]
async fn protected_cold_candidate_cannot_commit_after_summary_started() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(NoopObjectizer),
        Arc::new(PausedSummary {
            entered: entered.clone(),
            release: release.clone(),
        }),
        Arc::new(context_collector::InMemoryColdBacking::default()),
    )
    .unwrap();
    let (id, _) = drive_to_cold(&runtime).await;
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert_eq!(runtime.zone_of(id).await, Some(ZoneKind::Cold));
    let raw = tokio::time::timeout(Duration::from_secs(2), runtime.read(id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.representation,
        Representation::Raw("source payload".into())
    );
    runtime.protect(id, true).await.unwrap();
    release.notify_one();
    runtime.drain_maintenance().await;
    assert_eq!(runtime.zone_of(id).await, Some(ZoneKind::Cold));
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                RuntimeError::ColdCompaction(
                    context_collector::cold::ColdCompactorError::Invariant(
                        "Cold candidate changed"
                    )
                )
            ))
    );
}

#[derive(Default)]
struct CorruptReloadBacking {
    stored: Mutex<std::collections::BTreeMap<ContextId, ContextObject>>,
}
impl ColdBacking for CorruptReloadBacking {
    fn store(&self, object: &ContextObject) -> Result<(), ExternalError> {
        self.stored
            .lock()
            .unwrap()
            .insert(object.id, object.clone());
        Ok(())
    }
    fn load(&self, id: ContextId) -> Result<Option<ContextObject>, ExternalError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .map(|mut object| {
                object.revision += 1;
                object
            }))
    }
}

struct BadReferenceSummary;
impl ScopeSummarizer for BadReferenceSummary {
    fn summarize<'a>(
        &'a self,
        _scope: ScopeId,
        _inputs: &'a [ScopeSummaryInput],
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<ScopeSummaryProposal>, ExternalError>> + Send + 'a>,
    > {
        Box::pin(async {
            Ok(Some(ScopeSummaryProposal {
                content: "bad".into(),
                references: vec![ContextId(999)],
                covered: vec![ContextId(999)],
                data: (),
            }))
        })
    }
}

struct BadSourceObjectizer;
impl Objectizer for BadSourceObjectizer {
    fn extract<'a>(
        &'a self,
        _input: ObjectizationInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StructuredProposal>, ExternalError>> + Send + 'a>>
    {
        Box::pin(async {
            Ok(vec![StructuredProposal {
                content: "invented".into(),
                data: (),
                sources: vec![SourceSpan {
                    raw: ContextId(999),
                    revision: 1,
                    start: 0,
                    end: 1,
                }],
            }])
        })
    }
}

#[tokio::test]
async fn corrupt_exact_reload_keeps_cold_payload_resident() {
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(NoopObjectizer),
        Arc::new(NoSummary),
        Arc::new(CorruptReloadBacking::default()),
    )
    .unwrap();
    let (id, scope) = drive_to_cold(&runtime).await;
    runtime.drain_maintenance().await;
    assert_eq!(runtime.zone_of(id).await, Some(ZoneKind::Cold));
    assert!(
        runtime
            .cold_scope_entries(scope)
            .await
            .iter()
            .any(|entry| entry.id == id && entry.location == CatalogLocation::ColdZone)
    );
    assert_eq!(
        runtime.read(id).await.unwrap().unwrap().representation,
        Representation::Raw("source payload".into())
    );
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                RuntimeError::ColdCompaction(context_collector::cold::ColdCompactorError::Backing(
                    _
                ))
            ))
    );
}

#[tokio::test]
async fn summary_reference_outside_cold_cohort_is_rejected() {
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(NoopObjectizer),
        Arc::new(BadReferenceSummary),
        Arc::new(context_collector::InMemoryColdBacking::default()),
    )
    .unwrap();
    let (id, scope) = drive_to_cold(&runtime).await;
    runtime.drain_maintenance().await;
    assert_eq!(runtime.zone_of(id).await, Some(ZoneKind::Cold));
    assert!(
        runtime
            .cold_scope_entries(scope)
            .await
            .iter()
            .any(|entry| entry.id == id && entry.location == CatalogLocation::ColdZone)
    );
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                RuntimeError::ColdCompaction(
                    context_collector::cold::ColdCompactorError::InvalidSummary(_)
                )
            ))
    );
}

#[tokio::test]
async fn ungrounded_structured_proposal_is_rejected() {
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(BadSourceObjectizer),
        Arc::new(NoSummary),
        Arc::new(context_collector::InMemoryColdBacking::default()),
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
    assert_eq!(
        runtime
            .read(first.user)
            .await
            .unwrap()
            .unwrap()
            .representation,
        Representation::Raw("source".into())
    );
    assert!(
        runtime
            .maintenance_errors()
            .await
            .iter()
            .any(|error| matches!(
                error,
                RuntimeError::Objectization(
                    context_collector::compaction::ObjectizationError::Invalid(_)
                )
            ))
    );
}

#[derive(Default)]
struct LaterCorruptBacking {
    stored: Mutex<std::collections::BTreeMap<ContextId, ContextObject>>,
    corrupt: std::sync::atomic::AtomicBool,
}
impl ColdBacking for LaterCorruptBacking {
    fn store(&self, object: &ContextObject) -> Result<(), ExternalError> {
        self.stored
            .lock()
            .unwrap()
            .insert(object.id, object.clone());
        Ok(())
    }
    fn load(&self, id: ContextId) -> Result<Option<ContextObject>, ExternalError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .map(|mut object| {
                if self.corrupt.load(std::sync::atomic::Ordering::SeqCst) {
                    object.revision += 1;
                }
                object
            }))
    }
}

#[tokio::test]
async fn later_backing_revision_mismatch_is_not_returned_as_canonical() {
    let backing = Arc::new(LaterCorruptBacking::default());
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(NoopObjectizer),
        Arc::new(NoSummary),
        backing.clone(),
    )
    .unwrap();
    let (id, scope) = drive_to_cold(&runtime).await;
    runtime.drain_maintenance().await;
    assert!(
        runtime
            .cold_scope_entries(scope)
            .await
            .iter()
            .any(|entry| entry.id == id && entry.location == CatalogLocation::Backing)
    );
    backing
        .corrupt
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        runtime.read(id).await,
        Err(RuntimeError::Invariant(
            "backing returned wrong identity or revision"
        ))
    ));
    assert!(matches!(
        runtime
            .context_view(context_collector::TokenSpace(500), &[id])
            .await,
        Err(RuntimeError::View(
            context_collector::view::ViewError::Invariant(
                "backing returned wrong identity or revision"
            )
        ))
    ));
}

struct PausedObjectizer {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Objectizer for PausedObjectizer {
    fn extract<'a>(
        &'a self,
        input: ObjectizationInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<StructuredProposal>, ExternalError>> + Send + 'a>>
    {
        Box::pin(async move {
            let raw = &input.raw[0];
            self.entered.notify_one();
            self.release.notified().await;
            Ok(vec![StructuredProposal {
                content: "late fact".into(),
                sources: vec![SourceSpan {
                    raw: raw.id,
                    revision: raw.revision,
                    start: 0,
                    end: 1,
                }],
                data: (),
            }])
        })
    }
}

#[tokio::test]
async fn protected_raw_is_rejected_when_objectizer_returns_later() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = Runtime::with_counter(
        config(),
        Arc::new(Bytes),
        Arc::new(PausedObjectizer {
            entered: entered.clone(),
            release: release.clone(),
        }),
        Arc::new(NoSummary),
        Arc::new(context_collector::InMemoryColdBacking::default()),
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
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    runtime.protect(first.user, true).await.unwrap();
    release.notify_one();
    runtime.drain_maintenance().await;
    assert!(runtime.maintenance_errors().await.iter().any(|error| matches!(
        error,
        RuntimeError::Objectization(
            context_collector::compaction::ObjectizationError::Invalid(
                context_collector::compaction::objectization::ObjectizationValidationError::SourceProtected
            )
        )
    )));
    assert_eq!(
        runtime
            .read(first.user)
            .await
            .unwrap()
            .unwrap()
            .representation,
        Representation::Raw("source".into())
    );
}
