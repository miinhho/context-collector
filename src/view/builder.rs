use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;

use crate::cold::{CatalogLocation, ColdBacking, ColdCatalog};
use crate::context::{ContextId, ContextItem, InfoKind, MessageRole, ProcessingState, ScopeId};
use crate::error::ExternalError;
use crate::heap::{ContextHeap, ZoneKind};
use crate::scope::Scopes;
use crate::view::{ContextView, ViewMessage, ViewNote, note_for_item};

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

enum Candidate<Data> {
    Note(ViewNote),
    Explicit(StoredCandidate<Data>),
}

pub(crate) struct ViewPlan<Data> {
    messages: Vec<ViewMessage>,
    hot_notes: Vec<ViewNote>,
    additions: Vec<Candidate<Data>>,
}

pub(crate) struct ViewBuilder<Data> {
    backing: Arc<dyn ColdBacking<Data>>,
}

impl<Data> ViewBuilder<Data>
where
    Data: Clone + Send + Sync + 'static,
{
    pub fn new(backing: Arc<dyn ColdBacking<Data>>) -> Self {
        Self { backing }
    }

    pub fn prepare<SummaryData>(
        &self,
        heap: &ContextHeap<Data>,
        scopes: &Scopes,
        catalog: &ColdCatalog<SummaryData>,
        turn: u64,
        explicit_cold: &[ContextId],
    ) -> Result<ViewPlan<Data>, ViewError> {
        let current = scopes.current();
        let mut messages = Vec::new();
        let mut infos = Vec::new();
        for zone in [
            ZoneKind::Eden,
            ZoneKind::Survivor,
            ZoneKind::Mature,
            ZoneKind::Cooling,
        ] {
            for entry in heap.zone(zone).entries() {
                if entry.scope != current && entry.last_used_turn != Some(turn) {
                    continue;
                }
                match &entry.item.kind {
                    InfoKind::Raw(raw) => {
                        let Some(origin) = entry.item.message else {
                            continue;
                        };
                        let message = ViewMessage {
                            id: entry.id,
                            scope: entry.scope,
                            turn: origin.turn,
                            role: origin.role,
                            content: raw.content.clone(),
                        };
                        messages.push(message);
                    }
                    InfoKind::Info(_) => {
                        infos.push((
                            entry.last_used_turn == Some(turn),
                            entry.id,
                            note_for_item(entry.scope, &entry.item),
                        ));
                    }
                }
            }
        }
        messages.sort_by_key(|message| (message.turn, role_order(message.role), message.id));
        infos.sort_by_key(|(used, id, _)| (std::cmp::Reverse(*used), std::cmp::Reverse(*id)));
        let mut additions = Vec::new();
        let mut seen = BTreeSet::new();
        for id in explicit_cold {
            if !seen.insert(*id) {
                continue;
            }
            if let Some((zone, entry)) = heap.find(*id) {
                if zone == ZoneKind::Cold {
                    additions.push(Candidate::Explicit(StoredCandidate {
                        id: *id,
                        scope: entry.scope,
                        revision: entry.item.revision,
                        processing: entry.item.processing.clone(),
                        resident: Some(entry.item.clone()),
                    }));
                }
            } else {
                let entry = catalog.get(*id).ok_or(ViewError::UnknownContext(*id))?;
                if entry.location != CatalogLocation::Backing {
                    return Err(ViewError::Invariant(
                        "ColdCatalog points to missing Cold entry",
                    ));
                }
                additions.push(Candidate::Explicit(StoredCandidate {
                    id: *id,
                    scope: entry.scope,
                    revision: entry.revision,
                    processing: entry.processing.clone(),
                    resident: None,
                }));
            }
        }
        let hot_notes = infos.into_iter().map(|(_, _, note)| note).collect();

        let mut relevant_scopes = BTreeSet::from([current]);
        for entry in catalog.entries() {
            if entry.last_used_turn == Some(turn) {
                relevant_scopes.insert(entry.scope);
            }
        }
        for scope in scopes.all() {
            if scope.last_transition_out() == Some(turn) {
                relevant_scopes.insert(scope.id);
            }
        }
        let mut relevant_scopes = relevant_scopes.into_iter().collect::<Vec<_>>();
        relevant_scopes.sort_by_key(|scope| (*scope != current, std::cmp::Reverse(*scope)));
        for scope in relevant_scopes {
            for summary in catalog.summaries(scope).iter().rev() {
                additions.push(Candidate::Note(ViewNote {
                    id: None,
                    scope,
                    message: None,
                    content: summary.content.clone(),
                    sources: Vec::new(),
                    coverage: summary.coverage.iter().map(|(id, _)| *id).collect(),
                }));
            }
        }
        Ok(ViewPlan {
            messages,
            hot_notes,
            additions,
        })
    }

    pub async fn build(&self, plan: ViewPlan<Data>) -> Result<ContextView, ViewError> {
        let backing = Arc::clone(&self.backing);
        tokio::task::spawn_blocking(move || {
            let mut view = ContextView {
                notes: plan.hot_notes,
                messages: plan.messages,
            };
            for candidate in plan.additions {
                let note = match candidate {
                    Candidate::Note(note) => note,
                    Candidate::Explicit(stored) => {
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
                        note_for_item(stored.scope, &item)
                    }
                };
                view.notes.push(note);
            }
            Ok(view)
        })
        .await
        .map_err(|error| ViewError::Worker(Arc::new(error)))?
    }
}

fn role_order(role: MessageRole) -> u8 {
    match role {
        MessageRole::User => 0,
        MessageRole::Agent => 1,
    }
}
