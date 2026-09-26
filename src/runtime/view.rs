use super::*;

impl RuntimeState {
    pub fn context_view(
        &self,
        budget: TokenSpace,
        explicit_cold: &[ContextId],
    ) -> Result<ContextView, RuntimeError> {
        let mut view = ContextView::default();
        let mut seen = BTreeSet::new();
        for id in explicit_cold {
            if !seen.insert(*id) {
                continue;
            }
            let item = if let Some((zone, entry)) = self.heap.find(*id) {
                if zone != ZoneKind::Cold {
                    continue;
                }
                (entry.scope, Some(zone), entry.object.clone(), entry.tokens)
            } else {
                let entry = self
                    .catalog
                    .get(*id)
                    .ok_or(RuntimeError::UnknownContext(*id))?;
                if entry.location != CatalogLocation::Backing {
                    return Err(RuntimeError::Invariant(
                        "ColdCatalog points to missing Cold entry",
                    ));
                }
                let object = self
                    .backing
                    .load(*id)
                    .map_err(RuntimeError::ColdBacking)?
                    .ok_or(RuntimeError::UnknownContext(*id))?;
                (entry.scope, None, object, entry.tokens)
            };
            if view.used_tokens.saturating_add(item.3) > budget.0 {
                continue;
            }
            view.items.push(ContextViewItem {
                scope: item.0,
                zone: item.1,
                object: item.2,
            });
            view.used_tokens += item.3;
        }
        let current = self.scopes.current();
        let mut candidates = Vec::new();
        for zone in [
            ZoneKind::Eden,
            ZoneKind::Survivor,
            ZoneKind::Mature,
            ZoneKind::Cooling,
        ] {
            for entry in self.heap.zone(zone).entries() {
                if entry.scope == current || entry.last_used_turn == Some(self.turn) {
                    let priority = if entry.scope == current { 0 } else { 1 };
                    candidates.push((priority, zone, entry.id, entry.tokens));
                }
            }
        }
        candidates.sort_by_key(|(priority, _, id, _)| (*priority, std::cmp::Reverse(id.0)));
        for (_, zone, id, tokens) in candidates {
            if view.used_tokens.saturating_add(tokens) > budget.0 {
                continue;
            }
            let entry = self.heap.zone(zone).get(id).expect("listed entry exists");
            view.items.push(ContextViewItem {
                scope: entry.scope,
                zone: Some(zone),
                object: entry.object.clone(),
            });
            view.used_tokens += tokens;
        }
        let mut scope_counts = std::collections::BTreeMap::<ScopeId, usize>::new();
        for entry in self.catalog.entries() {
            *scope_counts.entry(entry.scope).or_default() += 1;
        }
        let current_scope = self.scopes.current();
        let mut scope_counts: Vec<_> = scope_counts.into_iter().collect();
        scope_counts.sort_by_key(|(scope, _)| {
            (
                *scope != current_scope,
                self.catalog.summaries(*scope).is_empty(),
                std::cmp::Reverse(scope.0),
            )
        });
        for (scope, object_count) in scope_counts {
            let header = format!("scope:{} objects:{}", scope.0, object_count);
            let header_tokens = self.counter.count(&header);
            if view.used_tokens.saturating_add(header_tokens) > budget.0 {
                continue;
            }
            let mut scope_view = ColdScopeView {
                scope,
                object_count,
                summaries: Vec::new(),
            };
            view.used_tokens += header_tokens;
            for summary in self.catalog.summaries(scope) {
                let description = format!(
                    "{} {:?} {}",
                    summary.content,
                    summary.references,
                    summary.coverage.len()
                );
                let tokens = self.counter.count(&description);
                if view.used_tokens.saturating_add(tokens) > budget.0 {
                    break;
                }
                scope_view.summaries.push(ColdScopeSummaryView {
                    content: summary.content.clone(),
                    references: summary.references.clone(),
                    covered_objects: summary.coverage.len(),
                });
                view.used_tokens += tokens;
            }
            view.cold_scopes.push(scope_view);
        }
        Ok(view)
    }
}
