//! Versioned Husk plugin platform and plugin-owned editor presentation resources.
//!
//! The registry owns discovery, compatibility, status, activation, and hot reload.
//! [`Runtime`] owns the Husk VM and implements the Rust side of the host boundary.
//! Feature modules model resources such as panels, overlays, gutter signs, decorations,
//! workspaces, window bars, and permitted child processes; the editor remains the sole
//! authority that applies their requested effects.
//!
//! `host_api.json` is the machine-readable compatibility contract. Rust request enums,
//! semantic declarations, and prose documentation must remain consistent with that
//! schema, but must not independently redefine its version.

mod api;
pub mod decoration;
pub mod gutter;
pub mod location;
pub(crate) mod markdown;
mod metadata;
pub mod overlay;
pub mod panel;
pub mod process;
mod registry;
mod runtime;
mod text_link;
pub mod timer_stats;
pub mod window_bar;
pub mod workspace;

pub use decoration::{Decoration, DecorationAnchor, DecorationManager};
pub use gutter::{GutterSign, GutterSignManager};
pub use location::{LocationColumnEncoding, OpenLocationTarget, PluginLocation};
pub use metadata::PluginMetadata;
pub use overlay::{OverlayAlignment, OverlayConfig, OverlayManager};
pub use panel::{
    PanelConfig, PanelManager, PanelRow, PanelRowKind, PanelSegment, PanelSide, TextPanelBlock,
    TextPanelBlockFormat, TextPanelBlockKind, TextPanelComposerConfig, TextPanelHeaderAction,
    TextPanelStatus,
};
pub use registry::{PluginRegistry, PluginStatus, RED_HOST_API_VERSION};
pub use runtime::{poll_timer_callbacks, RegisteredPluginCommand, Runtime};
#[cfg(test)]
pub(crate) use text_link::TextPanelFileLocation;
pub(crate) use text_link::TextPanelLinkTarget;
pub use window_bar::{
    RenderedWindowBar, WindowBarConfig, WindowBarEdge, WindowBarHitRegion, WindowBarManager,
    WindowBarOverflow, WindowBarSegment, WindowBarSemanticStyle, WindowBarStyle,
};
pub use workspace::{WorkspaceConfig, WorkspaceManager, WorkspaceModel, WorkspaceRow};
