mod loader;
mod metadata;
pub mod overlay;
pub mod panel;
mod registry;
mod runtime;
pub mod timer_stats;

pub use metadata::PluginMetadata;
pub use overlay::{OverlayAlignment, OverlayConfig, OverlayManager};
pub use panel::{PanelConfig, PanelManager, PanelRow, PanelRowKind, PanelSegment, PanelSide};
pub use registry::PluginRegistry;
pub use runtime::{poll_timer_callbacks, Runtime};
