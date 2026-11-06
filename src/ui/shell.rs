//! The menu shell: rounded, clipped container with a border and a soft drop
//! shadow. This is the visual that scales in and out.
//!
//! ```text
//! host (animated: Scale, Opacity, CenterPoint)
//! ├── shadow   (DropShadow shaped by a rounded-rect mask, outside the clip)
//! └── content  (clipped to the rounded rect)
//!     ├── background layers (backdrop → tint/gradient → overlay)
//!     └── border (1 px, white 10%)
//! ```

use windows::UI::Color;
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::background::Background;

/// Corner radius of the menu (DIPs).
pub const CORNER_RADIUS: f32 = 18.0;

const SHADOW_BLUR: f32 = 60.0;
const SHADOW_OFFSET_Y: f32 = 24.0;
const SHADOW_OPACITY: f32 = 0.45;

pub struct Shell {
    host: ContainerVisual,
    size: Vector2,
    pub background: Background,
}

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color {
        A: (a * 255.0) as u8,
        R: r,
        G: g,
        B: b,
    }
}

impl Shell {
    pub fn new(compositor: &Compositor, size: Vector2) -> Result<Self> {
        let radius = Vector2 {
            X: CORNER_RADIUS,
            Y: CORNER_RADIUS,
        };
        let rounded = || -> Result<_> {
            let g = compositor.CreateRoundedRectangleGeometry()?;
            g.SetSize(size)?;
            g.SetCornerRadius(radius)?;
            Ok(g)
        };

        let host = compositor.CreateContainerVisual()?;
        host.SetSize(size)?;

        // Shadow. A DropShadow normally takes its shape from the visual's
        // brush; here the shape comes from a mask: a rounded rectangle
        // rendered through a VisualSurface. The visual itself is transparent.
        let mask_shape = compositor.CreateSpriteShapeWithGeometry(&rounded()?)?;
        mask_shape.SetFillBrush(&compositor.CreateColorBrushWithColor(rgba(0, 0, 0, 1.0))?)?;
        let mask_visual = compositor.CreateShapeVisual()?;
        mask_visual.SetSize(size)?;
        mask_visual.Shapes()?.Append(&mask_shape)?;
        let mask_surface = compositor.CreateVisualSurface()?;
        mask_surface.SetSourceVisual(&mask_visual)?;
        mask_surface.SetSourceSize(size)?;

        let shadow = compositor.CreateDropShadow()?;
        shadow.SetBlurRadius(SHADOW_BLUR)?;
        shadow.SetOffset(Vector3 {
            X: 0.0,
            Y: SHADOW_OFFSET_Y,
            Z: 0.0,
        })?;
        shadow.SetColor(rgba(0, 0, 0, 1.0))?;
        shadow.SetOpacity(SHADOW_OPACITY)?;
        shadow.SetMask(&compositor.CreateSurfaceBrushWithSurface(&mask_surface)?)?;

        let shadow_visual = compositor.CreateSpriteVisual()?;
        shadow_visual.SetSize(size)?;
        shadow_visual.SetBrush(&compositor.CreateColorBrushWithColor(rgba(0, 0, 0, 0.0))?)?;
        shadow_visual.SetShadow(&shadow)?;
        host.Children()?.InsertAtTop(&shadow_visual)?;

        // Content, clipped to the rounded shape.
        let content = compositor.CreateContainerVisual()?;
        content.SetSize(size)?;
        content.SetClip(&compositor.CreateGeometricClipWithGeometry(&rounded()?)?)?;
        host.Children()?.InsertAtTop(&content)?;

        let background = Background::new(compositor, &content, size)?;

        // 1 px border, rgba(255,255,255,0.10). Inset by half a pixel so the
        // stroke sits fully inside the clip.
        let border_shape = compositor.CreateRoundedRectangleGeometry()?;
        border_shape.SetOffset(Vector2 { X: 0.5, Y: 0.5 })?;
        border_shape.SetSize(Vector2 {
            X: size.X - 1.0,
            Y: size.Y - 1.0,
        })?;
        border_shape.SetCornerRadius(Vector2 {
            X: CORNER_RADIUS - 0.5,
            Y: CORNER_RADIUS - 0.5,
        })?;
        let stroke = compositor.CreateSpriteShapeWithGeometry(&border_shape)?;
        stroke.SetStrokeBrush(&compositor.CreateColorBrushWithColor(rgba(255, 255, 255, 0.10))?)?;
        stroke.SetStrokeThickness(1.0)?;
        let border = compositor.CreateShapeVisual()?;
        border.SetSize(size)?;
        border.Shapes()?.Append(&stroke)?;
        content.Children()?.InsertAtTop(&border)?;

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
