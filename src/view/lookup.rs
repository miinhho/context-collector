use std::collections::{BTreeMap, BTreeSet};

use crate::cold::ColdCatalog;
use crate::context::{ContextId, ContextItem, ScopeId};
use crate::heap::{ContextHeap, ZoneKind};
use crate::scope::Scopes;

use super::{ContextView, ViewNote, note_for_item, references};

pub(crate) struct ScopeLookup {
    members: Vec<ContextId>,
    summaries: Vec<ViewNote>,
}

pub(crate) fn recent_ids<Data, SummaryData>(
    heap: &ContextHeap<Data>,
    catalog: &ColdCatalog<SummaryData>,
    scope: Option<ScopeId>,
    limit: usize,
) -> Vec<(ContextId, ScopeId)> {
    let mut found = BTreeMap::new();
    for zone in ZoneKind::ALL {
        for entry in heap.zone(zone).entries() {
            if let Some(turn) = entry.last_used_turn
                && scope.is_none_or(|scope| scope == entry.scope())
            {
                found.insert(entry.item.id, (entry.scope(), turn));
            }
        }
    }
    for entry in catalog.entries() {
        if let Some(turn) = entry.last_used_turn
            && scope.is_none_or(|scope| scope == entry.scope)
        {
            found.insert(entry.id, (entry.scope, turn));
        }
    }
    let mut recent = found.into_iter().collect::<Vec<_>>();
    recent.sort_by_key(|(id, (_, turn))| (std::cmp::Reverse(*turn), std::cmp::Reverse(*id)));
    recent
        .into_iter()
        .take(limit)
        .map(|(id, (scope, _))| (id, scope))
        .collect()
}

pub(crate) fn recent_markdown<Data>(items: Vec<(ScopeId, ContextItem<Data>)>) -> String {
    let notes = items
        .iter()
        .map(|(scope, item)| note_for_item(*scope, item))
        .collect();
    ContextView {
        notes,
        ..ContextView::default()
    }
    .notes_markdown()
}

pub(crate) fn scope_plan<SummaryData>(
    scopes: &Scopes,
    catalog: &ColdCatalog<SummaryData>,
    scope: ScopeId,
) -> Option<ScopeLookup> {
    let owned = scopes.get(scope)?;
    let summaries = catalog
        .summaries(scope)
        .iter()
        .rev()
        .map(|summary| ViewNote {
            id: None,
            scope,
            message: None,
            content: summary.content.clone(),
            sources: Vec::new(),
            coverage: summary.coverage.iter().map(|(id, _)| *id).collect(),
        })
        .collect();
    Some(ScopeLookup {
        members: owned.members().collect(),
        summaries,
    })
}

pub(crate) fn scope_markdown(plan: ScopeLookup) -> String {
    let covered = plan
        .summaries
        .iter()
        .flat_map(|summary| summary.coverage.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut output = ContextView {
        notes: plan.summaries,
        ..ContextView::default()
    }
    .notes_markdown();
    let extra = plan
        .members
        .into_iter()
        .filter(|id| !covered.contains(id))
        .collect::<Vec<_>>();
    if !extra.is_empty() {
        output.push_str(&format!(
            "추가로 조회할 수 있는 기록: {}\n",
            references(&extra)
        ));
    }
    output
}

pub(crate) fn item_markdown<Data>(scope: ScopeId, item: &ContextItem<Data>) -> String {
    ContextView {
        notes: vec![note_for_item(scope, item)],
        ..ContextView::default()
    }
    .notes_markdown()
}

pub(crate) fn evidence_markdown(excerpts: &[(ContextId, usize, usize, String)]) -> String {
    let mut output = String::new();
    for (raw, start, end, excerpt) in excerpts {
        output.push_str(&format!(
            "근거 #{} [{}..{}]:\n> {}\n",
            raw.0,
            start,
            end,
            excerpt.replace('\n', "\n> ")
        ));
    }
    output
}
