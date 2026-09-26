use super::*;
type Runtime = RuntimeState;
use crate::cold::InMemoryColdBacking;
use crate::context::{Representation, SourceSpan};
use crate::objectization::{RawInput, StructuredProposal};
use crate::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use std::sync::{Arc, Mutex};

struct Bytes;
impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

struct FirstSpan;
impl Objectizer for FirstSpan {
    fn extract(&self, inputs: &[RawInput]) -> Result<Vec<StructuredProposal>, String> {
        let first = &inputs[0];
        Ok(vec![StructuredProposal {
            content: "fact".into(),
            sources: vec![SourceSpan {
                raw: first.id,
                revision: first.revision,
                start: 0,
                end: first.content.chars().next().unwrap().len_utf8(),
            }],
        }])
    }
}

struct OpaqueSummaryProvider;
impl ScopeSummarizer for OpaqueSummaryProvider {
    fn summarize(
        &self,
        _scope: ScopeId,
        inputs: &[ScopeSummaryInput],
    ) -> Result<Option<ScopeSummaryProposal>, String> {
        Ok(Some(ScopeSummaryProposal {
            content: "opaque-summary".into(),
            references: inputs.iter().map(|input| input.object.id).collect(),
        }))
    }
}

fn config() -> RuntimeConfig {
    RuntimeConfig {
        watermarks: [
            Watermark { low: 2, high: 4 },
            Watermark { low: 1, high: 3 },
            Watermark { low: 1, high: 3 },
            Watermark { low: 1, high: 3 },
            Watermark { low: 1, high: 3 },
        ],
        hot_high: 100,
    }
}

fn runtime() -> Runtime {
    Runtime::new(
        config(),
        Box::new(Bytes),
        Box::new(FirstSpan),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap()
}

#[test]
fn transition_assigns_entire_turn_and_preserves_old_scope() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("user".into(), "agent".into(), TurnObservation::default())
        .unwrap();
    let second = runtime
        .complete_turn(
            "new".into(),
            "work".into(),
            TurnObservation {
                uses: Some(vec![first.user]),
                scope: Some(ScopeReport::Transition),
            },
        )
        .unwrap();
    assert_ne!(first.scope, second.scope);
    assert_eq!(runtime.scopes.owner_of(first.user), Some(first.scope));
    assert_eq!(runtime.scopes.owner_of(second.user), Some(second.scope));
    assert_eq!(runtime.scopes.owner_of(second.agent), Some(second.scope));
    assert_eq!(
        runtime.heap.find(first.user).unwrap().1.last_used_turn,
        Some(2)
    );
    runtime.select_scope(first.scope).unwrap();
    assert_eq!(runtime.scopes.current(), first.scope);
}

#[test]
fn high_watermark_schedules_minor_and_moves_only_aged_objects() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("ab".into(), "cd".into(), TurnObservation::default())
        .unwrap();
    assert!(runtime.pending_jobs() > 0);
    let result = runtime.run_next_job().unwrap().unwrap();
    assert!(result.affected.is_empty());
    let second = runtime
        .complete_turn("e".into(), "f".into(), TurnObservation::default())
        .unwrap();
    let result = runtime.run_next_job().unwrap().unwrap();
    assert_eq!(result.job, Job::Minor(ZoneKind::Eden));
    assert_eq!(result.affected, vec![first.user, first.agent]);
    assert_eq!(runtime.heap.find(first.user).unwrap().0, ZoneKind::Survivor);
    assert_eq!(runtime.heap.find(second.user).unwrap().0, ZoneKind::Eden);
    assert_eq!(runtime.scopes.owner_of(first.user), Some(first.scope));
    let next = runtime.run_next_job().unwrap().unwrap();
    assert_eq!(next.job, Job::Minor(ZoneKind::Survivor));
    assert!(next.affected.contains(&first.user));
    assert_eq!(runtime.heap.find(first.user).unwrap().0, ZoneKind::Mature);
}

#[test]
fn objectization_stays_in_source_zone_and_keeps_raw_exact() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("αbc".into(), "agent".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(first.user, ZoneKind::Eden, ZoneKind::Mature)
        .unwrap();
    runtime
        .complete_turn("x".into(), "y".into(), TurnObservation::default())
        .unwrap();
    let ids = runtime.objectize(ZoneKind::Mature, first.scope).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(runtime.heap.find(ids[0]).unwrap().0, ZoneKind::Mature);
    assert_eq!(runtime.scopes.owner_of(ids[0]), Some(first.scope));
    assert_eq!(
        runtime.read(first.user).unwrap().unwrap().representation,
        Representation::Raw("αbc".into())
    );
    assert!(matches!(
        runtime.read(ids[0]).unwrap().unwrap().representation,
        Representation::Structured { .. }
    ));
}

#[test]
fn stale_objectization_result_is_rejected_after_zone_move() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("abc".into(), "def".into(), TurnObservation::default())
        .unwrap();
    let prepared =
        Objectization::prepare(&runtime.heap, ZoneKind::Eden, first.scope, &[first.user]).unwrap();
    runtime
        .move_entry(first.user, ZoneKind::Eden, ZoneKind::Survivor)
        .unwrap();
    let proposals = FirstSpan.extract(prepared.inputs()).unwrap();
    assert!(matches!(
        runtime.commit_objectization(prepared, proposals),
        Err(RuntimeError::InvalidObjectization(_))
    ));
}

struct FailingBacking;
impl ColdBacking for FailingBacking {
    fn store(&self, _object: &ContextObject) -> Result<(), String> {
        Err("store failed".into())
    }
    fn load(&self, _id: ContextId) -> Result<Option<ContextObject>, String> {
        Ok(None)
    }
}

#[test]
fn cold_store_failure_keeps_canonical_payload_resident() {
    let mut runtime = Runtime::new(
        config(),
        Box::new(Bytes),
        Box::new(FirstSpan),
        Box::new(FailingBacking),
    )
    .unwrap();
    let turn = runtime
        .complete_turn("abc".into(), "def".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(turn.user, ZoneKind::Eden, ZoneKind::Cooling)
        .unwrap();
    runtime
        .move_entry(turn.user, ZoneKind::Cooling, ZoneKind::Cold)
        .unwrap();
    assert!(matches!(
        runtime.compact_cold(turn.scope),
        Err(RuntimeError::ColdBacking(_))
    ));
    assert_eq!(
        runtime.read(turn.user).unwrap().unwrap().representation,
        Representation::Raw("abc".into())
    );
    assert!(runtime.heap.zone(ZoneKind::Cold).get(turn.user).is_some());
}

struct ObservedBacking {
    items: Arc<Mutex<Vec<ContextId>>>,
}
impl ColdBacking for ObservedBacking {
    fn store(&self, object: &ContextObject) -> Result<(), String> {
        self.items.lock().unwrap().push(object.id);
        Ok(())
    }
    fn load(&self, _id: ContextId) -> Result<Option<ContextObject>, String> {
        Ok(None)
    }
}

#[test]
fn failed_exact_reload_does_not_release_cold_resident() {
    let mut runtime = Runtime::new(
        config(),
        Box::new(Bytes),
        Box::new(FirstSpan),
        Box::new(ObservedBacking {
            items: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .unwrap();
    let turn = runtime
        .complete_turn("abc".into(), "def".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(turn.user, ZoneKind::Eden, ZoneKind::Cold)
        .unwrap();
    assert!(matches!(
        runtime.compact_cold(turn.scope),
        Err(RuntimeError::ColdBacking(_))
    ));
    assert!(runtime.heap.zone(ZoneKind::Cold).get(turn.user).is_some());
}
#[test]
fn cooling_moves_raw_and_structured_from_transitioned_scope() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("abcd".into(), "a".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(first.user, ZoneKind::Eden, ZoneKind::Mature)
        .unwrap();
    runtime
        .complete_turn("b".into(), "c".into(), TurnObservation::default())
        .unwrap();
    let structured = runtime.objectize(ZoneKind::Mature, first.scope).unwrap()[0];
    runtime
        .complete_turn(
            "d".into(),
            "e".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Transition),
            },
        )
        .unwrap();
    runtime
        .complete_turn("f".into(), "g".into(), TurnObservation::default())
        .unwrap();
    let mut cooled = Vec::new();
    while let Some(result) = runtime.run_next_job().unwrap() {
        if result.job == Job::Cooling(ZoneKind::Mature) {
            cooled.extend(result.affected);
        }
    }
    assert!(cooled.contains(&first.user));
    assert!(cooled.contains(&structured));
    assert!(runtime.read(first.user).unwrap().is_some());
    assert!(runtime.read(structured).unwrap().is_some());
    assert_eq!(runtime.scopes.owner_of(structured), Some(first.scope));
}

#[test]
fn cold_compaction_reloads_exact_raw_through_backing() {
    let mut runtime = Runtime::new(
        config(),
        Box::new(Bytes),
        Box::new(crate::objectization::NoopObjectizer),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let first = runtime
        .complete_turn(
            "source payload".into(),
            "a".into(),
            TurnObservation::default(),
        )
        .unwrap();
    runtime
        .move_entry(first.user, ZoneKind::Eden, ZoneKind::Cold)
        .unwrap();
    runtime
        .complete_turn("b".into(), "c".into(), TurnObservation::default())
        .unwrap();
    let mut compacted = false;
    while let Some(result) = runtime.run_next_job().unwrap() {
        if result.job == Job::ColdCompaction(first.scope) {
            compacted = result.affected.contains(&first.user);
        }
    }
    assert!(compacted);
    assert!(runtime.heap.zone(ZoneKind::Cold).get(first.user).is_none());
    assert_eq!(
        runtime.read(first.user).unwrap().unwrap().representation,
        Representation::Raw("source payload".into())
    );
    let view = runtime
        .context_view(TokenSpace(100), &[first.user])
        .unwrap();
    assert!(
        view.cold_scopes
            .iter()
            .any(|entry| entry.scope == first.scope)
    );
    assert!(
        view.items
            .iter()
            .any(|item| item.object.id == first.user && item.zone.is_none())
    );
}

#[test]
fn opaque_summary_is_cataloged_without_exposing_object_rows() {
    let mut runtime = Runtime::with_summarizer(
        config(),
        Box::new(Bytes),
        Box::new(crate::objectization::NoopObjectizer),
        Box::new(OpaqueSummaryProvider),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let turn = runtime
        .complete_turn("abc".into(), "hot".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(turn.user, ZoneKind::Eden, ZoneKind::Cooling)
        .unwrap();
    runtime.run_job(Job::Major).unwrap();
    assert_eq!(
        runtime.catalog.get(turn.user).unwrap().location,
        CatalogLocation::ColdZone
    );
    assert_eq!(runtime.cold_scope_entries(turn.scope).len(), 1);
    assert!(runtime.catalog.summaries(turn.scope).is_empty());

    assert_eq!(runtime.compact_cold(turn.scope).unwrap(), vec![turn.user]);
    assert_eq!(
        runtime.catalog.get(turn.user).unwrap().location,
        CatalogLocation::Backing
    );
    let summary = &runtime.catalog.summaries(turn.scope)[0];
    assert_eq!(summary.references, vec![turn.user]);
    assert_eq!(summary.coverage, vec![(turn.user, 1)]);
    assert_eq!(runtime.scopes.get(turn.scope).unwrap().members().count(), 2);
    let view = runtime.context_view(TokenSpace(300), &[]).unwrap();
    assert_eq!(view.cold_scopes.len(), 1);
    assert_eq!(view.cold_scopes[0].scope, turn.scope);
    assert_eq!(view.cold_scopes[0].object_count, 1);
    assert_eq!(view.cold_scopes[0].summaries.len(), 1);
    assert_eq!(view.cold_scopes[0].summaries[0].content, "opaque-summary");
    assert_eq!(view.cold_scopes[0].summaries[0].references, vec![turn.user]);
    assert_eq!(
        runtime.read(turn.user).unwrap().unwrap().representation,
        Representation::Raw("abc".into())
    );
}

struct FailingScopeSummary;
impl ScopeSummarizer for FailingScopeSummary {
    fn summarize(
        &self,
        _scope: ScopeId,
        _inputs: &[ScopeSummaryInput],
    ) -> Result<Option<ScopeSummaryProposal>, String> {
        Err("summary failed".into())
    }
}

#[test]
fn summary_failure_keeps_cold_scope_payload_available() {
    let mut runtime = Runtime::with_summarizer(
        config(),
        Box::new(Bytes),
        Box::new(crate::objectization::NoopObjectizer),
        Box::new(FailingScopeSummary),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let turn = runtime
        .complete_turn("abc".into(), "x".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(turn.user, ZoneKind::Eden, ZoneKind::Cold)
        .unwrap();
    assert!(matches!(
        runtime.compact_cold(turn.scope),
        Err(RuntimeError::ScopeSummary(_))
    ));
    assert_eq!(
        runtime.catalog.get(turn.user).unwrap().location,
        CatalogLocation::ColdZone
    );
    assert!(runtime.catalog.summaries(turn.scope).is_empty());
    assert!(runtime.read(turn.user).unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_cold_job_publishes_opaque_summary_after_backing_reload() {
    let mut state = Runtime::with_summarizer(
        config(),
        Box::new(Bytes),
        Box::new(crate::objectization::NoopObjectizer),
        Box::new(OpaqueSummaryProvider),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let turn = state
        .complete_turn("abc".into(), "x".into(), TurnObservation::default())
        .unwrap();
    state
        .move_entry(turn.user, ZoneKind::Eden, ZoneKind::Cold)
        .unwrap();
    state.reschedule();
    let runtime = crate::Runtime::from_state(state);
    runtime.drain_maintenance().await;
    assert!(runtime.maintenance_errors().await.is_empty());
    assert_eq!(runtime.zone_of(turn.user).await, None);
    let view = runtime.context_view(TokenSpace(300), &[]).await.unwrap();
    assert_eq!(view.cold_scopes[0].summaries[0].references, vec![turn.user]);
    assert_eq!(
        runtime
            .read(turn.user)
            .await
            .unwrap()
            .unwrap()
            .representation,
        Representation::Raw("abc".into())
    );
}
struct BlockingBacking {
    objects: Mutex<std::collections::BTreeMap<ContextId, ContextObject>>,
    started: std::sync::mpsc::Sender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl ColdBacking for BlockingBacking {
    fn store(&self, object: &ContextObject) -> Result<(), String> {
        self.started.send(()).map_err(|error| error.to_string())?;
        self.resume
            .lock()
            .map_err(|error| error.to_string())?
            .recv()
            .map_err(|error| error.to_string())?;
        self.objects
            .lock()
            .map_err(|error| error.to_string())?
            .insert(object.id, object.clone());
        Ok(())
    }
    fn load(&self, id: ContextId) -> Result<Option<ContextObject>, String> {
        Ok(self
            .objects
            .lock()
            .map_err(|error| error.to_string())?
            .get(&id)
            .cloned())
    }
}

#[test]
fn cold_preparation_keeps_read_available_during_external_store() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let mut runtime = Runtime::new(
        config(),
        Box::new(Bytes),
        Box::new(crate::objectization::NoopObjectizer),
        Box::new(BlockingBacking {
            objects: Mutex::new(std::collections::BTreeMap::new()),
            started: started_tx,
            resume: Mutex::new(resume_rx),
        }),
    )
    .unwrap();
    let first = runtime
        .complete_turn("abc".into(), "x".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(first.user, ZoneKind::Eden, ZoneKind::Cold)
        .unwrap();
    let batch = runtime.prepare_cold_compaction(first.scope);
    let backing = runtime.backing_handle();
    let work = batch.clone();
    let worker = std::thread::spawn(move || {
        for record in work.records() {
            backing.store(&record.object).unwrap();
        }
    });
    started_rx.recv().unwrap();
    assert_eq!(
        runtime.read(first.user).unwrap().unwrap().representation,
        Representation::Raw("abc".into())
    );
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    let verify_backing = runtime.backing_handle();
    let verified = batch.verify(verify_backing.as_ref()).unwrap();
    runtime.commit_cold_compaction(verified, None).unwrap();
    assert!(runtime.heap.zone(ZoneKind::Cold).get(first.user).is_none());
    assert_eq!(
        runtime.read(first.user).unwrap().unwrap().representation,
        Representation::Raw("abc".into())
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_cold_store_does_not_block_runtime_read() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let mut runtime = Runtime::new(
        config(),
        Box::new(Bytes),
        Box::new(crate::objectization::NoopObjectizer),
        Box::new(BlockingBacking {
            objects: Mutex::new(std::collections::BTreeMap::new()),
            started: started_tx,
            resume: Mutex::new(resume_rx),
        }),
    )
    .unwrap();
    let first = runtime
        .complete_turn("abc".into(), "x".into(), TurnObservation::default())
        .unwrap();
    runtime
        .move_entry(first.user, ZoneKind::Eden, ZoneKind::Cold)
        .unwrap();
    let async_runtime = crate::Runtime::from_state(runtime);
    async_runtime
        .complete_turn("b".into(), "c".into(), TurnObservation::default())
        .await
        .unwrap();
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(std::time::Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    let object = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        async_runtime.read(first.user),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(object.representation, Representation::Raw("abc".into()));
    resume_tx.send(()).unwrap();
    async_runtime.drain_maintenance().await;
    assert_eq!(
        async_runtime
            .read(first.user)
            .await
            .unwrap()
            .unwrap()
            .representation,
        Representation::Raw("abc".into())
    );
    assert_eq!(async_runtime.zone_of(first.user).await, None);
    assert!(async_runtime.maintenance_errors().await.is_empty());
}
