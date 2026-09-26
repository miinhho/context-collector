use std::collections::{BTreeMap, BTreeSet};

use crate::cold::ColdCatalog;
use crate::context::{ContextId, InfoKind, MessageRole};
use crate::heap::{ContextHeap, TokenUsage, Watermark, ZoneKind};
use crate::scope::Scopes;
use crate::token::TokenCounter;

use super::{ContextView, ViewMessage, ViewNote, note_for_item};

/// Rendered delivery use, derived from one snapshot of the information state.
/// The final total is measured after merging sections, so it need not equal
/// the sum of their independently rendered token counts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ViewUsage {
    pub total: usize,
    pub pinned: PinnedViewUsage,
    pub sections: BTreeMap<ZoneKind, ViewSectionUsage>,
    pub recalled: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PinnedViewUsage {
    /// Tokens after joining caller-authored fragments for delivery.
    pub rendered_tokens: usize,
    /// Sum of individually counted entry payloads used for admission.
    pub stored_tokens: usize,
    pub capacity: usize,
}

impl ViewUsage {
    pub fn section(&self, zone: ZoneKind) -> Option<&ViewSectionUsage> {
        self.sections.get(&zone)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewSectionUsage {
    /// Tokens in the selected Markdown for this section.
    pub rendered_tokens: usize,
    /// Tokens stored in the corresponding Zone, including unselected items.
    pub stored_tokens: TokenUsage,
    pub watermark: Watermark,
}

struct ViewSection {
    zone: ZoneKind,
    stored_tokens: TokenUsage,
    watermark: Watermark,
    messages: Vec<ViewMessage>,
    infos: Vec<(bool, ContextId, ViewNote)>,
    summaries: Vec<ViewNote>,
}

impl ViewSection {
    fn new(zone: ZoneKind, stored_tokens: TokenUsage, watermark: Watermark) -> Self {
        Self {
            zone,
            stored_tokens,
            watermark,
            messages: Vec::new(),
            infos: Vec::new(),
            summaries: Vec::new(),
        }
    }

    fn view(&self) -> ContextView {
        let mut infos = self.infos.clone();
        infos.sort_by_key(|(used, id, _)| (std::cmp::Reverse(*used), std::cmp::Reverse(*id)));
        let mut messages = self.messages.clone();
        messages.sort_by_key(|message| (message.turn, role_order(message.role), message.id));
        let mut notes = infos
            .into_iter()
            .map(|(_, _, note)| note)
            .collect::<Vec<_>>();
        notes.extend(self.summaries.iter().cloned());
        ContextView {
            notes,
            messages,
            ..ContextView::default()
        }
    }
}

/// A read-only projection. Sections account for delivery, not Zone ownership.
pub(crate) struct ViewSpace {
    pinned: Vec<String>,
    pinned_stored: usize,
    pinned_capacity: usize,
    sections: BTreeMap<ZoneKind, ViewSection>,
}

impl ViewSpace {
    pub(crate) fn project<Data, SummaryData>(
        heap: &ContextHeap<Data>,
        scopes: &Scopes,
        catalog: &ColdCatalog<SummaryData>,
        turn: u64,
    ) -> Self {
        let current = scopes.current();
        let mut sections = ZoneKind::ALL
            .into_iter()
            .map(|zone| {
                let space = heap.zone(zone);
                (
                    zone,
                    ViewSection::new(zone, space.usage(), space.watermark()),
                )
            })
            .collect::<BTreeMap<_, _>>();

        // Only information represented in this View can replace a Raw body.
        let represented_raw = ZoneKind::ALL
            .into_iter()
            .filter(|zone| zone.is_hot())
            .flat_map(|zone| heap.zone(zone).entries())
            .filter(|entry| entry.scope == current || entry.last_used_turn == Some(turn))
            .filter_map(|entry| match &entry.item.kind {
                InfoKind::Info(info) => Some(info.sources.iter().map(|span| span.raw)),
                InfoKind::Raw(_) => None,
            })
            .flatten()
            .collect::<BTreeSet<_>>();

        for zone in ZoneKind::ALL.into_iter().filter(|zone| zone.is_hot()) {
            let section = sections.get_mut(&zone).expect("section exists");
            for entry in heap.zone(zone).entries() {
                if entry.scope != current && entry.last_used_turn != Some(turn) {
                    continue;
                }
                match &entry.item.kind {
                    InfoKind::Raw(raw) => {
                        if represented_raw.contains(&entry.id) {
                            continue;
                        }
                        let Some(origin) = entry.item.message else {
                            continue;
                        };
                        section.messages.push(ViewMessage {
                            id: entry.id,
                            scope: entry.scope,
                            turn: origin.turn,
                            role: origin.role,
                            content: raw.content.clone(),
                        });
                    }
                    InfoKind::Info(_) => section.infos.push((
                        entry.last_used_turn == Some(turn),
                        entry.id,
                        note_for_item(entry.scope, &entry.item),
                    )),
                }
            }
        }

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
        let cold = sections.get_mut(&ZoneKind::Cold).expect("section exists");
        for scope in relevant_scopes {
            for summary in catalog.summaries(scope).iter().rev() {
                cold.summaries.push(ViewNote {
                    id: None,
                    scope,
                    message: None,
                    content: summary.content.clone(),
                    sources: Vec::new(),
                    coverage: summary.coverage.iter().map(|(id, _)| *id).collect(),
                });
            }
        }

        Self {
            pinned: heap
                .pinned()
                .entries()
                .iter()
                .map(|entry| entry.content.clone())
                .collect(),
            pinned_stored: heap.pinned().usage(),
            pinned_capacity: heap.pinned().capacity(),
            sections,
        }
    }

    pub(crate) fn view(&self) -> ContextView {
        let mut messages = Vec::new();
        let mut infos = Vec::new();
        for section in self.sections.values() {
            messages.extend(section.messages.iter().cloned());
            infos.extend(section.infos.iter().cloned());
        }
        messages.sort_by_key(|message| (message.turn, role_order(message.role), message.id));
        infos.sort_by_key(|(used, id, _)| (std::cmp::Reverse(*used), std::cmp::Reverse(*id)));
        let mut notes = infos
            .into_iter()
            .map(|(_, _, note)| note)
            .collect::<Vec<_>>();
        notes.extend(self.sections[&ZoneKind::Cold].summaries.iter().cloned());
        ContextView {
            pinned: self.pinned.clone(),
            notes,
            messages,
            ..ContextView::default()
        }
    }

    pub(crate) fn usage(&self, counter: &dyn TokenCounter) -> ViewUsage {
        let sections = self
            .sections
            .iter()
            .map(|(zone, section)| {
                debug_assert_eq!(*zone, section.zone);
                (
                    *zone,
                    ViewSectionUsage {
                        rendered_tokens: counter.count(&section.view().markdown()),
                        stored_tokens: section.stored_tokens,
                        watermark: section.watermark,
                    },
                )
            })
            .collect();
        ViewUsage {
            total: counter.count(&self.view().markdown()),
            pinned: PinnedViewUsage {
                rendered_tokens: counter.count(&self.pinned.join("\n\n")),
                stored_tokens: self.pinned_stored,
                capacity: self.pinned_capacity,
            },
            sections,
            recalled: 0,
        }
    }
}

fn role_order(role: MessageRole) -> u8 {
    match role {
        MessageRole::User => 0,
        MessageRole::Agent => 1,
    }
}
