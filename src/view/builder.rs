use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;

use crate::cold::{CatalogLocation, ColdBacking, ColdCatalog};
use crate::context::{ContextId, ContextItem, ProcessingState, ScopeId};
use crate::error::ExternalError;
use crate::heap::{ContextHeap, ZoneKind};
use crate::scope::Scopes;
use crate::token::TokenCounter;
use crate::view::space::ViewSpace;
use crate::view::{ContextView, ViewUsage, note_for_item};

#[derive(Clone, Debug, Error)]
pub enum ViewError {
    #[error("unknown context {0:?}")]
    UnknownContext(ContextId),
    #[error("view invariant failed: {0}")]
    Invariant(&'static str),
    #[error("Cold backing failed")]
    ColdBacking(#[source] ExternalError),
    #[error("view worker failed")]
    Worker(#[source] ExternalError),
}

struct StoredCandidate<Data> {
    id: ContextId,
    scope: ScopeId,
    revision: u64,
    processing: ProcessingState,
    resident: Option<ContextItem<Data>>,
}

pub(crate) struct ViewPlan<Data> {
    space: ViewSpace,
    recalled: Vec<StoredCandidate<Data>>,
}

pub(crate) struct ViewBuilder<Data> {
    backing: Arc<dyn ColdBacking<Data>>,
    counter: Arc<dyn TokenCounter>,
}

impl<Data> ViewBuilder<Data>
where
    Data: Clone + Send + Sync + 'static,
{
    pub fn new(backing: Arc<dyn ColdBacking<Data>>, counter: Arc<dyn TokenCounter>) -> Self {
        Self { backing, counter }
    }

    pub fn project<SummaryData>(
        heap: &ContextHeap<Data>,
        scopes: &Scopes,
        catalog: &ColdCatalog<SummaryData>,
        turn: u64,
    ) -> ViewSpace {
        ViewSpace::project(heap, scopes, catalog, turn)
    }

    pub async fn usage(&self, space: ViewSpace) -> Result<ViewUsage, ViewError> {
        let counter = Arc::clone(&self.counter);
        // A token counter may do substantial CPU work; the state lock is released.
        tokio::task::spawn_blocking(move || space.usage(counter.as_ref()))
            .await
            .map_err(|error| ViewError::Worker(Arc::new(error)))
    }

    pub fn prepare<SummaryData>(
        &self,
        heap: &ContextHeap<Data>,
        scopes: &Scopes,
        catalog: &ColdCatalog<SummaryData>,
        turn: u64,
        explicit_cold: &[ContextId],
    ) -> Result<ViewPlan<Data>, ViewError> {
        let space = Self::project(heap, scopes, catalog, turn);
        let mut recalled = Vec::new();
        let mut seen = BTreeSet::new();
        for id in explicit_cold {
            if !seen.insert(*id) {
                continue;
            }
            if let Some((zone, entry)) = heap.find(*id) {
                if zone == ZoneKind::Cold {
                    recalled.push(StoredCandidate {
                        id: *id,
                        scope: entry.scope,
                        revision: entry.item.revision,
                        processing: entry.item.processing.clone(),
                        resident: Some(entry.item.clone()),
                    });
                }
            } else {
                let entry = catalog.get(*id).ok_or(ViewError::UnknownContext(*id))?;
                if entry.location != CatalogLocation::Backing {
                    return Err(ViewError::Invariant(
                        "ColdCatalog points to missing Cold entry",
                    ));
                }
                recalled.push(StoredCandidate {
                    id: *id,
                    scope: entry.scope,
                    revision: entry.revision,
                    processing: entry.processing.clone(),
                    resident: None,
                });
            }
        }
        Ok(ViewPlan { space, recalled })
    }

    pub async fn build(&self, plan: ViewPlan<Data>) -> Result<ContextView, ViewError> {
        let backing = Arc::clone(&self.backing);
        let counter = Arc::clone(&self.counter);
        tokio::task::spawn_blocking(move || {
            let mut view = plan.space.view();
            view.usage = plan.space.usage(counter.as_ref());
            let mut recalled_notes = Vec::new();
            for stored in plan.recalled {
                let item = match stored.resident {
                    Some(item) => item,
                    None => backing
                        .load(stored.id)
                        .map_err(ViewError::ColdBacking)?
                        .ok_or(ViewError::Invariant("backing lost cataloged info"))?,
                };
                if item.id != stored.id
                    || item.revision != stored.revision
                    || item.processing != stored.processing
                {
                    return Err(ViewError::Invariant(
                        "backing returned wrong identity, revision, or processing state",
                    ));
                }
                let note = note_for_item(stored.scope, &item);
                recalled_notes.push(note.clone());
                view.notes.push(note);
            }
            view.usage.recalled = counter.count(
                &ContextView {
                    notes: recalled_notes,
                    ..ContextView::default()
                }
                .notes_markdown(),
            );
            view.usage.total = counter.count(&view.markdown());
            Ok(view)
        })
        .await
        .map_err(|error| ViewError::Worker(Arc::new(error)))?
    }
}
