use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cold::{ColdBacking, ColdCatalogEntry};
use crate::collection::Job;
use crate::context::{ContextId, ContextObject, ScopeId};
use crate::heap::{TokenUsage, ZoneKind};
use crate::objectization::Objectizer;
use crate::runtime::{
    MaintenanceResult, RuntimeConfig, RuntimeError, RuntimeState, TurnObservation, TurnReceipt,
};
use crate::scope_summary::{NoopScopeSummarizer, ScopeSummarizer, ScopeSummaryInput};
use crate::token::{TiktokenCounter, TokenCounter};
use crate::view::{ContextView, TokenSpace};

#[derive(Clone)]
pub struct Runtime {
    runtime: Arc<Mutex<RuntimeState>>,
    worker_gate: Arc<Mutex<()>>,
    errors: Arc<Mutex<Vec<RuntimeError>>>,
}

impl Runtime {
    pub fn new(
        config: RuntimeConfig,
        objectizer: Box<dyn Objectizer>,
        backing: Box<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        Self::with_counter(config, Box::new(TiktokenCounter), objectizer, backing)
    }

    pub fn with_counter(
        config: RuntimeConfig,
        counter: Box<dyn TokenCounter>,
        objectizer: Box<dyn Objectizer>,
        backing: Box<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        Self::with_counter_and_summarizer(
            config,
            counter,
            objectizer,
            Box::new(NoopScopeSummarizer),
            backing,
        )
    }

    pub fn with_summarizer(
        config: RuntimeConfig,
        objectizer: Box<dyn Objectizer>,
        summarizer: Box<dyn ScopeSummarizer>,
        backing: Box<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        Self::with_counter_and_summarizer(
            config,
            Box::new(TiktokenCounter),
            objectizer,
            summarizer,
            backing,
        )
    }

    pub fn with_counter_and_summarizer(
        config: RuntimeConfig,
        counter: Box<dyn TokenCounter>,
        objectizer: Box<dyn Objectizer>,
        summarizer: Box<dyn ScopeSummarizer>,
        backing: Box<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        Ok(Self::from_state(RuntimeState::with_summarizer(
            config, counter, objectizer, summarizer, backing,
        )?))
    }

    pub(crate) fn from_state(runtime: RuntimeState) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
            worker_gate: Arc::new(Mutex::new(())),
            errors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn complete_turn(
        &self,
        user: String,
        agent: String,
        report: TurnObservation,
    ) -> Result<TurnReceipt, RuntimeError> {
        let receipt = self
            .runtime
            .lock()
            .await
            .complete_turn(user, agent, report)?;
        let worker = self.clone();
        tokio::spawn(async move { worker.drain_maintenance().await });
        Ok(receipt)
    }

    pub async fn read(&self, id: ContextId) -> Result<Option<ContextObject>, RuntimeError> {
        self.runtime.lock().await.read(id)
    }

    pub async fn context_view(
        &self,
        budget: TokenSpace,
        explicit_cold: &[ContextId],
    ) -> Result<ContextView, RuntimeError> {
        self.runtime
            .lock()
            .await
            .context_view(budget, explicit_cold)
    }

    pub async fn select_scope(&self, id: ScopeId) -> Result<(), RuntimeError> {
        self.runtime.lock().await.select_scope(id)
    }

    pub async fn current_scope(&self) -> ScopeId {
        self.runtime.lock().await.scopes().current()
    }

    pub async fn turn(&self) -> u64 {
        self.runtime.lock().await.turn()
    }

    pub async fn pending_jobs(&self) -> usize {
        self.runtime.lock().await.pending_jobs()
    }

    pub async fn cold_scope_entries(&self, scope: ScopeId) -> Vec<ColdCatalogEntry> {
        self.runtime.lock().await.cold_scope_entries(scope)
    }

    pub async fn protect(&self, id: ContextId, protected: bool) -> Result<(), RuntimeError> {
        self.runtime.lock().await.protect(id, protected)
    }

    pub async fn zone_of(&self, id: ContextId) -> Option<ZoneKind> {
        self.runtime
            .lock()
            .await
            .heap()
            .find(id)
            .map(|(zone, _)| zone)
    }

    pub async fn zone_usage(&self, zone: ZoneKind) -> TokenUsage {
        self.runtime.lock().await.heap().zone(zone).usage()
    }

    pub async fn maintenance_errors(&self) -> Vec<RuntimeError> {
        self.errors.lock().await.clone()
    }

    pub async fn drain_maintenance(&self) -> Vec<MaintenanceResult> {
        let _gate = self.worker_gate.lock().await;
        let mut completed = Vec::new();
        loop {
            let job = self.runtime.lock().await.take_job();
            let Some(job) = job else { break };
            match self.run_job(job).await {
                Ok(result) => completed.push(result),
                Err(error) => {
                    tracing::error!(%error, "maintenance job failed");
                    self.errors.lock().await.push(error);
                }
            }
        }
        completed
    }

    async fn run_job(&self, job: Job) -> Result<MaintenanceResult, RuntimeError> {
        tracing::debug!(?job, "async maintenance job started");
        let affected = match job {
            Job::Minor(_) | Job::Cooling(_) | Job::Major => {
                return self.runtime.lock().await.run_job(job);
            }
            Job::HotObjectization(zone, scope) => self.objectize(zone, scope).await?,
            Job::ColdObjectization(scope) => self.objectize(ZoneKind::Cold, scope).await?,
            Job::ColdCompaction(scope) => self.compact_cold(scope).await?,
        };
        tracing::debug!(
            ?job,
            affected = affected.len(),
            "async maintenance job completed"
        );
        Ok(MaintenanceResult { job, affected })
    }

    async fn objectize(
        &self,
        zone: ZoneKind,
        scope: ScopeId,
    ) -> Result<Vec<ContextId>, RuntimeError> {
        let (prepared, objectizer) = {
            let runtime = self.runtime.lock().await;
            (
                runtime.prepare_objectization(zone, scope),
                runtime.objectizer_handle(),
            )
        };
        let Some(prepared) = prepared else {
            return Ok(Vec::new());
        };
        let inputs = prepared.inputs().to_vec();
        let proposals = tokio::task::spawn_blocking(move || objectizer.extract(&inputs))
            .await
            .map_err(|error| RuntimeError::Worker(error.to_string()))?
            .map_err(RuntimeError::Objectizer)?;
        let mut runtime = self.runtime.lock().await;
        let made = runtime.commit_objectization(prepared, proposals)?;
        if !made.is_empty() {
            runtime.reschedule();
        }
        Ok(made)
    }

    async fn compact_cold(&self, scope: ScopeId) -> Result<Vec<ContextId>, RuntimeError> {
        let (batch, backing, summarizer) = {
            let runtime = self.runtime.lock().await;
            (
                runtime.prepare_cold_compaction(scope),
                runtime.backing_handle(),
                runtime.summarizer_handle(),
            )
        };
        if batch.records().is_empty() {
            return Ok(Vec::new());
        }
        let (verified, proposal) =
            tokio::task::spawn_blocking(move || -> Result<_, RuntimeError> {
                let records = batch.records().to_vec();
                let inputs: Vec<_> = records
                    .iter()
                    .map(|record| ScopeSummaryInput {
                        object: record.object.clone(),
                    })
                    .collect();
                let proposal = summarizer
                    .summarize(scope, &inputs)
                    .map_err(RuntimeError::ScopeSummary)?;
                for record in &records {
                    backing
                        .store(&record.object)
                        .map_err(RuntimeError::ColdBacking)?;
                }
                let verified = batch
                    .verify(backing.as_ref())
                    .map_err(RuntimeError::ColdBacking)?;
                Ok((verified, proposal))
            })
            .await
            .map_err(|error| RuntimeError::Worker(error.to_string()))??;
        self.runtime
            .lock()
            .await
            .commit_cold_compaction(verified, proposal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryColdBacking, NoopObjectizer, RuntimeConfig, Watermark};

    #[tokio::test]
    async fn public_runtime_receives_turn_and_exposes_raw() {
        let runtime = Runtime::new(
            RuntimeConfig {
                watermarks: [Watermark { low: 1, high: 1000 }; 5],
                hot_high: 10000,
            },
            Box::new(NoopObjectizer),
            Box::new(InMemoryColdBacking::default()),
        )
        .unwrap();
        let receipt = runtime
            .complete_turn(
                "user raw".into(),
                "agent raw".into(),
                TurnObservation::default(),
            )
            .await
            .unwrap();
        assert_eq!(runtime.current_scope().await, receipt.scope);
        assert_eq!(runtime.turn().await, 1);
        assert_eq!(
            runtime.zone_usage(ZoneKind::Eden).await.raw,
            TiktokenCounter.count("user raw") + TiktokenCounter.count("agent raw")
        );
        assert_eq!(
            runtime
                .read(receipt.user)
                .await
                .unwrap()
                .unwrap()
                .representation,
            crate::Representation::Raw("user raw".into())
        );
        assert!(runtime.maintenance_errors().await.is_empty());
    }
}
