//! The menu shell: rounded, clipped container. This is the visual that
//! scales in and out.
//!
//! ```text
//! host (animated: Scale, Opacity, CenterPoint)
//! ├── hard shadow (terminal themes only: offset 8, 8, no blur)
//! └── content  (clipped to the rounded rect)
//!     ├── background layers (backdrop → tint/gradient → overlay → sheen),
//!     │   see background.rs
//!     ├── body: search box, grid, footer, power strip
//!     └── border or glass rim (always on top)
//! ```
//!
//! No soft drop shadow: the owner found it looked odd over light backgrounds
//! (a deliberate change from CLAUDE.md section 5). Gruvbox has a hard,
//! unblurred offset shadow instead, which fits inside the window margin.

use windows::UI::Composition::{
    CompositionObject, CompositionRoundedRectangleGeometry, Compositor, ContainerVisual, Visual,
};
use windows::core::{Interface, Result};
use windows_numerics::Vector2;

use super::anim::Kind;
use super::background::Background;
use super::layout::Rect;
use super::shape::RoundRect;
use crate::theme::Theme;

/// The hard shadow's offset (DIPs).
const SHADOW_OFFSET: f32 = 8.0;

/// Corner radius of the menu (DIPs).
pub const CORNER_RADIUS: f32 = 18.0;

pub struct Shell {
    host: ContainerVisual,
    content: ContainerVisual,
    clip_shape: CompositionRoundedRectangleGeometry,
    body: ContainerVisual,
    shadow: RoundRect,
    pub background: Background,
}

impl Shell {
    pub fn new(compositor: &Compositor, size: Vector2) -> Result<Self> {
        let host = compositor.CreateContainerVisual()?;
        host.SetSize(size)?;
        let shadow = RoundRect::new(compositor, CORNER_RADIUS, 0.0)?;
        shadow.show(false)?;
        host.Children()?.InsertAtTop(&shadow.visual)?;

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

        // The menu's content sits between the background and the border.
        let body = compositor.CreateContainerVisual()?;
        body.SetSize(size)?;
        content
            .Children()?
            .InsertBelow(&body, background.border_visual())?;

        Ok(Self {
            host,
            content,
            clip_shape,
            body,
            shadow,
            background,
        })
    }

    pub fn host(&self) -> &ContainerVisual {
        &self.host
    }

    pub fn host_visual(&self) -> Result<Visual> {
        self.host.cast()
    }

    /// Where the search box, grid and footer go.
    pub fn body(&self) -> &ContainerVisual {
        &self.body
    }

    /// Everything that changes when the menu's height changes (the
    /// background layers included), for the 220 ms layout animation.
    pub fn motion_objects(&self) -> Result<Vec<(CompositionObject, Kind)>> {
        let mut list = vec![
            (self.host.cast()?, Kind::Visual),
            (self.content.cast()?, Kind::Visual),
            (self.body.cast()?, Kind::Visual),
            (self.clip_shape.cast()?, Kind::Geometry),
        ];
        list.extend(self.background.motion_objects()?);
        list.extend(self.shadow.motion_objects()?);
        Ok(list)
    }

    /// The content changed size (a different number of pins, search
    /// results). Inside `LayoutMotion::animate` this glides.
    pub fn set_size(&self, size: Vector2) -> Result<()> {
        self.host.SetSize(size)?;
        self.content.SetSize(size)?;
        self.body.SetSize(size)?;
        self.clip_shape.SetSize(size)?;
        self.shadow
            .set_rect(Rect::new(SHADOW_OFFSET, SHADOW_OFFSET, size.X, size.Y))?;
        self.background.set_size(size)
    }

    /// A new theme: corner radius and the hard shadow. (The background's
    /// colors and border come with `Background::apply`.)
    pub fn set_theme(&self, theme: &Theme, size: Vector2) -> Result<()> {
        let r = theme.shape.menu_radius;
        self.clip_shape.SetCornerRadius(Vector2 { X: r, Y: r })?;
        self.shadow.set_radius(r);
        match theme.palette.shadow {
            Some(c) => {
                self.shadow.set_fill(super::gfx::color(c, 1.0))?;
                self.shadow.show(true)?;
            }
            None => self.shadow.show(false)?,
        }
        self.set_size(size)
    }
}
