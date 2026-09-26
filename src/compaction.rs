mod manager;
pub mod refinement;
pub mod scope_summary;

pub use manager::RefinementError;
pub(crate) use manager::{RefinementCommit, RefinementManager};
