use std::sync::Arc;

use tokio::sync::Mutex;

use crate::collection::{CollectionManager, Job};
use crate::context::ContextId;
use crate::heap::ZoneKind;

use super::{MaintenanceResult, RuntimeError, RuntimeState};
use crate::cold::ColdCompactor;
use crate::compaction::{ObjectizationCommit, ObjectizationManager};

pub(super) struct MaintenanceRunner {
    state: Arc<Mutex<RuntimeState>>,
    collection: CollectionManager,
    objectization: ObjectizationManager,
    cold_compactor: ColdCompactor,
    gate: Mutex<()>,
    errors: Mutex<Vec<RuntimeError>>,
}

impl MaintenanceRunner {
    pub fn new(
        state: Arc<Mutex<RuntimeState>>,
        collection: CollectionManager,
        objectization: ObjectizationManager,
        cold_compactor: ColdCompactor,
    ) -> Self {
        Self {
            state,
            collection,
            objectization,
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
            Job::HotObjectization(zone, scope) => self.run_objectization(zone, scope).await?,
            Job::ColdObjectization(scope) => self.run_objectization(ZoneKind::Cold, scope).await?,
            Job::ColdCompaction(scope) => self.run_cold_compaction(scope).await?,
        };
        tracing::debug!(?job, affected = affected.len(), "maintenance job completed");
        Ok(affected)
    }

    async fn run_objectization(
        &self,
        zone: ZoneKind,
        scope: crate::context::ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let prepared = {
            let state = self.state.lock().await;
            self.objectization
                .prepare(&state.heap, zone, scope, state.turn, state.config.hot_high)
        };
        let Some(prepared) = prepared else {
            return Ok(Vec::new());
        };
        let proposals = self.objectization.extract(&prepared).await?;
        let mut state = self.state.lock().await;
        let RuntimeState {
            heap,
            scopes,
            catalog,
            next_id,
            turn,
            ..
        } = &mut *state;
        let made = self.objectization.commit(
            ObjectizationCommit {
                heap,
                scopes,
                catalog,
                next_id,
                turn: *turn,
            },
            prepared,
            proposals,
        )?;
        if !made.is_empty() {
            state.schedule();
        }
        Ok(made)
    }

    async fn run_cold_compaction(
        &self,
        scope: crate::context::ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let batch = {
            let state = self.state.lock().await;
            ColdCompactor::prepare(&state.heap, scope)
        };
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        let (verified, proposal) = self.cold_compactor.offload(batch).await?;
        let mut state = self.state.lock().await;
        let RuntimeState { heap, catalog, .. } = &mut *state;
        let moved = ColdCompactor::commit(heap, catalog, verified, proposal)?;
        if !moved.is_empty() {
            state.schedule();
        }
        Ok(moved)
    }

    async fn run_collection(
        &self,
        operation: impl FnOnce(&mut RuntimeState) -> Result<Vec<ContextId>, &'static str>,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let mut state = self.state.lock().await;
        let affected = operation(&mut state).map_err(RuntimeError::Invariant)?;
        if !affected.is_empty() {
            state.schedule();
        }
        Ok(affected)
    }
}
