use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cold::{ColdCompactor, ColdSummaryError, ColdSummaryManager};
use crate::collection::{CollectionError, CollectionManager, Job};
use crate::compaction::refinement::Refinement;
use crate::compaction::{RefinementCommit, RefinementManager};
use crate::context::{ContextId, ProcessingFailure, ScopeId};
use crate::heap::ZoneKind;

use super::{MaintenanceResult, RuntimeError, RuntimeState};

pub(super) struct MaintenanceRunner<Data, SummaryData> {
    state: Arc<Mutex<RuntimeState<Data, SummaryData>>>,
    collection: CollectionManager,
    refinement: RefinementManager<Data>,
    cold_summary: ColdSummaryManager<Data, SummaryData>,
    cold_compactor: ColdCompactor<Data>,
    gate: Mutex<()>,
    errors: Mutex<Vec<RuntimeError>>,
}

impl<Data, SummaryData> MaintenanceRunner<Data, SummaryData>
where
    Data: Clone + PartialEq + Send + Sync + 'static,
    SummaryData: Clone + Send + Sync + 'static,
{
    pub fn new(
        state: Arc<Mutex<RuntimeState<Data, SummaryData>>>,
        collection: CollectionManager,
        refinement: RefinementManager<Data>,
        cold_summary: ColdSummaryManager<Data, SummaryData>,
        cold_compactor: ColdCompactor<Data>,
    ) -> Self {
        Self {
            state,
            collection,
            refinement,
            cold_summary,
            cold_compactor,
            gate: Mutex::new(()),
            errors: Mutex::new(Vec::new()),
        }
    }

    pub async fn errors(&self) -> Vec<RuntimeError> {
        self.errors.lock().await.clone()
    }

    pub async fn drain(&self) -> Vec<MaintenanceResult> {
        let _gate = self.gate.lock().await;
        let mut completed = Vec::new();
        loop {
            let job = self.state.lock().await.scheduler.pop();
            let Some(job) = job else { break };
            match self.run(job.clone()).await {
                Ok(affected) => completed.push(MaintenanceResult { job, affected }),
                Err(error) => {
                    tracing::error!(%error, ?job, "maintenance job failed");
                    self.errors.lock().await.push(error);
                }
            }
        }
        completed
    }

    async fn run(&self, job: Job) -> Result<Vec<ContextId>, RuntimeError> {
        tracing::debug!(?job, "maintenance job started");
        let affected = match job {
            Job::Minor(source) => {
                self.run_collection(|state| {
                    self.collection.minor(&mut state.heap, source, state.turn)
                })
                .await?
            }
            Job::Cooling(source) => {
                self.run_collection(|state| {
                    self.collection
                        .cooling(&mut state.heap, &state.scopes, source, state.turn)
                })
                .await?
            }
            Job::Major => {
                self.run_collection(|state| {
                    let moved = self.collection.major(&mut state.heap)?;
                    for id in &moved {
                        let entry = state
                            .heap
                            .zone(ZoneKind::Cold)
                            .get(*id)
                            .expect("moved Cold entry exists");
                        state.catalog.record_cold(entry);
                    }
                    Ok(moved)
                })
                .await?
            }
            Job::HotRefinement(zone, scope) => self.run_refinement(zone, scope).await?,
            Job::ColdRefinement(scope) => self.run_refinement(ZoneKind::Cold, scope).await?,
            Job::ColdSummary(scope) => self.run_cold_summary(scope).await?,
            Job::ColdCompaction(scope) => self.run_cold_compaction(scope).await?,
        };
        tracing::debug!(?job, affected = affected.len(), "maintenance job completed");
        Ok(affected)
    }

    async fn run_refinement(
        &self,
        zone: ZoneKind,
        scope: ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let prepared = {
            let state = self.state.lock().await;
            self.refinement
                .prepare(&state.heap, zone, scope, state.turn, state.config.hot_high)
        };
        let Some(prepared) = prepared else {
            return Ok(Vec::new());
        };
        let result = match self.refinement.refine(&prepared).await {
            Ok(result) => result,
            Err(error) => {
                let mut state = self.state.lock().await;
                Self::record_refinement_failure(&mut state, &prepared, ProcessingFailure::External);
                state.schedule();
                return Err(error.into());
            }
        };
        let settled: BTreeSet<_> = result.settled.iter().copied().collect();
        let mut state = self.state.lock().await;
        let commit = {
            let RuntimeState {
                heap,
                scopes,
                catalog,
                next_id,
                turn,
                ..
            } = &mut *state;
            self.refinement.commit(
                RefinementCommit {
                    heap,
                    scopes,
                    catalog,
                    next_id,
                    turn: *turn,
                },
                prepared.clone(),
                result,
            )
        };
        let made = match commit {
            Ok(made) => made,
            Err(error) => {
                Self::record_refinement_failure(
                    &mut state,
                    &prepared,
                    ProcessingFailure::InvalidResult,
                );
                state.schedule();
                return Err(error.into());
            }
        };
        let turn = state.turn;
        let RuntimeState { heap, catalog, .. } = &mut *state;
        for raw in prepared.inputs() {
            let Some(entry) = heap.zone_mut(zone).get_mut(raw.id) else {
                continue;
            };
            let attempt = if zone == ZoneKind::Cold {
                &mut entry.item.processing.cold_refinement
            } else {
                &mut entry.item.processing.hot_refinement
            };
            if settled.contains(&raw.id) {
                attempt.succeed(turn);
            } else {
                attempt.defer(turn);
            }
            if zone == ZoneKind::Cold {
                catalog.update_processing(raw.id, entry.item.processing.clone());
            }
        }
        state.schedule();
        Ok(made)
    }

    fn record_refinement_failure(
        state: &mut RuntimeState<Data, SummaryData>,
        prepared: &Refinement,
        reason: ProcessingFailure,
    ) {
        let zone = prepared.zone();
        let turn = state.turn;
        let max_failures = state.config.max_processing_failures;
        let RuntimeState { heap, catalog, .. } = state;
        for raw in prepared.inputs() {
            let Some(entry) = heap.zone_mut(zone).get_mut(raw.id) else {
                continue;
            };
            if entry.protected
                || entry.scope != prepared.scope()
                || entry.item.revision != raw.revision
                || !matches!(&entry.item.kind, crate::context::InfoKind::Raw(current) if current.content == raw.content)
            {
                continue;
            }
            let attempt = if zone == ZoneKind::Cold {
                &mut entry.item.processing.cold_refinement
            } else {
                &mut entry.item.processing.hot_refinement
            };
            attempt.fail(turn, reason, max_failures);
            if zone == ZoneKind::Cold {
                catalog.update_processing(raw.id, entry.item.processing.clone());
            }
        }
    }

    async fn run_cold_summary(&self, scope: ScopeId) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = {
            let state = self.state.lock().await;
            self.cold_summary.prepare(&state.heap, scope, state.turn)
        };
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let proposal = match self.cold_summary.summarize(&batch).await {
            Ok(proposal) => proposal,
            Err(error) => {
                let mut state = self.state.lock().await;
                let turn = state.turn;
                let limit = state.config.max_processing_failures;
                let RuntimeState { heap, catalog, .. } = &mut *state;
                ColdSummaryManager::<Data, SummaryData>::record_failure(
                    heap,
                    catalog,
                    &batch,
                    turn,
                    ProcessingFailure::External,
                    limit,
                );
                state.schedule();
                return Err(error.into());
            }
        };
        let Some(proposal) = proposal else {
            let mut state = self.state.lock().await;
            let turn = state.turn;
            let limit = state.config.max_processing_failures;
            let RuntimeState { heap, catalog, .. } = &mut *state;
            ColdSummaryManager::<Data, SummaryData>::record_failure(
                heap,
                catalog,
                &batch,
                turn,
                ProcessingFailure::NoSummaryReturned,
                limit,
            );
            state.schedule();
            return Err(ColdSummaryError::NoSummaryReturned.into());
        };
        let mut state = self.state.lock().await;
        let turn = state.turn;
        let result = {
            let RuntimeState { heap, catalog, .. } = &mut *state;
            ColdSummaryManager::<Data, SummaryData>::commit(
                heap,
                catalog,
                batch.clone(),
                proposal,
                turn,
            )
        };
        let covered = match result {
            Ok(covered) => covered,
            Err(error) => {
                let limit = state.config.max_processing_failures;
                let RuntimeState { heap, catalog, .. } = &mut *state;
                ColdSummaryManager::<Data, SummaryData>::record_failure(
                    heap,
                    catalog,
                    &batch,
                    turn,
                    ProcessingFailure::InvalidResult,
                    limit,
                );
                state.schedule();
                return Err(error.into());
            }
        };
        state.schedule();
        Ok(covered)
    }

    async fn run_cold_compaction(&self, scope: ScopeId) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = {
            let state = self.state.lock().await;
            ColdCompactor::<Data>::prepare(&state.heap, scope)
        };
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        let verified = self.cold_compactor.offload(batch).await?;
        let mut state = self.state.lock().await;
        let RuntimeState { heap, catalog, .. } = &mut *state;
        let moved = ColdCompactor::<Data>::commit(heap, catalog, verified)?;
        if !moved.is_empty() {
            state.schedule();
        }
        Ok(moved)
    }

    async fn run_collection(
        &self,
        operation: impl FnOnce(
            &mut RuntimeState<Data, SummaryData>,
        ) -> Result<Vec<ContextId>, CollectionError>,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let mut state = self.state.lock().await;
        let affected = operation(&mut state)?;
        if !affected.is_empty() {
            state.schedule();
        }
        Ok(affected)
    }
}
