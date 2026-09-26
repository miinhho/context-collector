use self::maintenance::MaintenanceRunner;
use self::turn::TurnRecorder;
use crate::cold::{
    CatalogLocation, ColdBacking, ColdCatalogEntry, ColdCompactor, ColdCompactorError,
    ColdSummaryError, ColdSummaryManager,
};
use crate::collection::{CollectionError, CollectionManager};
use crate::compaction::refinement::InfoRefiner;
use crate::compaction::scope_summary::ScopeSummarizer;
use crate::compaction::{RefinementError, RefinementManager};
use crate::context::{ContextItem, InfoKind};
use crate::error::ExternalError;
use crate::heap::{TokenUsage, ZoneKind};
use crate::token::{TiktokenCounter, TokenCounter};
use crate::view::{ContextView, ViewBuilder, ViewError, ViewUsage, lookup};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::cold::ColdCatalog;
use crate::collection::{CollectionScheduler, Job};
use crate::context::{ContextId, ScopeId};
use crate::heap::{ContextHeap, Watermark};
use crate::scope::Scopes;

mod maintenance;
mod turn;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeReport {
    Continue,
    Transition,
    Uncertain,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnObservation {
    pub uses: Option<Vec<ContextId>>,
    pub scope: Option<ScopeReport>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub watermarks: [Watermark; 5],
    pub hot_high: usize,
    pub processing_batch_tokens: usize,
    pub max_processing_failures: u32,
}

impl RuntimeConfig {
    pub fn valid(self) -> bool {
        self.hot_high > 0
            && self.processing_batch_tokens > 0
            && self.max_processing_failures > 0
            && self.watermarks.iter().all(|mark| mark.valid())
    }
}

#[derive(Clone, Debug, Error)]
pub enum RuntimeError {
    #[error("invalid runtime configuration")]
    InvalidConfig,
    #[error("unknown context {0:?}")]
    UnknownContext(ContextId),
    #[error("unknown scope {0:?}")]
    UnknownScope(ScopeId),
    #[error(transparent)]
    Refinement(#[from] RefinementError),
    #[error(transparent)]
    ColdCompaction(#[from] ColdCompactorError),
    #[error(transparent)]
    ColdSummary(#[from] ColdSummaryError),
    #[error(transparent)]
    View(#[from] ViewError),
    #[error(transparent)]
    Collection(#[from] CollectionError),
    #[error("Cold backing failed")]
    ColdBacking(#[source] ExternalError),
    #[error("maintenance worker failed")]
    Worker(#[source] ExternalError),
    #[error("runtime invariant failed: {0}")]
    Invariant(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnReceipt {
    pub turn: u64,
    pub scope: ScopeId,
    pub user: ContextId,
    pub agent: ContextId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceResult {
    pub job: Job,
    pub affected: Vec<ContextId>,
}

// Canonical mutable state. Work policy and external services live in composed workers.
struct RuntimeState<Data, SummaryData> {
    config: RuntimeConfig,
    heap: ContextHeap<Data>,
    scopes: Scopes,
    catalog: ColdCatalog<SummaryData>,
    scheduler: CollectionScheduler,
    turn: u64,
    next_id: u64,
}

impl<Data, SummaryData> RuntimeState<Data, SummaryData> {
    fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        if !config.valid() {
            return Err(RuntimeError::InvalidConfig);
        }
        Ok(Self {
            config,
            heap: ContextHeap::new(config.watermarks).ok_or(RuntimeError::InvalidConfig)?,
            scopes: Scopes::default(),
            catalog: ColdCatalog::default(),
            scheduler: CollectionScheduler::default(),
            turn: 0,
            next_id: 1,
        })
    }

    fn schedule(&mut self) {
        self.scheduler.schedule(&self.heap, self.config.hot_high);
    }

    fn next_context_id(&mut self) -> ContextId {
        let id = ContextId(self.next_id);
        self.next_id += 1;
        id
    }
}

#[derive(Clone)]
pub struct Runtime<Data = (), SummaryData = ()> {
    state: Arc<Mutex<RuntimeState<Data, SummaryData>>>,
    recorder: Arc<TurnRecorder>,
    view: Arc<ViewBuilder<Data>>,
    maintenance: Arc<MaintenanceRunner<Data, SummaryData>>,
    backing: Arc<dyn ColdBacking<Data>>,
}

impl<Data, SummaryData> Runtime<Data, SummaryData>
where
    Data: Clone + PartialEq + Send + Sync + 'static,
    SummaryData: Clone + Send + Sync + 'static,
{
    pub fn new(
        config: RuntimeConfig,
        refiner: Arc<dyn InfoRefiner<Data>>,
        summarizer: Arc<dyn ScopeSummarizer<Data, SummaryData>>,
        backing: Arc<dyn ColdBacking<Data>>,
    ) -> Result<Self, RuntimeError> {
        Self::with_counter(
            config,
            Arc::new(TiktokenCounter),
            refiner,
            summarizer,
            backing,
        )
    }

    pub fn with_counter(
        config: RuntimeConfig,
        counter: Arc<dyn TokenCounter>,
        refiner: Arc<dyn InfoRefiner<Data>>,
        summarizer: Arc<dyn ScopeSummarizer<Data, SummaryData>>,
        backing: Arc<dyn ColdBacking<Data>>,
    ) -> Result<Self, RuntimeError> {
        let state = Arc::new(Mutex::new(RuntimeState::new(config)?));
        let recorder = Arc::new(TurnRecorder::new(Arc::clone(&counter)));
        let view = Arc::new(ViewBuilder::new(Arc::clone(&backing), Arc::clone(&counter)));
        let maintenance = Arc::new(MaintenanceRunner::new(
            Arc::clone(&state),
            CollectionManager::new(config.hot_high),
            RefinementManager::new(
                refiner,
                Arc::clone(&counter),
                config.processing_batch_tokens,
            ),
            ColdSummaryManager::new(summarizer, config.processing_batch_tokens),
            ColdCompactor::new(Arc::clone(&backing)),
        ));
        Ok(Self {
            state,
            recorder,
            view,
            maintenance,
            backing,
        })
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

    pub async fn read(&self, id: ContextId) -> Result<Option<ContextItem<Data>>, RuntimeError> {
        let (expected_revision, expected_processing) = {
            let state = self.state.lock().await;
            if let Some((_, entry)) = state.heap.find(id) {
                return Ok(Some(entry.item.clone()));
            }
            match state.catalog.get(id) {
                None => return Ok(None),
                Some(entry) if entry.location == CatalogLocation::ColdZone => {
                    return Err(RuntimeError::Invariant(
                        "ColdCatalog points to missing Cold entry",
                    ));
                }
                Some(entry) => (entry.revision, entry.processing.clone()),
            }
        };
        let backing = Arc::clone(&self.backing);
        tokio::task::spawn_blocking(move || {
            let item = backing
                .load(id)
                .map_err(RuntimeError::ColdBacking)?
                .ok_or(RuntimeError::Invariant("backing lost cataloged info"))?;
            if item.id != id
                || item.revision != expected_revision
                || item.processing != expected_processing
            {
                return Err(RuntimeError::Invariant(
                    "backing returned wrong identity, revision, or processing state",
                ));
            }
            Ok(Some(item))
        })
        .await
        .map_err(|error| RuntimeError::Worker(Arc::new(error)))?
    }

    pub async fn context_view(
        &self,
        explicit_cold: &[ContextId],
    ) -> Result<ContextView, RuntimeError> {
        let plan = {
            let state = self.state.lock().await;
            self.view.prepare(
                &state.heap,
                &state.scopes,
                &state.catalog,
                state.turn,
                explicit_cold,
            )?
        };
        Ok(self.view.build(plan).await?)
    }

    pub async fn view_usage(&self) -> Result<ViewUsage, RuntimeError> {
        let space = {
            let state = self.state.lock().await;
            ViewBuilder::<Data>::project(&state.heap, &state.scopes, &state.catalog, state.turn)
        };
        Ok(self.view.usage(space).await?)
    }

    /// Recently reported uses across resident and backed information.
    /// The returned Markdown contains no Zone or storage labels.
    pub async fn recent_context(
        &self,
        scope: Option<ScopeId>,
        limit: usize,
    ) -> Result<String, RuntimeError> {
        let recent = {
            let state = self.state.lock().await;
            lookup::recent_ids(&state.heap, &state.catalog, scope, limit)
        };
        let mut items = Vec::new();
        for (id, scope) in recent {
            let item = self
                .read(id)
                .await?
                .ok_or(RuntimeError::Invariant("reported context is missing"))?;
            items.push((scope, item));
        }
        Ok(lookup::recent_markdown(items))
    }

    /// Scope navigation using existing summaries and exact context references.
    pub async fn scope_context_markdown(&self, scope: ScopeId) -> Result<String, RuntimeError> {
        let plan = {
            let state = self.state.lock().await;
            lookup::scope_plan(&state.scopes, &state.catalog, scope)
                .ok_or(RuntimeError::UnknownScope(scope))?
        };
        Ok(lookup::scope_markdown(plan))
    }

    pub async fn open_context_markdown(
        &self,
        id: ContextId,
    ) -> Result<Option<String>, RuntimeError> {
        let Some(item) = self.read(id).await? else {
            return Ok(None);
        };
        let scope = self
            .state
            .lock()
            .await
            .scopes
            .owner_of(id)
            .ok_or(RuntimeError::Invariant("context has no Scope owner"))?;
        Ok(Some(lookup::item_markdown(scope, &item)))
    }

    pub async fn evidence_markdown(
        &self,
        info_id: ContextId,
    ) -> Result<Option<String>, RuntimeError> {
        let Some(item) = self.read(info_id).await? else {
            return Ok(None);
        };
        let InfoKind::Info(info) = item.kind else {
            return Ok(None);
        };
        let mut excerpts = Vec::new();
        for source in info.sources {
            let raw = self
                .read(source.raw)
                .await?
                .ok_or(RuntimeError::Invariant("Info source is missing"))?;
            let InfoKind::Raw(raw_info) = raw.kind else {
                return Err(RuntimeError::Invariant("Info source is not Raw"));
            };
            if raw.revision != source.revision {
                return Err(RuntimeError::Invariant("Info source revision changed"));
            }
            let excerpt = raw_info
                .content
                .get(source.start..source.end)
                .ok_or(RuntimeError::Invariant("Info source span is invalid"))?;
            excerpts.push((source.raw, source.start, source.end, excerpt.to_owned()));
        }
        Ok(Some(lookup::evidence_markdown(&excerpts)))
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
    pub async fn cold_scope_summaries(
        &self,
        scope: ScopeId,
    ) -> Vec<crate::cold::ScopeSummary<SummaryData>> {
        self.state.lock().await.catalog.summaries(scope).to_vec()
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
