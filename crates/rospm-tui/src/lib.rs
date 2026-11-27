//! Terminal UI for rospm.

mod actions;
mod app;
mod dashboard;
mod log_view;
mod logs;
mod navigation;
mod packages;
mod pages;
mod runs;
mod table;
mod terminal;
mod text;
mod view;

use app::{LOG_KEY, PROCESS_KEY};
use navigation::{InstallPage, DEBUG_TAB, HELP_TAB};

pub use actions::Action;
pub use navigation::DEBUG_TOOLS;
pub use runs::{RunBackend, TaskOutcome};
pub use terminal::{run, run_with_actions, run_with_build, run_with_installers, run_with_runs};
pub use text::Text;
