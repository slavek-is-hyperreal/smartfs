//! Command implementations for `smartfs-cli`.

pub mod calibrate;
pub mod cat;
pub mod concepts;
pub mod diff;
pub mod history;
pub mod import;
pub mod recover;
pub mod search;
pub mod status;
pub mod write;

pub use calibrate::{handle_calibrate, CalibrateResult};
pub use cat::{handle_cat, CatResult};
pub use concepts::{handle_concepts, ConceptSummary, ConceptsResult};
pub use diff::{handle_diff, DiffResult};
pub use history::{handle_history, HistoryResult};
pub use import::{handle_import, ImportResult};
pub use recover::{handle_recover, RecoverResult};
pub use search::{handle_search, SearchResult};
pub use status::{handle_status, read_pending_queue_state, SystemStatus};
pub use write::{handle_write, WriteResult};
