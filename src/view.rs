use std::collections::BTreeSet;

use crate::context::{ContextId, ContextItem, InfoKind, MessageOrigin, MessageRole, ScopeId};

mod builder;
pub(crate) mod lookup;
mod space;
pub(crate) use builder::ViewBuilder;
pub use builder::ViewError;
pub use space::{PinnedViewUsage, ViewSectionUsage, ViewUsage};

/// A turn message retained with its original speaker and order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewMessage {
    pub id: ContextId,
    pub scope: ScopeId,
    pub turn: u64,
    pub role: MessageRole,
    pub content: String,
}

/// Information selected to supply context around the ordered conversation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewNote {
    pub id: Option<ContextId>,
    pub scope: ScopeId,
    pub message: Option<MessageOrigin>,
    pub content: String,
    pub sources: Vec<ContextId>,
    pub coverage: Vec<ContextId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextView {
    /// Caller-authored fragments, preserved verbatim and ordered before dynamic context.
    pub pinned: Vec<String>,
    pub notes: Vec<ViewNote>,
    pub messages: Vec<ViewMessage>,
    /// Delivery measurement from the same renderer used for this View.
    pub usage: ViewUsage,
}

impl ContextView {
    pub fn notes_markdown(&self) -> String {
        if self.notes.is_empty() {
            return String::new();
        }
        let mut output = String::from("이전에 확인된 내용:\n");
        for note in &self.notes {
            output.push_str("- ");
            if let Some(id) = note.id {
                output.push_str(&format!("#{}", id.0));
                if let Some(origin) = note.message {
                    let role = match origin.role {
                        MessageRole::User => "사용자",
                        MessageRole::Agent => "Agent",
                    };
                    output.push_str(&format!(" ({role})"));
                }
                output.push_str(": ");
            }
            output.push_str(&note.content.replace('\n', "\n  "));
            output.push('\n');
            if !note.sources.is_empty() {
                output.push_str("  근거: ");
                output.push_str(&references(&note.sources));
                output.push('\n');
            }
            if !note.coverage.is_empty() {
                output.push_str("  반영된 정보: ");
                output.push_str(&references(&note.coverage));
                output.push('\n');
            }
        }
        output
    }

    /// Markdown rendering of selected information and the ordered conversation.
    pub fn markdown(&self) -> String {
        let mut output = self.notes_markdown();
        if !self.messages.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str("대화:\n");
            for message in &self.messages {
                let role = match message.role {
                    MessageRole::User => "사용자",
                    MessageRole::Agent => "Agent",
                };
                output.push_str(&format!(
                    "- {role} (#{}): {}\n",
                    message.id.0,
                    message.content.replace('\n', "\n  ")
                ));
            }
        }
        if self.pinned.is_empty() {
            return output;
        }
        let mut pinned = self.pinned.join("\n\n");
        if !output.is_empty() {
            pinned.push_str("\n\n");
            pinned.push_str(&output);
        }
        pinned
    }
}

pub(crate) fn references(ids: &[ContextId]) -> String {
    ids.iter()
        .map(|id| format!("#{}", id.0))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn note_for_item<Data>(scope: ScopeId, item: &ContextItem<Data>) -> ViewNote {
    match &item.kind {
        InfoKind::Raw(raw) => ViewNote {
            id: Some(item.id),
            scope,
            message: item.message,
            content: raw.content.clone(),
            sources: Vec::new(),
            coverage: Vec::new(),
        },
        InfoKind::Info(info) => ViewNote {
            id: Some(item.id),
            scope,
            message: None,
            content: info.content.clone(),
            sources: info
                .sources
                .iter()
                .map(|source| source.raw)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            coverage: Vec::new(),
        },
    }
}
