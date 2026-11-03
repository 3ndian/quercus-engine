//! Composite editor commands grouped by domain.
//!
//! These functions operate on the session's live `ProjectRef` and use quercus-undo
//! commands for user-visible edits. The dispatch layer supplies sequence/session
//! context; this layer owns graph validation and undo grouping.

pub mod prelude;
pub use prelude::*;
mod project;
mod parameters;
pub use parameters::*;
pub use project::*;
pub(crate) mod sequence;
pub use sequence::*;
mod media;
pub use media::*;
mod library;
pub use library::*;
mod tracks;
pub use tracks::*;
mod transitions;
pub use transitions::*;
mod generators;
pub use generators::*;
mod editing;
pub use editing::*;
mod moving;
pub use moving::*;
mod multicam;
pub use multicam::*;
mod multicam_wizard;
pub use multicam_wizard::*;
mod edit_menu;
mod subtitles;
pub use subtitles::*;
pub use edit_menu::*;
#[cfg(test)]
mod tests;
