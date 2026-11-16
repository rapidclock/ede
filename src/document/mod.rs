//! Disk-backed UTF-8 documents, independent of terminal state.
//!
//! [`Document`] and [`Snapshot`] perform blocking storage work and belong on a
//! storage worker, never the terminal thread. [`StorageService`] is the bounded,
//! nonblocking bridge. Logical offsets exclude a leading UTF-8 BOM.

mod bitmap;
mod cache;
mod cleanup;
mod compact;
mod history;
mod index;
mod live;
mod model;
mod platform;
mod prepared_edit;
mod refresh;
mod resources;
mod save;
mod service;
mod snapshot;
mod source;
mod transaction;
mod tree;
mod types;

#[doc(hidden)]
pub use live::CaptureAdoption;
pub(crate) use live::{Capture, CaptureFactory};
pub use model::Document;
pub(crate) use platform::{Version as FileVersion, open_regular};
pub use prepared_edit::{EditReport, EditTask, PreparedEdit};
pub(crate) use refresh::observe;
pub use refresh::{FileObservation, PreparedRefresh, RefreshProgress, RefreshReport, RefreshTask};
pub(crate) use resources::Permit;
pub use save::{SaveOutcome, SaveReport, SaveTarget};
pub use service::{
    Completion, DocumentInfo, Job, Request, Response, Shutdown, StorageService, SubmissionError,
};
pub use snapshot::{Snapshot, Window};
pub use transaction::StagedTransaction;
pub use types::{
    ByteOffset, Cancellation, DocumentId, Edit, Error, FaultInjector, Format, IoPoint, JobId,
    Newline, OpenOptions, OpenProgress, Revision, SourceId, StorageLimits, StorageStats, TextRange,
};
