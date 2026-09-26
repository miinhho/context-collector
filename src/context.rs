use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ContextId(pub u64);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ScopeId(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub raw: ContextId,
    pub revision: u64,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Representation {
    Raw(String),
    Structured {
        content: String,
        sources: Vec<SourceSpan>,
    },
}

impl Representation {
    pub fn content(&self) -> &str {
        match self {
            Self::Raw(content) | Self::Structured { content, .. } => content,
        }
    }

    pub fn is_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextObject {
    pub id: ContextId,
    pub revision: u64,
    pub representation: Representation,
}

impl ContextObject {
    pub fn raw(id: ContextId, content: String) -> Self {
        Self {
            id,
            revision: 1,
            representation: Representation::Raw(content),
        }
    }

    pub fn structured(id: ContextId, content: String, sources: Vec<SourceSpan>) -> Self {
        Self {
            id,
            revision: 1,
            representation: Representation::Structured { content, sources },
        }
    }
}

pub use crate::token::{TiktokenCounter, TokenCounter};
