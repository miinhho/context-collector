use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cold::{CatalogLocation, ColdBacking, ColdCatalogEntry};
use crate::collection::CollectionManager;
use crate::compaction::objectization::Objectizer;
use crate::compaction::scope_summary::ScopeSummarizer;
use crate::context::{ContextId, ContextObject, ScopeId};
use crate::heap::{TokenUsage, ZoneKind};
use crate::llm::{LlmClient, LlmObjectizer, LlmScopeSummarizer, LlmTaskConfig};
use crate::token::{TiktokenCounter, TokenCounter};
use crate::view::{ContextView, TokenSpace};

use super::cold_compactor::ColdCompactor;
use super::maintenance::MaintenanceRunner;
use super::objectization::ObjectizationWorker;
use super::turn::TurnRecorder;
use super::view::ViewBuilder;
use super::{
    MaintenanceResult, RuntimeConfig, RuntimeError, RuntimeState, TurnObservation, TurnReceipt,
};

#[derive(Clone)]
pub struct Runtime {
    state: Arc<Mutex<RuntimeState>>,
    recorder: Arc<TurnRecorder>,
    view: Arc<ViewBuilder>,
    maintenance: Arc<MaintenanceRunner>,
    backing: Arc<dyn ColdBacking>,
}

impl Runtime {
    pub fn new(
        config: RuntimeConfig,
        objectizer: Arc<dyn Objectizer>,
        summarizer: Arc<dyn ScopeSummarizer>,
        backing: Arc<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        Self::with_counter(
            config,
            Arc::new(TiktokenCounter),
            objectizer,
            summarizer,
            backing,
        )
    }

    pub fn with_counter(
        config: RuntimeConfig,
        counter: Arc<dyn TokenCounter>,
        objectizer: Arc<dyn Objectizer>,
        summarizer: Arc<dyn ScopeSummarizer>,
        backing: Arc<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        let state = Arc::new(Mutex::new(RuntimeState::new(config)?));
        let recorder = Arc::new(TurnRecorder::new(Arc::clone(&counter)));
        let view = Arc::new(ViewBuilder::new(Arc::clone(&counter), Arc::clone(&backing)));
        let maintenance = Arc::new(MaintenanceRunner::new(
            Arc::clone(&state),
            CollectionManager::new(config.hot_high),
            ObjectizationWorker::new(objectizer, counter),
            ColdCompactor::new(summarizer, Arc::clone(&backing)),
        ));
        Ok(Self {
            state,
            recorder,
            view,
            maintenance,
            backing,
        })
    }

    pub fn with_llm(
        config: RuntimeConfig,
        client: Arc<dyn LlmClient>,
        objectization: LlmTaskConfig,
        scope_summary: LlmTaskConfig,
        backing: Arc<dyn ColdBacking>,
    ) -> Result<Self, RuntimeError> {
        let objectizer = LlmObjectizer::new(Arc::clone(&client), objectization)
            .map_err(RuntimeError::InvalidConfigDetail)?;
        let summarizer = LlmScopeSummarizer::new(client, scope_summary)
            .map_err(RuntimeError::InvalidConfigDetail)?;
        Self::new(config, Arc::new(objectizer), Arc::new(summarizer), backing)
    }

    pub async fn complete_turn(
        &self,
        user: String,
        agent: String,
        report: TurnObservation,
    ) -> Result<TurnReceipt, RuntimeError> {
        let receipt = {
            let mut state = self.state.lock().await;
            self.recorder.record(&mut state, user, agent, report)?
        };
        let runner = Arc::clone(&self.maintenance);
        tokio::spawn(async move { runner.drain().await });
        Ok(receipt)
    }

    pub async fn read(&self, id: ContextId) -> Result<Option<ContextObject>, RuntimeError> {
        let expected_revision = {
            let state = self.state.lock().await;
            if let Some((_, entry)) = state.heap.find(id) {
                return Ok(Some(entry.object.clone()));
            }
            match state.catalog.get(id) {
                None => return Ok(None),
                Some(entry) if entry.location == CatalogLocation::ColdZone => {
                    return Err(RuntimeError::Invariant(
                        "ColdCatalog points to missing Cold entry",
                    ));
                }
                Some(entry) => entry.revision,
            }
        };
        let backing = Arc::clone(&self.backing);
        tokio::task::spawn_blocking(move || {
            let object = backing
                .load(id)
                .map_err(RuntimeError::ColdBacking)?
                .ok_or(RuntimeError::Invariant("backing lost cataloged object"))?;
            if object.id != id || object.revision != expected_revision {
                return Err(RuntimeError::Invariant(
                    "backing returned wrong identity or revision",
                ));
            }
            Ok(Some(object))
        })
        .await
        .map_err(|error| RuntimeError::Worker(error.to_string()))?
    }

    pub async fn context_view(
        &self,
        budget: TokenSpace,
        explicit_cold: &[ContextId],
    ) -> Result<ContextView, RuntimeError> {
        let plan = {
            let state = self.state.lock().await;
            self.view.prepare(&state, budget, explicit_cold)?
        };
        self.view.build(plan).await
    }

    pub async fn select_scope(&self, id: ScopeId) -> Result<(), RuntimeError> {
        let mut state = self.state.lock().await;
        if state.scopes.select_existing(id) {
            Ok(())
        } else {
            Err(RuntimeError::UnknownScope(id))
        }
    }

    pub async fn current_scope(&self) -> ScopeId {
        self.state.lock().await.scopes.current()
    }
    pub async fn turn(&self) -> u64 {
        self.state.lock().await.turn
    }
    pub async fn pending_jobs(&self) -> usize {
        self.state.lock().await.scheduler.pending()
    }
    pub async fn cold_scope_entries(&self, scope: ScopeId) -> Vec<ColdCatalogEntry> {
        self.state
            .lock()
            .await
            .catalog
            .entries_for_scope(scope)
            .cloned()
            .collect()
    }
    pub async fn protect(&self, id: ContextId, protected: bool) -> Result<(), RuntimeError> {
        let mut state = self.state.lock().await;
        let zone = state
            .heap
            .find(id)
            .map(|(zone, _)| zone)
            .ok_or(RuntimeError::UnknownContext(id))?;
        state
            .heap
            .zone_mut(zone)
            .get_mut(id)
            .expect("found entry")
            .protected = protected;
        Ok(())
    }
    pub async fn zone_of(&self, id: ContextId) -> Option<ZoneKind> {
        self.state.lock().await.heap.find(id).map(|(zone, _)| zone)
    }
    pub async fn zone_usage(&self, zone: ZoneKind) -> TokenUsage {
        self.state.lock().await.heap.zone(zone).usage()
    }
    pub async fn maintenance_errors(&self) -> Vec<RuntimeError> {
        self.maintenance.errors().await
    }
    pub async fn drain_maintenance(&self) -> Vec<MaintenanceResult> {
        self.maintenance.drain().await
    }
}
