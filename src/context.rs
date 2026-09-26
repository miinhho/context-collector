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
pub struct RawInfo {
    pub content: String,
}

impl From<String> for RawInfo {
    fn from(content: String) -> Self {
        Self { content }
    }
}

impl From<&str> for RawInfo {
    fn from(content: &str) -> Self {
        Self::from(content.to_owned())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Info<Data = ()> {
    pub content: String,
    pub sources: Vec<SourceSpan>,
    pub data: Data,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InfoKind<Data = ()> {
    Raw(RawInfo),
    Info(Info<Data>),
}

impl<Data> InfoKind<Data> {
    pub fn content(&self) -> &str {
        match self {
            Self::Raw(raw) => &raw.content,
            Self::Info(info) => &info.content,
        }
    }

    pub fn is_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProcessingFailure {
    External,
    InvalidResult,
    NoSummaryReturned,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessingAttempt {
    pub attempts: u32,
    pub failures: u32,
    pub completed: bool,
    pub exhausted: bool,
    pub last_failure: Option<ProcessingFailure>,
    pub last_attempt_turn: Option<u64>,
}

impl ProcessingAttempt {
    pub(crate) fn succeed(&mut self, turn: u64) {
        self.attempts += 1;
        self.completed = true;
        self.last_failure = None;
        self.last_attempt_turn = Some(turn);
    }

    pub(crate) fn defer(&mut self, turn: u64) {
        self.attempts += 1;
        self.last_failure = None;
        self.last_attempt_turn = Some(turn);
    }

    pub(crate) fn fail(&mut self, turn: u64, failure: ProcessingFailure, max_attempts: u32) {
        self.attempts += 1;
        self.failures += 1;
        self.last_failure = Some(failure);
        self.last_attempt_turn = Some(turn);
        self.exhausted = self.failures >= max_attempts;
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessingState {
    pub hot_refinement: ProcessingAttempt,
    pub cold_refinement: ProcessingAttempt,
    pub cold_summary: ProcessingAttempt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextItem<Data = ()> {
    pub id: ContextId,
    /// Revision of the information payload; processing attempts do not change it.
    pub revision: u64,
    pub kind: InfoKind<Data>,
    pub processing: ProcessingState,
}

impl<Data> ContextItem<Data> {
    pub fn raw(id: ContextId, content: String) -> Self {
        Self {
            id,
            revision: 1,
            kind: InfoKind::Raw(RawInfo { content }),
            processing: ProcessingState::default(),
        }
    }

    pub fn info(id: ContextId, content: String, sources: Vec<SourceSpan>, data: Data) -> Self {
        Self {
            id,
            revision: 1,
            kind: InfoKind::Info(Info {
                content,
                sources,
                data,
            }),
            processing: ProcessingState::default(),
        }
    }

    pub(crate) fn without_user_data(self) -> ContextItem {
        let kind = match self.kind {
            InfoKind::Raw(raw) => InfoKind::Raw(raw),
            InfoKind::Info(info) => InfoKind::Info(Info {
                content: info.content,
                sources: info.sources,
                data: (),
            }),
        };
        ContextItem {
            id: self.id,
            revision: self.revision,
            kind,
            processing: ProcessingState::default(),
        }
    }
}
