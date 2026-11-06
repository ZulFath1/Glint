//! The menu shell: rounded, clipped container. This is the visual that
//! scales in and out.
//!
//! ```text
//! host (animated: Scale, Opacity, CenterPoint)
//! └── content  (clipped to the rounded rect)
//!     └── background layers (backdrop → tint/gradient → overlay → sheen →
//!         border or glass rim), see background.rs
//! ```
//!
//! No drop shadow: the owner found it looked odd over light backgrounds
//! (a deliberate change from CLAUDE.md section 5).

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::core::{Interface, Result};
use windows_numerics::Vector2;

use super::background::Background;

/// Corner radius of the menu (DIPs).
pub const CORNER_RADIUS: f32 = 18.0;

pub struct Shell {
    host: ContainerVisual,
    size: Vector2,
    pub background: Background,
}

impl Shell {
    pub fn new(compositor: &Compositor, size: Vector2) -> Result<Self> {
        let host = compositor.CreateContainerVisual()?;
        host.SetSize(size)?;

        // Content, clipped to the rounded shape.
        let clip_shape = compositor.CreateRoundedRectangleGeometry()?;
        clip_shape.SetSize(size)?;
        clip_shape.SetCornerRadius(Vector2 {
            X: CORNER_RADIUS,
            Y: CORNER_RADIUS,
        })?;
        let content = compositor.CreateContainerVisual()?;
        content.SetSize(size)?;
        content.SetClip(&compositor.CreateGeometricClipWithGeometry(&clip_shape)?)?;
        host.Children()?.InsertAtTop(&content)?;

        // Background layers, sheen and the border / glass rim.
        let background = Background::new(compositor, &content, size, CORNER_RADIUS)?;

        Ok(Self {
            host,
            size,
            background,
        })
    }

    pub fn host(&self) -> &ContainerVisual {
        &self.host
    }

    pub fn host_visual(&self) -> Result<Visual> {
        self.host.cast()
    }

    pub fn size(&self) -> Vector2 {
        self.size
    }
}
