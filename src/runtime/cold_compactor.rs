use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cold::{
    BackingRecord, CatalogLocation, ColdBacking, ColdCompactionBatch, ScopeSummary,
    VerifiedColdCompactionBatch,
};
use crate::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::context::{ContextId, ScopeId};
use crate::heap::ZoneKind;

use super::{RuntimeError, RuntimeState};

pub(super) struct ColdCompactor {
    summarizer: Arc<dyn ScopeSummarizer>,
    backing: Arc<dyn ColdBacking>,
}

impl ColdCompactor {
    pub fn new(summarizer: Arc<dyn ScopeSummarizer>, backing: Arc<dyn ColdBacking>) -> Self {
        Self {
            summarizer,
            backing,
        }
    }

    pub async fn execute(
        &self,
        shared: &Arc<Mutex<RuntimeState>>,
        scope: ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = {
            let state = shared.lock().await;
            Self::prepare(&state, scope)
        };
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        let inputs: Vec<_> = batch
            .records()
            .iter()
            .map(|record| ScopeSummaryInput {
                object: record.object.clone(),
            })
            .collect();
        let proposal = self
            .summarizer
            .summarize(scope, &inputs)
            .await
            .map_err(RuntimeError::ScopeSummary)?;
        let backing = Arc::clone(&self.backing);
        let verified = tokio::task::spawn_blocking(
            move || -> Result<VerifiedColdCompactionBatch, RuntimeError> {
                for record in batch.records() {
                    backing
                        .store(&record.object)
                        .map_err(RuntimeError::ColdBacking)?;
                }
                batch
                    .verify(backing.as_ref())
                    .map_err(RuntimeError::ColdBacking)
            },
        )
        .await
        .map_err(|error| RuntimeError::Worker(error.to_string()))??;
        let mut state = shared.lock().await;
        let moved = Self::commit(&mut state, verified, proposal)?;
        if !moved.is_empty() {
            state.schedule();
        }
        Ok(moved)
    }

    fn prepare(state: &RuntimeState, scope: ScopeId) -> ColdCompactionBatch {
        if !state.heap.zone(ZoneKind::Cold).above_high() {
            return ColdCompactionBatch {
                scope,
                records: Vec::new(),
            };
        }
        let mut remaining = state.heap.zone(ZoneKind::Cold).usage().total();
        let low = state.heap.zone(ZoneKind::Cold).watermark().low;
        let mut records = Vec::new();
        for id in state.heap.zone(ZoneKind::Cold).ids_for_scope(scope) {
            if remaining <= low {
                break;
            }
            let entry = state
                .heap
                .zone(ZoneKind::Cold)
                .get(id)
                .expect("listed entry exists");
            if entry.protected {
                continue;
            }
            records.push(BackingRecord {
                object: entry.object.clone(),
                scope: entry.scope,
                tokens: entry.tokens,
            });
            remaining = remaining.saturating_sub(entry.tokens);
        }
        ColdCompactionBatch { scope, records }
    }

    fn commit(
        state: &mut RuntimeState,
        verified: VerifiedColdCompactionBatch,
        proposal: Option<ScopeSummaryProposal>,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = verified.into_batch();
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        if let Some(proposal) = &proposal {
            if proposal.content.trim().is_empty() || proposal.references.is_empty() {
                return Err(RuntimeError::InvalidScopeSummary(
                    "summary requires content and references".into(),
                ));
            }
            let covered: BTreeSet<_> = batch
                .records()
                .iter()
                .map(|record| record.object.id)
                .collect();
            if proposal.references.iter().any(|id| !covered.contains(id)) {
                return Err(RuntimeError::InvalidScopeSummary(
                    "summary references must belong to the selected Scope cohort".into(),
                ));
            }
        }
        for record in batch.records() {
            if record.scope != batch.scope {
                return Err(RuntimeError::Invariant("Cold batch mixed scopes"));
            }
            let current = state
                .heap
                .zone(ZoneKind::Cold)
                .get(record.object.id)
                .ok_or(RuntimeError::Invariant("Cold candidate moved"))?;
            if current.protected
                || current.object != record.object
                || current.scope != record.scope
                || current.tokens != record.tokens
            {
                return Err(RuntimeError::Invariant("Cold candidate changed"));
            }
            let catalog = state
                .catalog
                .get(record.object.id)
                .ok_or(RuntimeError::Invariant(
                    "Cold candidate missing from catalog",
                ))?;
            if catalog.location != CatalogLocation::ColdZone
                || catalog.scope != record.scope
                || catalog.revision != record.object.revision
            {
                return Err(RuntimeError::Invariant("Cold catalog entry changed"));
            }
        }
        let coverage = batch
            .records()
            .iter()
            .map(|record| (record.object.id, record.object.revision))
            .collect();
        let mut stored = Vec::new();
        for record in batch.records {
            state
                .heap
                .zone_mut(ZoneKind::Cold)
                .remove(record.object.id)
                .ok_or(RuntimeError::Invariant("Cold removal failed"))?;
            state.catalog.record_backing(record.object.id);
            stored.push(record.object.id);
        }
        if let Some(proposal) = proposal {
            state.catalog.add_summary(
                batch.scope,
                ScopeSummary {
                    content: proposal.content,
                    references: proposal.references,
                    coverage,
                },
            );
        }
        Ok(stored)
    }
}
