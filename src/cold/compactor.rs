use std::sync::Arc;

use thiserror::Error;

use crate::context::ContextId;
use crate::error::ExternalError;
use crate::heap::{ContextHeap, ZoneKind};

use super::{
    BackingRecord, CatalogLocation, ColdBacking, ColdCatalog, ColdCompactionBatch,
    VerifiedColdCompactionBatch,
};

#[derive(Clone, Debug, Error)]
pub enum ColdCompactorError {
    #[error("Cold backing failed")]
    Backing(#[source] ExternalError),
    #[error("Cold compaction worker failed")]
    Worker(#[source] ExternalError),
    #[error("Cold compaction invariant failed: {0}")]
    Invariant(&'static str),
}

pub(crate) struct ColdCompactor<Data = ()> {
    backing: Arc<dyn ColdBacking<Data>>,
}

impl<Data> ColdCompactor<Data>
where
    Data: Clone + PartialEq + Send + Sync + 'static,
{
    pub fn new(backing: Arc<dyn ColdBacking<Data>>) -> Self {
        Self { backing }
    }

    pub fn prepare(
        heap: &ContextHeap<Data>,
        scope: crate::context::ScopeId,
    ) -> ColdCompactionBatch<Data> {
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
            let processing = &entry.item.processing;
            if entry.protected
                || !(processing.cold_summary.completed || processing.cold_summary.exhausted)
                || (entry.is_raw()
                    && !(processing.cold_refinement.completed
                        || processing.cold_refinement.exhausted
                        || processing.cold_refinement.last_attempt_turn.is_some()))
            {
                continue;
            }
            records.push(BackingRecord {
                item: entry.item.clone(),
                scope: entry.scope,
                tokens: entry.tokens,
            });
            remaining = remaining.saturating_sub(entry.tokens);
        }
        ColdCompactionBatch { scope, records }
    }

    pub async fn offload(
        &self,
        batch: ColdCompactionBatch<Data>,
    ) -> Result<VerifiedColdCompactionBatch<Data>, ColdCompactorError> {
        let backing = Arc::clone(&self.backing);
        tokio::task::spawn_blocking(move || {
            for record in batch.records() {
                backing
                    .store(&record.item)
                    .map_err(ColdCompactorError::Backing)?;
            }
            batch
                .verify(backing.as_ref())
                .map_err(ColdCompactorError::Backing)
        })
        .await
        .map_err(|error| ColdCompactorError::Worker(Arc::new(error)))?
    }

    pub fn commit<SummaryData>(
        heap: &mut ContextHeap<Data>,
        catalog: &mut ColdCatalog<SummaryData>,
        verified: VerifiedColdCompactionBatch<Data>,
    ) -> Result<Vec<ContextId>, ColdCompactorError> {
        let batch = verified.into_batch();
        for record in batch.records() {
            if record.scope != batch.scope {
                return Err(ColdCompactorError::Invariant("Cold batch mixed scopes"));
            }
            let current = heap
                .zone(ZoneKind::Cold)
                .get(record.item.id)
                .ok_or(ColdCompactorError::Invariant("Cold candidate moved"))?;
            if current.protected
                || current.item != record.item
                || current.scope != record.scope
                || current.tokens != record.tokens
            {
                return Err(ColdCompactorError::Invariant("Cold candidate changed"));
            }
            let catalog_entry =
                catalog
                    .get(record.item.id)
                    .ok_or(ColdCompactorError::Invariant(
                        "Cold candidate missing from catalog",
                    ))?;
            if catalog_entry.location != CatalogLocation::ColdZone
                || catalog_entry.scope != record.scope
                || catalog_entry.revision != record.item.revision
            {
                return Err(ColdCompactorError::Invariant("Cold catalog entry changed"));
            }
        }
        let mut stored = Vec::new();
        for record in batch.records {
            heap.zone_mut(ZoneKind::Cold)
                .remove(record.item.id)
                .ok_or(ColdCompactorError::Invariant("Cold removal failed"))?;
            catalog.update_processing(record.item.id, record.item.processing.clone());
            catalog.record_backing(record.item.id);
            stored.push(record.item.id);
        }
        Ok(stored)
    }
}
