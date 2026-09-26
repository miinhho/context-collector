use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::cold::{CatalogLocation, ColdBacking};
use crate::context::{ContextId, ContextObject, ScopeId};
use crate::heap::ZoneKind;
use crate::token::TokenCounter;
use crate::view::{ColdScopeSummaryView, ColdScopeView, ContextView, ContextViewItem, TokenSpace};

use crate::cold::ColdCatalog;
use crate::heap::ContextHeap;
use crate::scope::Scopes;

#[derive(Debug)]
pub(crate) enum ViewError {
    UnknownContext(ContextId),
    Invariant(&'static str),
    ColdBacking(String),
    Worker(String),
}

struct PlannedItem {
    id: ContextId,
    revision: u64,
    scope: ScopeId,
    zone: Option<ZoneKind>,
    resident: Option<ContextObject>,
}

pub(crate) struct ViewPlan {
    items: Vec<PlannedItem>,
    cold_scopes: Vec<ColdScopeView>,
    used_tokens: usize,
}

pub(crate) struct ViewBuilder {
    counter: Arc<dyn TokenCounter>,
    backing: Arc<dyn ColdBacking>,
}

impl ViewBuilder {
    pub fn new(counter: Arc<dyn TokenCounter>, backing: Arc<dyn ColdBacking>) -> Self {
        Self { counter, backing }
    }

    pub fn prepare(
        &self,
        heap: &ContextHeap,
        scopes: &Scopes,
        catalog: &ColdCatalog,
        turn: u64,
        budget: TokenSpace,
        explicit_cold: &[ContextId],
    ) -> Result<ViewPlan, ViewError> {
        let mut items = Vec::new();
        let mut used_tokens: usize = 0;
        let mut seen = BTreeSet::new();
        for id in explicit_cold {
            if !seen.insert(*id) {
                continue;
            }
            let (scope, zone, resident, tokens, revision) =
                if let Some((zone, entry)) = heap.find(*id) {
                    if zone != ZoneKind::Cold {
                        continue;
                    }
                    (
                        entry.scope,
                        Some(zone),
                        Some(entry.object.clone()),
                        entry.tokens,
                        entry.object.revision,
                    )
                } else {
                    let entry = catalog.get(*id).ok_or(ViewError::UnknownContext(*id))?;
                    if entry.location != CatalogLocation::Backing {
                        return Err(ViewError::Invariant(
                            "ColdCatalog points to missing Cold entry",
                        ));
                    }
                    (entry.scope, None, None, entry.tokens, entry.revision)
                };
            if used_tokens.saturating_add(tokens) > budget.0 {
                continue;
            }
            items.push(PlannedItem {
                id: *id,
                revision,
                scope,
                zone,
                resident,
            });
            used_tokens += tokens;
        }
        let current = scopes.current();
        let mut candidates = Vec::new();
        for zone in [
            ZoneKind::Eden,
            ZoneKind::Survivor,
            ZoneKind::Mature,
            ZoneKind::Cooling,
        ] {
            for entry in heap.zone(zone).entries() {
                if entry.scope == current || entry.last_used_turn == Some(turn) {
                    let priority = if entry.scope == current { 0 } else { 1 };
                    candidates.push((priority, zone, entry.id, entry.tokens));
                }
            }
        }
        candidates.sort_by_key(|(priority, _, id, _)| (*priority, std::cmp::Reverse(id.0)));
        for (_, zone, id, tokens) in candidates {
            if used_tokens.saturating_add(tokens) > budget.0 {
                continue;
            }
            let entry = heap.zone(zone).get(id).expect("listed entry exists");
            items.push(PlannedItem {
                id,
                revision: entry.object.revision,
                scope: entry.scope,
                zone: Some(zone),
                resident: Some(entry.object.clone()),
            });
            used_tokens += tokens;
        }
        let mut scope_counts = BTreeMap::<ScopeId, usize>::new();
        for entry in catalog.entries() {
            *scope_counts.entry(entry.scope).or_default() += 1;
        }
        let mut scope_counts: Vec<_> = scope_counts.into_iter().collect();
        scope_counts.sort_by_key(|(scope, _)| {
            (
                *scope != current,
                catalog.summaries(*scope).is_empty(),
                std::cmp::Reverse(scope.0),
            )
        });
        let mut cold_scopes = Vec::new();
        for (scope, object_count) in scope_counts {
            let header = format!("scope:{} objects:{}", scope.0, object_count);
            let header_tokens = self.counter.count(&header);
            if used_tokens.saturating_add(header_tokens) > budget.0 {
                continue;
            }
            let mut scope_view = ColdScopeView {
                scope,
                object_count,
                summaries: Vec::new(),
            };
            used_tokens += header_tokens;
            for summary in catalog.summaries(scope) {
                let description = format!(
                    "{} {:?} {}",
                    summary.content,
                    summary.references,
                    summary.coverage.len()
                );
                let tokens = self.counter.count(&description);
                if used_tokens.saturating_add(tokens) > budget.0 {
                    break;
                }
                scope_view.summaries.push(ColdScopeSummaryView {
                    content: summary.content.clone(),
                    references: summary.references.clone(),
                    covered_objects: summary.coverage.len(),
                });
                used_tokens += tokens;
            }
            cold_scopes.push(scope_view);
        }
        Ok(ViewPlan {
            items,
            cold_scopes,
            used_tokens,
        })
    }

    pub async fn build(&self, plan: ViewPlan) -> Result<ContextView, ViewError> {
        let backing = Arc::clone(&self.backing);
        tokio::task::spawn_blocking(move || {
            let mut items = Vec::with_capacity(plan.items.len());
            for item in plan.items {
                let object = match item.resident {
                    Some(object) => object,
                    None => backing
                        .load(item.id)
                        .map_err(ViewError::ColdBacking)?
                        .ok_or(ViewError::Invariant("backing lost cataloged object"))?,
                };
                if object.id != item.id || object.revision != item.revision {
                    return Err(ViewError::Invariant(
                        "backing returned wrong identity or revision",
                    ));
                }
                items.push(ContextViewItem {
                    scope: item.scope,
                    zone: item.zone,
                    object,
                });
            }
            Ok(ContextView {
                items,
                cold_scopes: plan.cold_scopes,
                used_tokens: plan.used_tokens,
            })
        })
        .await
        .map_err(|error| ViewError::Worker(error.to_string()))?
    }
}
