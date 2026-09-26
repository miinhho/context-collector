use std::sync::Arc;

use tokio::sync::Mutex;

use crate::collection::{CollectionManager, Job};
use crate::context::ContextId;
use crate::heap::ZoneKind;

use super::cold_compactor::ColdCompactor;
use super::objectization::ObjectizationWorker;
use super::{MaintenanceResult, RuntimeError, RuntimeState};

pub(super) struct MaintenanceRunner {
    state: Arc<Mutex<RuntimeState>>,
    collection: CollectionManager,
    objectization: ObjectizationWorker,
    cold_compactor: ColdCompactor,
    gate: Mutex<()>,
    errors: Mutex<Vec<RuntimeError>>,
}

impl MaintenanceRunner {
    pub fn new(
        state: Arc<Mutex<RuntimeState>>,
        collection: CollectionManager,
        objectization: ObjectizationWorker,
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
            Job::HotObjectization(zone, scope) => {
                self.objectization.execute(&self.state, zone, scope).await?
            }
            Job::ColdObjectization(scope) => {
                self.objectization
                    .execute(&self.state, ZoneKind::Cold, scope)
                    .await?
            }
            Job::ColdCompaction(scope) => self.cold_compactor.execute(&self.state, scope).await?,
        };
        tracing::debug!(?job, affected = affected.len(), "maintenance job completed");
        Ok(affected)
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
