use super::*;
use crate::cold::InMemoryColdBacking;
use crate::context::SourceSpan;
use crate::objectization::NoopObjectizer;
use crate::objectization::{Objectizer, RawInput, StructuredProposal};
use crate::scope_summary::NoopScopeSummarizer;
use std::collections::BTreeSet;

struct Bytes;

impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

fn runtime() -> RuntimeState {
    RuntimeState::with_summarizer(
        RuntimeConfig {
            watermarks: [
                Watermark { low: 1, high: 100 },
                Watermark { low: 1, high: 100 },
                Watermark { low: 1, high: 100 },
                Watermark { low: 1, high: 100 },
                Watermark { low: 1, high: 4 },
            ],
            hot_high: 1000,
        },
        Box::new(Bytes),
        Box::new(NoopObjectizer),
        Box::new(NoopScopeSummarizer),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap()
}

fn put_in_cold(runtime: &mut RuntimeState, id: ContextId) {
    CollectionManager::move_entry(&mut runtime.heap, id, ZoneKind::Eden, ZoneKind::Cold).unwrap();
    let entry = runtime.heap.zone(ZoneKind::Cold).get(id).unwrap();
    runtime.catalog.record_cold(entry);
}

fn assert_model_invariants(runtime: &RuntimeState) {
    let mut all_ids = BTreeSet::new();
    for zone in ZoneKind::ALL {
        let space = runtime.heap.zone(zone);
        let mut raw = 0;
        let mut structured = 0;
        let mut block_ids = BTreeSet::new();
        for block in space.blocks() {
            for id in block.ids() {
                assert!(block_ids.insert(*id));
                assert_eq!(space.get(*id).unwrap().scope, block.scope);
            }
        }
        let entry_ids: BTreeSet<_> = space.entries().map(|entry| entry.id).collect();
        assert_eq!(block_ids, entry_ids);
        for entry in space.entries() {
            assert!(all_ids.insert(entry.id));
            assert_eq!(entry.raw, entry.object.representation.is_raw());
            assert_eq!(
                entry.tokens,
                runtime.counter.count(entry.object.representation.content())
            );
            assert_eq!(runtime.scopes.owner_of(entry.id), Some(entry.scope));
            if entry.raw {
                raw += entry.tokens;
            } else {
                structured += entry.tokens;
            }
            if zone == ZoneKind::Cold {
                let catalog = runtime.catalog.get(entry.id).unwrap();
                assert_eq!(catalog.location, CatalogLocation::ColdZone);
                assert_eq!(catalog.scope, entry.scope);
                assert_eq!(catalog.revision, entry.object.revision);
            }
        }
        assert_eq!(space.usage().raw, raw);
        assert_eq!(space.usage().structured, structured);
    }
    for entry in runtime.catalog.entries() {
        assert_eq!(runtime.scopes.owner_of(entry.id), Some(entry.scope));
        match entry.location {
            CatalogLocation::ColdZone => {
                assert!(runtime.heap.zone(ZoneKind::Cold).get(entry.id).is_some());
            }
            CatalogLocation::Backing => {
                assert!(all_ids.insert(entry.id));
                assert!(runtime.heap.find(entry.id).is_none());
                let object = runtime.read(entry.id).unwrap().unwrap();
                assert_eq!(object.id, entry.id);
                assert_eq!(object.revision, entry.revision);
            }
        }
    }
    let mut all_members = BTreeSet::new();
    for scope in runtime.scopes.all() {
        for id in scope.members() {
            assert!(all_ids.contains(&id));
            assert!(all_members.insert(id));
        }
        for summary in runtime.catalog.summaries(scope.id) {
            let covered: BTreeSet<_> = summary.coverage.iter().map(|(id, _)| *id).collect();
            assert!(summary.references.iter().all(|id| covered.contains(id)));
            for (id, revision) in &summary.coverage {
                let entry = runtime.catalog.get(*id).unwrap();
                assert_eq!(entry.scope, scope.id);
                assert_eq!(entry.revision, *revision);
            }
        }
    }
    assert_eq!(all_members, all_ids);
}

#[test]
fn missing_use_and_uncertain_scope_are_not_negative_evidence() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("first".into(), "reply".into(), TurnObservation::default())
        .unwrap();
    let uncertain = runtime
        .complete_turn(
            "more".into(),
            "reply".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Uncertain),
            },
        )
        .unwrap();
    assert_eq!(uncertain.scope, first.scope);
    let next = runtime
        .complete_turn(
            "different".into(),
            "reply".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Transition),
            },
        )
        .unwrap();
    assert_ne!(next.scope, first.scope);
    assert_model_invariants(&runtime);
    assert_eq!(runtime.scopes.owner_of(first.user), Some(first.scope));
    assert_eq!(runtime.heap.find(first.user).unwrap().0, ZoneKind::Eden);
    assert!(runtime.scheduler.pending() == 0);
}

#[test]
fn synthetic_scope_report_sequences_preserve_runtime_invariants() {
    let reports = [
        None,
        Some(ScopeReport::Continue),
        Some(ScopeReport::Transition),
        Some(ScopeReport::Uncertain),
    ];
    for first_report in reports {
        for second_report in reports {
            for third_report in reports {
                let mut runtime = RuntimeState::new(
                    RuntimeConfig {
                        watermarks: [Watermark { low: 1, high: 4 }; 5],
                        hot_high: 8,
                    },
                    Box::new(Bytes),
                    Box::new(NoopObjectizer),
                    Box::new(InMemoryColdBacking::default()),
                )
                .unwrap();
                let mut expected_raw = Vec::new();
                for (turn_index, scope_report) in [first_report, second_report, third_report]
                    .into_iter()
                    .enumerate()
                {
                    let previous_scope = runtime.scopes.current();
                    let user = format!("u{turn_index}");
                    let agent = format!("a{turn_index}");
                    let receipt = runtime
                        .complete_turn(
                            user.clone(),
                            agent.clone(),
                            TurnObservation {
                                uses: expected_raw.first().map(|(id, _)| vec![*id]),
                                scope: scope_report,
                            },
                        )
                        .unwrap();
                    if scope_report == Some(ScopeReport::Transition) {
                        assert_ne!(receipt.scope, previous_scope);
                    } else {
                        assert_eq!(receipt.scope, previous_scope);
                    }
                    expected_raw.push((receipt.user, user));
                    expected_raw.push((receipt.agent, agent));
                    assert_model_invariants(&runtime);
                    for _ in 0..128 {
                        if runtime.run_next_job().unwrap().is_none() {
                            break;
                        }
                        assert_model_invariants(&runtime);
                    }
                    assert_eq!(runtime.pending_jobs(), 0);
                    for (id, content) in &expected_raw {
                        let object = runtime.read(*id).unwrap().unwrap();
                        assert_eq!(object.representation.content(), content);
                    }
                    let view = runtime.context_view(TokenSpace(16), &[]).unwrap();
                    assert!(view.used_tokens <= 16);
                }
            }
        }
    }
}

struct FixedProposal;

impl Objectizer for FixedProposal {
    fn extract(&self, inputs: &[RawInput]) -> Result<Vec<StructuredProposal>, String> {
        Ok(vec![StructuredProposal {
            content: "ssss".into(),
            sources: vec![SourceSpan {
                raw: inputs[0].id,
                revision: inputs[0].revision,
                start: 0,
                end: inputs[0].content.len(),
            }],
        }])
    }
}

#[test]
fn raw_and_structured_both_contribute_to_zone_pressure() {
    let mut config = RuntimeConfig {
        watermarks: [Watermark { low: 1, high: 100 }; 5],
        hot_high: 1000,
    };
    config.watermarks[2] = Watermark { low: 1, high: 4 };
    let mut runtime = RuntimeState::with_summarizer(
        config,
        Box::new(Bytes),
        Box::new(FixedProposal),
        Box::new(NoopScopeSummarizer),
        Box::new(InMemoryColdBacking::default()),
    )
    .unwrap();
    let first = runtime
        .complete_turn("aaaa".into(), "x".into(), TurnObservation::default())
        .unwrap();
    CollectionManager::move_entry(
        &mut runtime.heap,
        first.user,
        ZoneKind::Eden,
        ZoneKind::Mature,
    )
    .unwrap();
    runtime
        .complete_turn("y".into(), "z".into(), TurnObservation::default())
        .unwrap();
    let made = runtime.objectize(ZoneKind::Mature, first.scope).unwrap();
    assert_eq!(made.len(), 1);
    let usage = runtime.heap.zone(ZoneKind::Mature).usage();
    assert_eq!(usage.raw, 4);
    assert_eq!(usage.structured, 4);
    assert_eq!(usage.total(), 8);
    assert!(runtime.heap.zone(ZoneKind::Mature).above_high());
    assert_eq!(runtime.heap.find(made[0]).unwrap().0, ZoneKind::Mature);
    assert_eq!(runtime.scopes.owner_of(made[0]), Some(first.scope));
    assert_model_invariants(&runtime);
}

#[test]
fn cold_batches_are_partitioned_by_scope_even_under_shared_watermark() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("aaaa".into(), "x".into(), TurnObservation::default())
        .unwrap();
    let second = runtime
        .complete_turn(
            "bbbb".into(),
            "y".into(),
            TurnObservation {
                scope: Some(ScopeReport::Transition),
                ..TurnObservation::default()
            },
        )
        .unwrap();
    put_in_cold(&mut runtime, first.user);
    put_in_cold(&mut runtime, second.user);
    assert_model_invariants(&runtime);
    let first_batch = runtime.prepare_cold_compaction(first.scope);
    let second_batch = runtime.prepare_cold_compaction(second.scope);
    assert_eq!(
        first_batch
            .records()
            .iter()
            .map(|r| r.object.id)
            .collect::<Vec<_>>(),
        vec![first.user]
    );
    assert_eq!(
        second_batch
            .records()
            .iter()
            .map(|r| r.object.id)
            .collect::<Vec<_>>(),
        vec![second.user]
    );

    runtime.reschedule();
    let scheduled: Vec<_> = std::iter::from_fn(|| runtime.take_job()).collect();
    assert!(scheduled.contains(&Job::ColdCompaction(first.scope)));
    assert!(scheduled.contains(&Job::ColdCompaction(second.scope)));

    runtime.run_job(Job::ColdCompaction(first.scope)).unwrap();
    assert_model_invariants(&runtime);
    assert_eq!(
        runtime.catalog.get(first.user).unwrap().location,
        CatalogLocation::Backing
    );
    assert!(runtime.catalog.summaries(second.scope).is_empty());
    assert!(runtime.heap.zone(ZoneKind::Cold).get(second.user).is_some());
    assert_eq!(
        runtime.catalog.get(second.user).unwrap().location,
        CatalogLocation::ColdZone
    );

    runtime.run_job(Job::ColdCompaction(second.scope)).unwrap();
    assert_model_invariants(&runtime);
    assert!(runtime.catalog.summaries(first.scope).is_empty());
    assert!(runtime.catalog.summaries(second.scope).is_empty());
    let view = runtime.context_view(TokenSpace(300), &[]).unwrap();
    assert_eq!(view.cold_scopes.len(), 2);
    assert!(
        view.cold_scopes
            .iter()
            .any(|item| item.scope == first.scope)
    );
    assert!(
        view.cold_scopes
            .iter()
            .any(|item| item.scope == second.scope)
    );
    assert_eq!(runtime.read(first.user).unwrap().unwrap().id, first.user);
    assert_eq!(runtime.read(second.user).unwrap().unwrap().id, second.user);
}

#[test]
fn verified_cold_batch_cannot_commit_after_candidate_becomes_protected() {
    let mut runtime = runtime();
    let turn = runtime
        .complete_turn("aaaa".into(), "x".into(), TurnObservation::default())
        .unwrap();
    put_in_cold(&mut runtime, turn.user);
    let batch = runtime.prepare_cold_compaction(turn.scope);
    let backing = runtime.backing_handle();
    for record in batch.records() {
        backing.store(&record.object).unwrap();
    }
    let verified = batch.verify(backing.as_ref()).unwrap();
    runtime.protect(turn.user, true).unwrap();
    assert!(matches!(
        runtime.commit_cold_compaction(verified, None),
        Err(RuntimeError::Invariant("Cold candidate changed"))
    ));
    assert!(runtime.catalog.summaries(turn.scope).is_empty());
    assert!(runtime.heap.zone(ZoneKind::Cold).get(turn.user).is_some());
}

#[test]
fn summary_reference_outside_prepared_cohort_is_rejected() {
    let mut runtime = runtime();
    let first = runtime
        .complete_turn("aaaa".into(), "x".into(), TurnObservation::default())
        .unwrap();
    let second = runtime
        .complete_turn(
            "bbbb".into(),
            "y".into(),
            TurnObservation {
                scope: Some(ScopeReport::Transition),
                ..TurnObservation::default()
            },
        )
        .unwrap();
    put_in_cold(&mut runtime, first.user);
    put_in_cold(&mut runtime, second.user);
    let batch = runtime.prepare_cold_compaction(first.scope);
    let backing = runtime.backing_handle();
    for record in batch.records() {
        backing.store(&record.object).unwrap();
    }
    let verified = batch.verify(backing.as_ref()).unwrap();
    assert!(matches!(
        runtime.commit_cold_compaction(
            verified,
            Some(ScopeSummaryProposal {
                content: "opaque".into(),
                references: vec![second.user],
            }),
        ),
        Err(RuntimeError::InvalidScopeSummary(_))
    ));
    assert!(runtime.heap.zone(ZoneKind::Cold).get(first.user).is_some());
    assert!(runtime.catalog.summaries(first.scope).is_empty());
}
