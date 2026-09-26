use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ContextId(pub u64);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ScopeId(pub u64);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceSpan {
    pub raw: ContextId,
    pub revision: u64,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Representation<Data = ()> {
    Raw(String),
    Structured {
        content: String,
        sources: Vec<SourceSpan>,
        data: Data,
    },
}

impl<Data> Representation<Data> {
    pub fn content(&self) -> &str {
        match self {
            Self::Raw(content) | Self::Structured { content, .. } => content,
        }
    }

    pub fn is_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextObject<Data = ()> {
    pub id: ContextId,
    pub revision: u64,
    pub representation: Representation<Data>,
}

impl<Data> ContextObject<Data> {
    pub fn raw(id: ContextId, content: String) -> Self {
        Self {
            id,
            revision: 1,
            representation: Representation::Raw(content),
        }
    }

    pub fn structured(
        id: ContextId,
        content: String,
        sources: Vec<SourceSpan>,
        data: Data,
    ) -> Self {
        Self {
            id,
            revision: 1,
            representation: Representation::Structured {
                content,
                sources,
                data,
            },
        }
    }

    pub(crate) fn without_user_data(self) -> ContextObject {
        let representation = match self.representation {
            Representation::Raw(content) => Representation::Raw(content),
            Representation::Structured {
                content, sources, ..
            } => Representation::Structured {
                content,
                sources,
                data: (),
            },
        };
        ContextObject {
            id: self.id,
            revision: self.revision,
            representation,
        }
    }
}
