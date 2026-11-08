//! A rounded rectangle drawn by the compositor itself (no painting): used for
//! hover highlights, focus rings, the search box frame and the power strip.

use windows::UI::Color;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionObject, CompositionRoundedRectangleGeometry, Compositor,
    ShapeVisual,
};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::anim::Kind;
use super::layout::Rect;

pub struct RoundRect {
    pub visual: ShapeVisual,
    geometry: CompositionRoundedRectangleGeometry,
    fill: CompositionColorBrush,
    stroke: CompositionColorBrush,
    radius: std::cell::Cell<f32>,
    stroke_width: std::cell::Cell<f32>,
    shape: windows::UI::Composition::CompositionSpriteShape,
}

impl RoundRect {
    /// `stroke_width` 0 = no outline.
    pub fn new(c: &Compositor, radius: f32, stroke_width: f32) -> Result<Self> {
        let geometry = c.CreateRoundedRectangleGeometry()?;
        let shape = c.CreateSpriteShapeWithGeometry(&geometry)?;
        let fill = c.CreateColorBrush()?;
        let stroke = c.CreateColorBrush()?;
        shape.SetFillBrush(&fill)?;
        if stroke_width > 0.0 {
            shape.SetStrokeBrush(&stroke)?;
            shape.SetStrokeThickness(stroke_width)?;
        }
        let visual = c.CreateShapeVisual()?;
        visual.Shapes()?.Append(&shape)?;
        Ok(Self {
            visual,
            geometry,
            fill,
            stroke,
            radius: std::cell::Cell::new(radius),
            stroke_width: std::cell::Cell::new(stroke_width),
            shape,
        })
    }

    pub fn set_fill(&self, color: Color) -> Result<()> {
        self.fill.SetColor(color)
    }

    pub fn set_stroke(&self, color: Color) -> Result<()> {
        self.stroke.SetColor(color)
    }

    /// Moves and sizes it. The outline sits fully inside the rectangle.
    pub fn set_rect(&self, r: Rect) -> Result<()> {
        self.visual.SetOffset(Vector3 {
            X: r.x,
            Y: r.y,
            Z: 0.0,
        })?;
        self.visual.SetSize(r.size())?;
        let width = self.stroke_width.get();
        let inset = width / 2.0;
        self.geometry.SetOffset(Vector2 { X: inset, Y: inset })?;
        self.geometry.SetSize(Vector2 {
            X: (r.w - width).max(0.0),
            Y: (r.h - width).max(0.0),
        })?;
        let radius = (self.radius.get() - inset).max(0.0);
        self.geometry.SetCornerRadius(Vector2 {
            X: radius,
            Y: radius,
        })
    }

    /// Changes the outline width (takes effect on the next `set_rect`). Only
    /// for shapes created with an outline.
    pub fn set_stroke_width(&self, width: f32) -> Result<()> {
        self.stroke_width.set(width);
        self.shape.SetStrokeThickness(width)
    }

    /// Changes the corner radius (takes effect on the next `set_rect`).
    pub fn set_radius(&self, radius: f32) {
        self.radius.set(radius);
    }

    pub fn show(&self, visible: bool) -> Result<()> {
        self.visual.SetIsVisible(visible)
    }

    /// The visual and its shape, for the layout animation (so a size change
    /// glides).
    pub fn motion_objects(&self) -> Result<Vec<(CompositionObject, Kind)>> {
        Ok(vec![
            (self.visual.cast()?, Kind::Visual),
            (self.geometry.cast()?, Kind::Geometry),
        ])
    }
}
