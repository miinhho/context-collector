use std::collections::BTreeMap;

use crate::context::{ContextId, ContextItem, ScopeId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ZoneKind {
    Eden,
    Survivor,
    Mature,
    Cooling,
    Cold,
}

impl ZoneKind {
    pub const ALL: [Self; 5] = [
        Self::Eden,
        Self::Survivor,
        Self::Mature,
        Self::Cooling,
        Self::Cold,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Eden => 0,
            Self::Survivor => 1,
            Self::Mature => 2,
            Self::Cooling => 3,
            Self::Cold => 4,
        }
    }

    pub fn is_hot(self) -> bool {
        self != Self::Cold
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Watermark {
    pub low: usize,
    pub high: usize,
}

impl Watermark {
    pub fn valid(self) -> bool {
        self.low < self.high
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenUsage {
    pub raw: usize,
    pub info: usize,
}

impl TokenUsage {
    pub fn total(self) -> usize {
        self.raw + self.info
    }

    fn add<Data>(&mut self, entry: &ZoneEntry<Data>) {
        if entry.raw {
            self.raw += entry.tokens;
        } else {
            self.info += entry.tokens;
        }
    }

    fn subtract<Data>(&mut self, entry: &ZoneEntry<Data>) {
        if entry.raw {
            self.raw -= entry.tokens;
        } else {
            self.info -= entry.tokens;
        }
    }
}

#[derive(Clone, Debug)]
pub struct ZoneEntry<Data = ()> {
    pub(crate) id: ContextId,
    pub(crate) scope: ScopeId,
    pub(crate) item: ContextItem<Data>,
    pub(crate) tokens: usize,
    pub(crate) raw: bool,
    pub(crate) born_turn: u64,
    pub(crate) last_used_turn: Option<u64>,
    pub(crate) collections: u32,
    pub(crate) protected: bool,
}

impl<Data> ZoneEntry<Data> {
    pub(crate) fn new(item: ContextItem<Data>, scope: ScopeId, tokens: usize, turn: u64) -> Self {
        Self {
            id: item.id,
            scope,
            raw: item.kind.is_raw(),
            item,
            tokens,
            born_turn: turn,
            last_used_turn: None,
            collections: 0,
            protected: false,
        }
    }

    pub fn scope(&self) -> ScopeId {
        self.scope
    }

    pub fn token_count(&self) -> usize {
        self.tokens
    }

    pub fn is_raw(&self) -> bool {
        self.raw
    }
}

#[derive(Clone, Debug)]
pub struct ScopeBlock {
    pub scope: ScopeId,
    ids: Vec<ContextId>,
}

impl ScopeBlock {
    pub fn ids(&self) -> &[ContextId] {
        &self.ids
    }
}

#[derive(Debug)]
pub struct ContextHeapSpace<Data = ()> {
    kind: ZoneKind,
    watermark: Watermark,
    entries: BTreeMap<ContextId, ZoneEntry<Data>>,
    blocks: Vec<ScopeBlock>,
    usage: TokenUsage,
}

impl<Data> ContextHeapSpace<Data> {
    fn new(kind: ZoneKind, watermark: Watermark) -> Self {
        Self {
            kind,
            watermark,
            entries: BTreeMap::new(),
            blocks: Vec::new(),
            usage: TokenUsage::default(),
        }
    }

    pub fn kind(&self) -> ZoneKind {
        self.kind
    }

    pub fn watermark(&self) -> Watermark {
        self.watermark
    }

    pub fn usage(&self) -> TokenUsage {
        self.usage
    }

    pub fn above_high(&self) -> bool {
        self.usage.total() >= self.watermark.high
    }

    pub fn get(&self, id: ContextId) -> Option<&ZoneEntry<Data>> {
        self.entries.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: ContextId) -> Option<&mut ZoneEntry<Data>> {
        self.entries.get_mut(&id)
    }

    pub fn blocks(&self) -> &[ScopeBlock] {
        &self.blocks
    }

    pub fn ids_for_scope(&self, scope: ScopeId) -> Vec<ContextId> {
        self.blocks
            .iter()
            .filter(|block| block.scope == scope)
            .flat_map(|block| block.ids.iter().copied())
            .collect()
    }

    pub fn entries(&self) -> impl Iterator<Item = &ZoneEntry<Data>> {
        self.entries.values()
    }

    pub(crate) fn insert(&mut self, entry: ZoneEntry<Data>) -> Result<(), Box<ZoneEntry<Data>>> {
        if self.entries.contains_key(&entry.id) {
            return Err(Box::new(entry));
        }
        self.usage.add(&entry);
        if let Some(block) = self
            .blocks
            .iter_mut()
            .find(|block| block.scope == entry.scope)
        {
            block.ids.push(entry.id);
        } else {
            self.blocks.push(ScopeBlock {
                scope: entry.scope,
                ids: vec![entry.id],
            });
        }
        self.entries.insert(entry.id, entry);
        Ok(())
    }

    pub(crate) fn remove(&mut self, id: ContextId) -> Option<ZoneEntry<Data>> {
        let entry = self.entries.remove(&id)?;
        self.usage.subtract(&entry);
        if let Some(index) = self
            .blocks
            .iter()
            .position(|block| block.scope == entry.scope)
        {
            self.blocks[index].ids.retain(|item| *item != id);
            if self.blocks[index].ids.is_empty() {
                self.blocks.remove(index);
            }
        }
        Some(entry)
    }
}

#[derive(Debug)]
pub struct ContextHeap<Data = ()> {
    zones: [ContextHeapSpace<Data>; 5],
}

impl<Data> ContextHeap<Data> {
    pub fn new(watermarks: [Watermark; 5]) -> Option<Self> {
        if watermarks.iter().any(|mark| !mark.valid()) {
            return None;
        }
        Some(Self {
            zones: std::array::from_fn(|index| {
                ContextHeapSpace::new(ZoneKind::ALL[index], watermarks[index])
            }),
        })
    }

    pub fn zone(&self, kind: ZoneKind) -> &ContextHeapSpace<Data> {
        &self.zones[kind.index()]
    }

    pub(crate) fn zone_mut(&mut self, kind: ZoneKind) -> &mut ContextHeapSpace<Data> {
        &mut self.zones[kind.index()]
    }

    pub fn find(&self, id: ContextId) -> Option<(ZoneKind, &ZoneEntry<Data>)> {
        ZoneKind::ALL
            .iter()
            .find_map(|kind| self.zone(*kind).get(id).map(|entry| (*kind, entry)))
    }

    pub fn hot_usage(&self) -> usize {
        ZoneKind::ALL
            .iter()
            .filter(|kind| kind.is_hot())
            .map(|kind| self.zone(*kind).usage().total())
            .sum()
    }
}
