//! The menu shell: a rounded rectangle with a hairline border.
//!
//! Milestone 1 only draws a flat background. Milestone 2 replaces the fill
//! with the background modes and adds the shadow and animations.

use windows::UI::Color;
use windows::UI::Composition::{Compositor, ContainerVisual};
use windows::core::Result;
use windows_numerics::Vector2;

/// Corner radius of the menu (DIPs).
pub const CORNER_RADIUS: f32 = 18.0;

pub struct Shell {
    visual: ContainerVisual,
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

        // Container clipped to the rounded shape; everything goes inside it.
        let visual = compositor.CreateContainerVisual()?;
        visual.SetSize(size)?;
        let clip_shape = compositor.CreateRoundedRectangleGeometry()?;
        clip_shape.SetSize(size)?;
        clip_shape.SetCornerRadius(radius)?;
        visual.SetClip(&compositor.CreateGeometricClipWithGeometry(&clip_shape)?)?;

        // Background fill: the "plain" preset color for now.
        let background = compositor.CreateSpriteVisual()?;
        background.SetSize(size)?;
        background.SetBrush(&compositor.CreateColorBrushWithColor(rgba(0x1e, 0x20, 0x26, 1.0))?)?;
        visual.Children()?.InsertAtTop(&background)?;

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
        visual.Children()?.InsertAtTop(&border)?;

        Ok(Self { visual })
    }

    pub fn visual(&self) -> &ContainerVisual {
        &self.visual
    }
}
