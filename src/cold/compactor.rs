use std::collections::BTreeSet;
use std::sync::Arc;

use crate::cold::{
    BackingRecord, CatalogLocation, ColdBacking, ColdCompactionBatch, ScopeSummary,
    VerifiedColdCompactionBatch,
};
use crate::compaction::scope_summary::{ScopeSummarizer, ScopeSummaryInput, ScopeSummaryProposal};
use crate::context::{ContextId, ScopeId};
use crate::heap::{ContextHeap, ZoneKind};

use super::ColdCatalog;

#[derive(Debug)]
pub(crate) enum ColdCompactorError {
    Summary(String),
    InvalidSummary(String),
    Backing(String),
    Worker(String),
    Invariant(&'static str),
}

pub(crate) struct ColdCompactor {
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

    pub fn prepare(heap: &ContextHeap, scope: ScopeId) -> ColdCompactionBatch {
        if !heap.zone(ZoneKind::Cold).above_high() {
            return ColdCompactionBatch {
                scope,
                records: Vec::new(),
            };
        }
        let mut remaining = heap.zone(ZoneKind::Cold).usage().total();
        let low = heap.zone(ZoneKind::Cold).watermark().low;
        let mut records = Vec::new();
        for id in heap.zone(ZoneKind::Cold).ids_for_scope(scope) {
            if remaining <= low {
                break;
            }
            let entry = heap
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

    pub async fn offload(
        &self,
        batch: ColdCompactionBatch,
    ) -> Result<(VerifiedColdCompactionBatch, Option<ScopeSummaryProposal>), ColdCompactorError>
    {
        let inputs: Vec<_> = batch
            .records()
            .iter()
            .map(|record| ScopeSummaryInput {
                object: record.object.clone(),
            })
            .collect();
        let proposal = self
            .summarizer
            .summarize(batch.scope, &inputs)
            .await
            .map_err(ColdCompactorError::Summary)?;
        let backing = Arc::clone(&self.backing);
        let verified = tokio::task::spawn_blocking(move || {
            for record in batch.records() {
                backing
                    .store(&record.object)
                    .map_err(ColdCompactorError::Backing)?;
            }
            batch
                .verify(backing.as_ref())
                .map_err(ColdCompactorError::Backing)
        })
        .await
        .map_err(|error| ColdCompactorError::Worker(error.to_string()))??;
        Ok((verified, proposal))
    }

    pub fn commit(
        heap: &mut ContextHeap,
        catalog: &mut ColdCatalog,
        verified: VerifiedColdCompactionBatch,
        proposal: Option<ScopeSummaryProposal>,
    ) -> Result<Vec<ContextId>, ColdCompactorError> {
        let batch = verified.into_batch();
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        if let Some(proposal) = &proposal {
            if proposal.content.trim().is_empty() || proposal.references.is_empty() {
                return Err(ColdCompactorError::InvalidSummary(
                    "summary requires content and references".into(),
                ));
            }
            let covered: BTreeSet<_> = batch
                .records()
                .iter()
                .map(|record| record.object.id)
                .collect();
            if proposal.references.iter().any(|id| !covered.contains(id)) {
                return Err(ColdCompactorError::InvalidSummary(
                    "summary references must belong to the selected Scope cohort".into(),
                ));
            }
        }
        for record in batch.records() {
            if record.scope != batch.scope {
                return Err(ColdCompactorError::Invariant("Cold batch mixed scopes"));
            }
            let current = heap
                .zone(ZoneKind::Cold)
                .get(record.object.id)
                .ok_or(ColdCompactorError::Invariant("Cold candidate moved"))?;
            if current.protected
                || current.object != record.object
                || current.scope != record.scope
                || current.tokens != record.tokens
            {
                return Err(ColdCompactorError::Invariant("Cold candidate changed"));
            }
            let catalog = catalog
                .get(record.object.id)
                .ok_or(ColdCompactorError::Invariant(
                    "Cold candidate missing from catalog",
                ))?;
            if catalog.location != CatalogLocation::ColdZone
                || catalog.scope != record.scope
                || catalog.revision != record.object.revision
            {
                return Err(ColdCompactorError::Invariant("Cold catalog entry changed"));
            }
        }
        let coverage = batch
            .records()
            .iter()
            .map(|record| (record.object.id, record.object.revision))
            .collect();
        let mut stored = Vec::new();
        for record in batch.records {
            heap.zone_mut(ZoneKind::Cold)
                .remove(record.object.id)
                .ok_or(ColdCompactorError::Invariant("Cold removal failed"))?;
            catalog.record_backing(record.object.id);
            stored.push(record.object.id);
        }
        if let Some(proposal) = proposal {
            catalog.add_summary(
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
