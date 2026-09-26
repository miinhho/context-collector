mod manager;
pub mod objectization;
pub mod scope_summary;

pub use manager::ObjectizationError;
pub(crate) use manager::{ObjectizationCommit, ObjectizationManager};
