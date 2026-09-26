use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PinnedId(pub u64);

/// Caller-authored text that stays in the Heap and is included in every View.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedEntry {
    pub id: PinnedId,
    pub content: String,
    pub tokens: usize,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PinnedError {
    #[error("Pinned content cannot be empty")]
    Empty,
    #[error("PinnedZone capacity exceeded")]
    CapacityExceeded,
    #[error("PinnedZone identifier exhausted")]
    IdExhausted,
    #[error("unknown Pinned entry {0:?}")]
    Unknown(PinnedId),
}

/// Fixed residency, insertion order, and token accounting; no GC watermark.
#[derive(Debug)]
pub struct PinnedZone {
    entries: Vec<PinnedEntry>,
    capacity: usize,
    usage: usize,
    next_id: u64,
}

impl PinnedZone {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity,
            usage: 0,
            next_id: 1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn usage(&self) -> usize {
        self.usage
    }

    pub fn entries(&self) -> &[PinnedEntry] {
        &self.entries
    }

    pub(crate) fn insert(
        &mut self,
        content: String,
        tokens: usize,
    ) -> Result<PinnedId, PinnedError> {
        if content.is_empty() {
            return Err(PinnedError::Empty);
        }
        let usage = self
            .usage
            .checked_add(tokens)
            .filter(|usage| *usage <= self.capacity)
            .ok_or(PinnedError::CapacityExceeded)?;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(PinnedError::IdExhausted)?;
        let id = PinnedId(self.next_id);
        self.entries.push(PinnedEntry {
            id,
            content,
            tokens,
        });
        self.next_id = next_id;
        self.usage = usage;
        Ok(id)
    }

    pub(crate) fn remove(&mut self, id: PinnedId) -> Result<(), PinnedError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(PinnedError::Unknown(id))?;
        let entry = self.entries.remove(index);
        self.usage -= entry.tokens;
        Ok(())
    }
}
