//! The music card (CLAUDE.md sections 5 and 8): cover, title, artist,
//! Previous / Play-Pause / Next, a seek bar, and "Playing from <app>" with
//! a "Switch source" pill when 2+ apps have media.
//!
//! ```text
//! outer   placed by the layout (glides), cut off at the footer's top
//! └── inner   popped in by the open animation
//!     ├── panel (rounded, dark), hover highlight
//!     ├── cover (rounded clip), content (text, buttons, source row)
//!     ├── bar: track, fill (scaled by DWM), thumb (follows the fill)
//!     └── elapsed time, total time
//! ```
//!
//! The seek bar moves by itself: we start one linear animation from "now"
//! to the end of the track, and DWM plays it. Only the elapsed-time text
//! needs repainting (at most 4 checks per second, a repaint when the text
//! changes), and only while the menu is open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use windows::Foundation::TimeSpan;
use windows::UI::Composition::{
    CompositionEasingFunction, CompositionObject, Compositor, ContainerVisual,
    ScalarKeyFrameAnimation, SpriteVisual, Visual,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1, ID2D1DeviceContext,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    IDWriteTextFormat,
};
use windows::core::{Interface, Result, h};
use windows_numerics::{Vector2, Vector3};

use super::anim::{Kind, ms};
use super::gfx::{self, Canvas, Gfx, TEXT_PRIMARY, TEXT_SECONDARY};
use super::layout::{self, COVER, CardParts, Layout, Music, Rect, THUMB, TRACK_HEIGHT};
use super::shape::RoundRect;
use crate::config::Rgb;
use crate::media::{Info, MediaCommand, MusicView, format_time};

const RADIUS: f32 = 14.0;
const FILL_ALPHA: f32 = 0.28;
const BORDER_ALPHA: f32 = 0.08;
const COVER_RADIUS: f32 = 8.0;
const HOVER_ALPHA: f32 = 0.08;
const BUTTON_RADIUS: f32 = 10.0;
/// Buttons the app doesn't support are drawn this faint.
const DIM_ALPHA: f32 = 0.32;
const TRACK_ALPHA: f32 = 0.20;
const PILL_ALPHA: f32 = 0.10;
const SOURCE_ICON: f32 = 16.0;
/// The longest a single seek animation runs (Composition caps durations).
const MAX_ANIMATION_TICKS: i64 = 24 * 3600 * 10_000_000;

const PREVIOUS_GLYPH: &str = "\u{E892}";
const PLAY_GLYPH: &str = "\u{E768}";
const PAUSE_GLYPH: &str = "\u{E769}";
const NEXT_GLYPH: &str = "\u{E893}";
const MUSIC_GLYPH: &str = "\u{E8D6}";

/// Something on the card you can click or focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardPart {
    Previous,
    PlayPause,
    Next,
    Seek,
    Source,
}

impl CardPart {
    const BUTTONS: [CardPart; 3] = [CardPart::Previous, CardPart::PlayPause, CardPart::Next];

    /// What clicking it (or Enter on it) does. The seek bar is dragged
    /// instead.
    pub fn command(self) -> Option<MediaCommand> {
        match self {
            CardPart::Previous => Some(MediaCommand::Previous),
            CardPart::PlayPause => Some(MediaCommand::PlayPause),
            CardPart::Next => Some(MediaCommand::Next),
            CardPart::Source => Some(MediaCommand::SwitchSource),
            CardPart::Seek => None,
        }
    }
}

/// What the content canvas shows, so it is only repainted on a change.
#[derive(Debug, Clone, PartialEq)]
struct Painted {
    title: String,
    artist: String,
    playing: bool,
    enabled: [bool; 3],
    sources: bool,
    source_name: String,
    has_icon: bool,
}

fn white(alpha: f32) -> windows::UI::Color {
    gfx::color(Rgb::new(255, 255, 255), alpha)
}

fn draw_text(
    ctx: &ID2D1DeviceContext,
    gfx: &Gfx,
    text: &str,
    format: &IDWriteTextFormat,
    r: Rect,
    align: DWRITE_TEXT_ALIGNMENT,
    (color, alpha): (Rgb, f32),
) -> Result<()> {
    let layout = gfx.text_layout(text, format, r.size(), align, true)?;
    let brush = gfx::brush(ctx, color, alpha)?;
    // SAFETY: draws a live text layout with a live brush.
    unsafe {
        ctx.DrawTextLayout(
            Vector2 { X: r.x, Y: r.y },
            &layout,
            &brush,
            gfx::TEXT_OPTIONS,
        )
    };
    Ok(())
}

pub struct MusicCard {
    outer: ContainerVisual,
    inner: ContainerVisual,
    panel: RoundRect,
    hover: RoundRect,
    cover: Canvas,
    content: Canvas,
    bar: ContainerVisual,
    track: SpriteVisual,
    fill: SpriteVisual,
    elapsed: Canvas,
    total: Canvas,
    seek: ScalarKeyFrameAnimation,
    /// Where the card is (menu coordinates) and how wide its parts are.
    rect: Cell<Rect>,
    view: RefCell<Option<MusicView>>,
    source_name: RefCell<String>,
    source_icon: RefCell<Option<ID2D1Bitmap1>>,
    painted: RefCell<Option<Painted>>,
    /// The info whose cover is painted (compared by pointer).
    painted_cover: RefCell<Option<Rc<Info>>>,
    /// The two time labels as painted.
    times: RefCell<(String, String)>,
    /// While the seek bar is dragged: where (0–1).
    drag: Cell<Option<f32>>,
    title_format: IDWriteTextFormat,
    artist_format: IDWriteTextFormat,
    time_format: IDWriteTextFormat,
    pill_format: IDWriteTextFormat,
    small_glyph: IDWriteTextFormat,
    big_glyph: IDWriteTextFormat,
}

impl MusicCard {
    /// Creates the card (hidden). `footer` is the footer's background: the
    /// card is cut off at its top, so it never draws over the footer while
    /// the menu glides to a new height.
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        parent: &ContainerVisual,
        footer: &Visual,
        accent: Rgb,
        l: &Layout,
    ) -> Result<Self> {
        let outer = c.CreateContainerVisual()?;
        outer.SetOpacity(0.0)?;
        let clip = c.CreateInsetClip()?;
        let cut = c.CreateExpressionAnimationWithExpression(h!(
            "Max(0, card.Offset.Y + card.Size.Y - footer.Offset.Y)"
        ))?;
        cut.SetReferenceParameter(h!("card"), &outer)?;
        cut.SetReferenceParameter(h!("footer"), footer)?;
        clip.StartAnimation(h!("BottomInset"), &cut)?;
        outer.SetClip(&clip)?;
        parent.Children()?.InsertAtTop(&outer)?;

        let inner = c.CreateContainerVisual()?;
        outer.Children()?.InsertAtTop(&inner)?;

        let panel = RoundRect::new(c, RADIUS, 1.0)?;
        panel.set_fill(gfx::color(Rgb::new(0, 0, 0), FILL_ALPHA))?;
        panel.set_stroke(white(BORDER_ALPHA))?;
        let hover = RoundRect::new(c, BUTTON_RADIUS, 0.0)?;
        hover.set_fill(white(HOVER_ALPHA))?;
        hover.show(false)?;

        let cover_size = Vector2 { X: COVER, Y: COVER };
        let cover = Canvas::new(c, gfx, cover_size)?;
        let rounded = c.CreateRoundedRectangleGeometry()?;
        rounded.SetSize(cover_size)?;
        rounded.SetCornerRadius(Vector2 {
            X: COVER_RADIUS,
            Y: COVER_RADIUS,
        })?;
        cover
            .visual
            .SetClip(&c.CreateGeometricClipWithGeometry(&rounded)?)?;
        let content = Canvas::new(c, gfx, Self::content_size(l))?;

        // The seek bar: the fill is scaled from its left edge; the thumb's
        // x follows the fill's scale (worked out by DWM every frame).
        let bar = c.CreateContainerVisual()?;
        let track = c.CreateSpriteVisual()?;
        track.SetBrush(&c.CreateColorBrushWithColor(white(TRACK_ALPHA))?)?;
        let fill = c.CreateSpriteVisual()?;
        fill.SetBrush(&c.CreateColorBrushWithColor(gfx::color(accent, 1.0))?)?;
        fill.SetScale(Vector3 {
            X: 0.0,
            Y: 1.0,
            Z: 1.0,
        })?;
        let dot = c.CreateEllipseGeometry()?;
        dot.SetCenter(Vector2 {
            X: THUMB / 2.0,
            Y: THUMB / 2.0,
        })?;
        dot.SetRadius(Vector2 {
            X: THUMB / 2.0,
            Y: THUMB / 2.0,
        })?;
        let dot_shape = c.CreateSpriteShapeWithGeometry(&dot)?;
        dot_shape.SetFillBrush(&c.CreateColorBrushWithColor(gfx::color(TEXT_PRIMARY, 1.0))?)?;
        let thumb = c.CreateShapeVisual()?;
        thumb.SetSize(Vector2 { X: THUMB, Y: THUMB })?;
        thumb.Shapes()?.Append(&dot_shape)?;
        // The thumb's whole position comes from this formula: x = the end of
        // the fill, y = centered on the track. It is never set directly,
        // because setting a property by hand detaches its animation (that
        // left the thumb stuck at the start of the bar).
        let follow = c.CreateExpressionAnimationWithExpression(h!(
            "Vector3(fill.Scale.X * track.Size.X - 6, track.Offset.Y + track.Size.Y / 2 - 6, 0)"
        ))?;
        follow.SetReferenceParameter(h!("fill"), &fill)?;
        follow.SetReferenceParameter(h!("track"), &track)?;
        thumb.StartAnimation(h!("Offset"), &follow)?;
        for v in [&track.cast::<Visual>()?, &fill.cast()?, &thumb.cast()?] {
            bar.Children()?.InsertAtTop(v)?;
        }

        let time_size = l.card_parts.elapsed.size();
        let elapsed = Canvas::new(c, gfx, time_size)?;
        let total = Canvas::new(c, gfx, time_size)?;

        for v in [
            &panel.visual.cast::<Visual>()?,
            &hover.visual.cast()?,
            &cover.visual.cast()?,
            &content.visual.cast()?,
            &bar.cast()?,
            &elapsed.visual.cast()?,
            &total.visual.cast()?,
        ] {
            inner.Children()?.InsertAtTop(v)?;
        }

        // The seek bar moves at a steady speed: linear easing.
        let linear: CompositionEasingFunction = c.CreateLinearEasingFunction()?.cast()?;
        let seek = c.CreateScalarKeyFrameAnimation()?;
        seek.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        seek.InsertKeyFrameWithEasingFunction(1.0, 1.0, &linear)?;

        let card = Self {
            outer,
            inner,
            panel,
            hover,
            cover,
            content,
            bar,
            track,
            fill,
            elapsed,
            total,
            seek,
            rect: Cell::new(l.card),
            view: RefCell::new(None),
            source_name: RefCell::new(String::new()),
            source_icon: RefCell::new(None),
            painted: RefCell::new(None),
            painted_cover: RefCell::new(None),
            times: RefCell::new((String::new(), String::new())),
            drag: Cell::new(None),
            title_format: gfx.text_format(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            artist_format: gfx.text_format(12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            time_format: gfx.text_format(11.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            pill_format: gfx.text_format(11.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            small_glyph: gfx.icon_format(14.0)?,
            big_glyph: gfx.icon_format(20.0)?,
        };
        card.place(gfx, l.card)?;
        Ok(card)
    }

    /// The content canvas always has room for the source row.
    fn content_size(l: &Layout) -> Vector2 {
        Vector2 {
            X: l.card.w,
            Y: layout::card_height(Music::CardWithSources),
        }
    }

    /// The parts, relative to the card (the source row is always laid out;
    /// it only shows with 2+ sources).
    fn parts(&self) -> CardParts {
        layout::card_parts(self.rect.get().w, true)
    }

    /// The container that fades in and out (the card shows or hides).
    pub fn visual(&self) -> Result<Visual> {
        self.outer.cast()
    }

    /// The container the open animation pops in.
    pub fn animated(&self) -> Result<Visual> {
        self.inner.cast()
    }

    /// What glides when the menu's layout changes.
    pub fn motion_objects(&self) -> Result<Vec<(CompositionObject, Kind)>> {
        let mut list = vec![(self.outer.cast()?, Kind::Visual)];
        list.extend(self.panel.motion_objects()?);
        Ok(list)
    }

    /// New DPI scale (or first use): resize and repaint everything.
    pub fn refresh(&self, gfx: &Gfx, l: &Layout) -> Result<()> {
        let p = l.card_parts;
        self.cover.resize(gfx, Vector2 { X: COVER, Y: COVER })?;
        self.content.resize(gfx, Self::content_size(l))?;
        self.elapsed.resize(gfx, p.elapsed.size())?;
        self.total.resize(gfx, p.total.size())?;
        self.move_to(gfx, l)?;
        self.painted.borrow_mut().take();
        self.painted_cover.borrow_mut().take();
        *self.times.borrow_mut() = Default::default();
        self.repaint(gfx)
    }

    /// Follows the layout. A hidden card stays where it was, so it fades
    /// out in place.
    pub fn move_to(&self, gfx: &Gfx, l: &Layout) -> Result<()> {
        if l.music == Music::Hidden {
            return Ok(());
        }
        self.place(gfx, l.card)
    }

    fn place(&self, gfx: &Gfx, card: Rect) -> Result<()> {
        self.rect.set(card);
        let s = gfx.scale();
        let size = card.size();
        self.outer.SetOffset(Vector3 {
            X: gfx::snap(card.x, s),
            Y: gfx::snap(card.y, s),
            Z: 0.0,
        })?;
        self.outer.SetSize(size)?;
        self.inner.SetSize(size)?;
        self.inner.SetCenterPoint(Vector3 {
            X: card.w / 2.0,
            Y: card.h / 2.0,
            Z: 0.0,
        })?;
        self.panel.set_rect(Rect::new(0.0, 0.0, card.w, card.h))?;

        let p = self.parts();
        self.cover.set_offset(gfx, p.cover.x, p.cover.y)?;
        self.content.set_offset(gfx, 0.0, 0.0)?;
        self.elapsed.set_offset(gfx, p.elapsed.x, p.elapsed.y)?;
        self.total.set_offset(gfx, p.total.x, p.total.y)?;
        let row = p.elapsed;
        self.bar.SetOffset(Vector3 {
            X: gfx::snap(p.track.x, s),
            Y: gfx::snap(row.y, s),
            Z: 0.0,
        })?;
        self.bar.SetSize(Vector2 {
            X: p.track.w,
            Y: row.h,
        })?;
        let track_y = (row.h - TRACK_HEIGHT) / 2.0;
        for v in [&self.track, &self.fill] {
            v.SetOffset(Vector3 {
                X: 0.0,
                Y: track_y,
                Z: 0.0,
            })?;
            v.SetSize(Vector2 {
                X: p.track.w,
                Y: TRACK_HEIGHT,
            })?;
        }
        // The thumb follows by itself (its expression); never set it here.
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Content
    // -----------------------------------------------------------------------

    /// New content. `None` keeps the old picture (the card is fading out).
    /// Only what changed is repainted.
    pub fn set_view(
        &self,
        gfx: &Gfx,
        view: Option<MusicView>,
        source_name: String,
        source_icon: Option<ID2D1Bitmap1>,
    ) -> Result<()> {
        let Some(view) = view else {
            self.view.borrow_mut().take();
            return Ok(());
        };
        *self.view.borrow_mut() = Some(view);
        *self.source_name.borrow_mut() = source_name;
        *self.source_icon.borrow_mut() = source_icon;
        self.repaint(gfx)
    }

    pub fn view(&self) -> Option<MusicView> {
        self.view.borrow().clone()
    }

    pub fn is_playing(&self) -> bool {
        self.view.borrow().as_ref().is_some_and(|v| v.playing)
    }

    /// Can this part be used right now? (The app may not support it.)
    pub fn is_enabled(&self, part: CardPart) -> bool {
        let view = self.view.borrow();
        let Some(v) = view.as_ref() else {
            return false;
        };
        match part {
            CardPart::Previous => v.can_previous,
            CardPart::PlayPause => v.can_play_pause,
            CardPart::Next => v.can_next,
            CardPart::Seek => v.can_seek,
            CardPart::Source => v.sources >= 2,
        }
    }

    fn repaint(&self, gfx: &Gfx) -> Result<()> {
        let Some(view) = self.view() else {
            return Ok(());
        };
        let source_name = self.source_name.borrow().clone();
        let title = if view.info.title.is_empty() {
            source_name.clone()
        } else {
            view.info.title.clone()
        };
        let painted = Painted {
            title,
            artist: view.info.artist.clone(),
            playing: view.playing,
            enabled: [view.can_previous, view.can_play_pause, view.can_next],
            sources: view.sources >= 2,
            source_name,
            has_icon: self.source_icon.borrow().is_some(),
        };
        if self.painted.borrow().as_ref() != Some(&painted) {
            self.paint_content(gfx, &painted)?;
            *self.painted.borrow_mut() = Some(painted);
        }
        let same_cover = self
            .painted_cover
            .borrow()
            .as_ref()
            .is_some_and(|i| Rc::ptr_eq(i, &view.info));
        if !same_cover {
            self.paint_cover(gfx, &view.info)?;
            *self.painted_cover.borrow_mut() = Some(view.info.clone());
        }
        self.paint_times(gfx)
    }

    fn paint_content(&self, gfx: &Gfx, painted: &Painted) -> Result<()> {
        let p = self.parts();
        let icon = self.source_icon.borrow().clone();
        self.content.draw(gfx, |ctx| {
            if painted.sources {
                let brush = gfx::brush(ctx, Rgb::new(255, 255, 255), PILL_ALPHA)?;
                let radius = p.pill.h / 2.0;
                // SAFETY: plain drawing call with a local struct.
                unsafe {
                    ctx.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: gfx::d2d_rect(p.pill),
                            radiusX: radius,
                            radiusY: radius,
                        },
                        &brush,
                    )
                };
            }
            gfx::with_glow(ctx, |ctx| {
                let lead = DWRITE_TEXT_ALIGNMENT_LEADING;
                let center = DWRITE_TEXT_ALIGNMENT_CENTER;
                draw_text(
                    ctx,
                    gfx,
                    &painted.title,
                    &self.title_format,
                    p.title,
                    lead,
                    (TEXT_PRIMARY, 1.0),
                )?;
                draw_text(
                    ctx,
                    gfx,
                    &painted.artist,
                    &self.artist_format,
                    p.artist,
                    lead,
                    (TEXT_SECONDARY, 1.0),
                )?;
                let play = if painted.playing {
                    PAUSE_GLYPH
                } else {
                    PLAY_GLYPH
                };
                let glyphs = [
                    (PREVIOUS_GLYPH, &self.small_glyph),
                    (play, &self.big_glyph),
                    (NEXT_GLYPH, &self.small_glyph),
                ];
                for (i, (glyph, format)) in glyphs.into_iter().enumerate() {
                    let alpha = if painted.enabled[i] { 1.0 } else { DIM_ALPHA };
                    draw_text(
                        ctx,
                        gfx,
                        glyph,
                        format,
                        p.buttons[i],
                        center,
                        (TEXT_PRIMARY, alpha),
                    )?;
                }
                if painted.sources {
                    let mut text_x = p.source.x;
                    if let Some(icon) = &icon {
                        let top = p.source.y + (p.source.h - SOURCE_ICON) / 2.0;
                        let dest = gfx::d2d_rect(Rect::new(text_x, top, SOURCE_ICON, SOURCE_ICON));
                        // SAFETY: draws a live bitmap into a local rect.
                        unsafe {
                            ctx.DrawBitmap(
                                icon,
                                Some(&dest),
                                1.0,
                                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                                None,
                                None,
                            )
                        };
                        text_x += SOURCE_ICON + 6.0;
                    }
                    let text = Rect::new(
                        text_x,
                        p.source.y,
                        (p.source.right() - text_x).max(0.0),
                        p.source.h,
                    );
                    draw_text(
                        ctx,
                        gfx,
                        &format!("Playing from {}", painted.source_name),
                        &self.artist_format,
                        text,
                        lead,
                        (TEXT_SECONDARY, 1.0),
                    )?;
                    draw_text(
                        ctx,
                        gfx,
                        "Switch source",
                        &self.pill_format,
                        p.pill,
                        center,
                        (TEXT_PRIMARY, 1.0),
                    )?;
                }
                Ok(())
            })
        })
    }

    fn paint_cover(&self, gfx: &Gfx, info: &Info) -> Result<()> {
        let bitmap = match &info.cover {
            Some(c) => gfx
                .bitmap_from_pixels(c.px, c.px, &c.pixels)
                .inspect_err(|e| log_warn!("album cover bitmap: {e}"))
                .ok(),
            None => None,
        };
        let full = Rect::new(0.0, 0.0, COVER, COVER);
        self.cover.draw(gfx, |ctx| {
            if let Some(bitmap) = &bitmap {
                // SAFETY: draws a live bitmap into a local rect.
                unsafe {
                    ctx.DrawBitmap(
                        bitmap,
                        Some(&gfx::d2d_rect(full)),
                        1.0,
                        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                        None,
                        None,
                    )
                };
                return Ok(());
            }
            // No cover: a music note on a soft square.
            let bg = gfx::brush(ctx, Rgb::new(255, 255, 255), 0.10)?;
            // SAFETY: plain drawing call with a local rect.
            unsafe { ctx.FillRectangle(&gfx::d2d_rect(full), &bg) };
            draw_text(
                ctx,
                gfx,
                MUSIC_GLYPH,
                &self.big_glyph,
                full,
                DWRITE_TEXT_ALIGNMENT_CENTER,
                (TEXT_SECONDARY, 1.0),
            )
        })
    }

    /// The position shown: where the bar is dragged, or where playback is.
    fn shown_position(&self) -> Option<(i64, i64)> {
        let view = self.view.borrow();
        let v = view.as_ref()?;
        let position = match self.drag.get() {
            Some(f) => (f64::from(f) * v.length as f64) as i64,
            None => v.position_now(),
        };
        Some((position, v.length))
    }

    /// Repaints the time labels if their text changed.
    fn paint_times(&self, gfx: &Gfx) -> Result<()> {
        let Some((position, length)) = self.shown_position() else {
            return Ok(());
        };
        let (elapsed, total) = if length > 0 {
            (format_time(position), format_time(length))
        } else {
            // Live streams and some players don't report a length.
            (String::new(), String::new())
        };
        let mut times = self.times.borrow_mut();
        if times.0 != elapsed {
            self.paint_time(gfx, &self.elapsed, &elapsed, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            times.0 = elapsed;
        }
        if times.1 != total {
            self.paint_time(gfx, &self.total, &total, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            times.1 = total;
        }
        Ok(())
    }

    fn paint_time(
        &self,
        gfx: &Gfx,
        canvas: &Canvas,
        text: &str,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> Result<()> {
        let size = canvas.size();
        canvas.draw(gfx, |ctx| {
            gfx::with_glow(ctx, |ctx| {
                draw_text(
                    ctx,
                    gfx,
                    text,
                    &self.time_format,
                    Rect::new(0.0, 0.0, size.X, size.Y),
                    align,
                    (TEXT_SECONDARY, 1.0),
                )
            })
        })
    }

    /// The menu is open: refresh the elapsed time (called up to 4 × a
    /// second while playing; repaints only when the text changes).
    pub fn tick(&self, gfx: &Gfx) -> Result<()> {
        self.paint_times(gfx)
    }

    // -----------------------------------------------------------------------
    // Seek bar
    // -----------------------------------------------------------------------

    /// Puts the bar where playback is and, if `moving` (menu open and
    /// playing), lets DWM move it to the end of the track by itself.
    pub fn sync_seek(&self, moving: bool) -> Result<()> {
        self.fill.StopAnimation(h!("Scale.X"))?;
        let Some((position, length)) = self.shown_position() else {
            return self.set_fraction(0.0);
        };
        if length <= 0 {
            return self.set_fraction(0.0);
        }
        self.set_fraction((position as f64 / length as f64) as f32)?;
        let (playing, rate) = self
            .view
            .borrow()
            .as_ref()
            .map_or((false, 1.0), |v| (v.playing, v.rate));
        if !moving || !playing || self.drag.get().is_some() || position >= length {
            return Ok(());
        }
        let remaining = (((length - position) as f64) / rate) as i64;
        self.seek.SetDuration(TimeSpan {
            Duration: remaining.clamp(ms(1).Duration, MAX_ANIMATION_TICKS),
        })?;
        self.fill.StartAnimation(h!("Scale.X"), &self.seek)
    }

    fn set_fraction(&self, f: f32) -> Result<()> {
        self.fill.SetScale(Vector3 {
            X: f.clamp(0.0, 1.0),
            Y: 1.0,
            Z: 1.0,
        })
    }

    /// The seek bar is grabbed or dragged to `x` (menu DIPs).
    pub fn drag_to(&self, gfx: &Gfx, x: f32) -> Result<()> {
        let track = self.parts().track;
        let left = self.rect.get().x + track.x;
        let f = ((x - left) / track.w.max(1.0)).clamp(0.0, 1.0);
        self.drag.set(Some(f));
        self.fill.StopAnimation(h!("Scale.X"))?;
        self.set_fraction(f)?;
        self.paint_times(gfx)
    }

    /// The seek bar was let go: the position to seek to (ticks). The card
    /// assumes the seek works and continues from there.
    pub fn end_drag(&self) -> Option<i64> {
        let f = self.drag.take()?;
        let mut view = self.view.borrow_mut();
        let v = view.as_mut()?;
        let ticks = (f64::from(f) * v.length as f64) as i64;
        v.position = ticks;
        v.at = Instant::now();
        Some(ticks)
    }

    /// The drag was interrupted (menu closed): forget it.
    pub fn cancel_drag(&self) {
        self.drag.set(None);
    }

    // -----------------------------------------------------------------------
    // Hit testing, hover, focus
    // -----------------------------------------------------------------------

    /// The part under `p` (menu DIPs). Disabled parts are ignored.
    pub fn hit(&self, p: Vector2) -> Option<CardPart> {
        let r = self.rect.get();
        if !r.contains(p) {
            return None;
        }
        let local = Vector2 {
            X: p.X - r.x,
            Y: p.Y - r.y,
        };
        let parts = self.parts();
        let candidates = CardPart::BUTTONS.into_iter().zip(parts.buttons).chain([
            (CardPart::Seek, parts.seek_hit),
            (CardPart::Source, parts.pill),
        ]);
        for (part, rect) in candidates {
            if rect.contains(local) && self.is_enabled(part) {
                return Some(part);
            }
        }
        None
    }

    /// A part's rectangle in menu coordinates (for the focus ring).
    pub fn part_rect(&self, part: CardPart) -> Rect {
        let r = self.rect.get();
        let p = self.parts();
        let local = match part {
            CardPart::Previous => p.buttons[0],
            CardPart::PlayPause => p.buttons[1],
            CardPart::Next => p.buttons[2],
            CardPart::Seek => p.seek_hit,
            CardPart::Source => p.pill,
        };
        Rect::new(r.x + local.x, r.y + local.y, local.w, local.h)
    }

    /// Corner radius of a part's highlight (the pill is fully round).
    pub fn part_radius(&self, part: CardPart) -> f32 {
        match part {
            CardPart::Source => self.parts().pill.h / 2.0,
            _ => BUTTON_RADIUS,
        }
    }

    /// Where Tab and the arrow keys go inside the card, in order.
    pub fn focus_order(&self) -> Vec<CardPart> {
        CardPart::BUTTONS
            .into_iter()
            .chain([CardPart::Source])
            .filter(|&p| self.is_enabled(p))
            .collect()
    }

    /// Highlights the button under the mouse (none for the seek bar).
    pub fn set_hover(&self, part: Option<CardPart>) -> Result<()> {
        match part.filter(|&p| p != CardPart::Seek) {
            Some(part) => {
                let r = self.rect.get();
                let m = self.part_rect(part);
                self.hover.set_radius(self.part_radius(part));
                self.hover
                    .set_rect(Rect::new(m.x - r.x, m.y - r.y, m.w, m.h))?;
                self.hover.show(true)
            }
            None => self.hover.show(false),
        }
    }
}
