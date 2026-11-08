//! The footer (profile + power button) and the power strip popup.
//!
//! The power strip is a row of 4 buttons (Lock, Sleep, Restart, Shut down)
//! that pops up above the footer. Restart and Shut down first
//! swap the strip to "Restart now?  [Cancel] [Restart]".

use std::cell::{Cell, RefCell};

use windows::UI::Composition::{
    CompositionObject, Compositor, ContainerVisual, SpriteVisual, Visual,
};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ELLIPSE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, ID2D1DeviceContext,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameDisplay};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, Interface, PWSTR, Result};
use windows_numerics::{Vector2, Vector3};

use super::anim::{Animator, Kind, POP_FROM_SCALE};
use super::gfx::{self, Canvas, Gfx, text_primary, text_secondary};
use super::layout::{self, AVATAR, Layout, POWER_BUTTON, Rect};
use super::shape::RoundRect;
use crate::config::Rgb;
use crate::power::PowerAction;
use crate::theme::FooterStyle;

const NAME_SIZE: f32 = 13.0;
const POWER_GLYPH: &str = "\u{E7E8}";
const PERSON_GLYPH: &str = "\u{E77B}";
const GEAR_GLYPH: &str = "\u{E713}";
/// The separate footer buttons (Buttons / Text styles), in order.
pub const QUICK_ACTIONS: [PowerAction; 3] = [
    PowerAction::Lock,
    PowerAction::Restart,
    PowerAction::ShutDown,
];
const QUICK_TEXT_SIZE: f32 = 12.0;

// ---------------------------------------------------------------------------
// Who is signed in
// ---------------------------------------------------------------------------

/// "First Last" if Windows knows it, else the account name.
fn display_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: valid buffer and length; GetUserNameExW writes at most `len`.
    let ok = unsafe { GetUserNameExW(NameDisplay, Some(PWSTR(buf.as_mut_ptr())), &mut len) };
    if ok && len > 0 {
        return String::from_utf16_lossy(&buf[..len as usize]);
    }
    std::env::var("USERNAME").unwrap_or_else(|_| "User".into())
}

/// Our user's SID as text ("S-1-5-21-..."), for the account picture lookup.
fn user_sid() -> Option<String> {
    // SAFETY: opens our own process token (closed below); the buffer is
    // sized by the first call and holds a TOKEN_USER afterwards. The SID
    // string is freed with LocalFree after copying it.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        // u64s keep the buffer aligned for TOKEN_USER.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let got = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        );
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

/// Windows keeps the account picture in several sizes. Returns the path of
/// the smallest one that is at least `px` pixels, or the biggest there is.
fn account_picture(px: i32) -> Option<String> {
    const SIZES: [i32; 11] = [32, 40, 48, 64, 96, 192, 208, 240, 424, 448, 1080];
    let sid = user_sid()?;
    let key = HSTRING::from(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AccountPicture\\Users\\{sid}"
    ));
    let read = |size: i32| -> Option<String> {
        let value = HSTRING::from(format!("Image{size}"));
        let mut buf = [0u16; 520];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: valid buffer with its size in bytes; read-only lookup.
        let err = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                &value,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if err != ERROR_SUCCESS {
            return None;
        }
        let len = (bytes as usize / 2).saturating_sub(1); // without the NUL
        let path = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
        std::path::Path::new(&path).exists().then_some(path)
    };
    let bigger = SIZES.iter().filter(|&&s| s >= px).find_map(|&s| read(s));
    bigger.or_else(|| SIZES.iter().rev().find_map(|&s| read(s)))
}

pub(super) fn draw_text(
    ctx: &ID2D1DeviceContext,
    gfx: &Gfx,
    text: &str,
    format: &IDWriteTextFormat,
    r: Rect,
    align: DWRITE_TEXT_ALIGNMENT,
    color: Rgb,
) -> Result<()> {
    let layout = gfx.text_layout(text, format, r.size(), align, true)?;
    let brush = gfx::brush(ctx, color, 1.0)?;
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

// ---------------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------------

pub struct Footer {
    fill: SpriteVisual,
    divider: SpriteVisual,
    /// Lock / Restart / Shut down (only shown with those footer styles).
    quick: Vec<Canvas>,
    quick_text: IDWriteTextFormat,
    avatar: Canvas,
    name: Canvas,
    power: Canvas,
    /// The gear that opens Settings.
    gear: Canvas,
    user_name: String,
    /// Width of the painted name, so the profile hover hugs it.
    name_width: Cell<f32>,
    layout: RefCell<Layout>,
    name_format: IDWriteTextFormat,
    glyph_format: IDWriteTextFormat,
    avatar_glyph_format: IDWriteTextFormat,
}

impl Footer {
    /// `back` gets the fill and divider (below the hover highlight),
    /// `front` the avatar, name and power icon.
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        back: &ContainerVisual,
        front: &ContainerVisual,
        layout: &Layout,
    ) -> Result<Self> {
        let p = gfx::theme().palette;
        let fill = c.CreateSpriteVisual()?;
        fill.SetBrush(&c.CreateColorBrushWithColor(gfx::paint(p.footer))?)?;
        let divider = c.CreateSpriteVisual()?;
        divider.SetBrush(&c.CreateColorBrushWithColor(gfx::paint(p.divider))?)?;
        back.Children()?.InsertAtTop(&fill)?;
        back.Children()?.InsertAtTop(&divider)?;

        let avatar = Canvas::new(c, gfx, AVATAR_SIZE)?;
        // A round picture: clip the square to a circle.
        let circle = c.CreateEllipseGeometry()?;
        circle.SetCenter(Vector2 {
            X: AVATAR / 2.0,
            Y: AVATAR / 2.0,
        })?;
        circle.SetRadius(Vector2 {
            X: AVATAR / 2.0,
            Y: AVATAR / 2.0,
        })?;
        avatar
            .visual
            .SetClip(&c.CreateGeometricClipWithGeometry(&circle)?)?;
        let name = Canvas::new(c, gfx, layout.name.size())?;
        let power = Canvas::new(c, gfx, layout.power.size())?;
        let gear = Canvas::new(c, gfx, layout.settings.size())?;
        for v in [&avatar.visual, &name.visual, &power.visual, &gear.visual] {
            front.Children()?.InsertAtTop(v)?;
        }
        let mut quick = Vec::with_capacity(QUICK_ACTIONS.len());
        for _ in QUICK_ACTIONS {
            let q = Canvas::new(c, gfx, Vector2 { X: 1.0, Y: 1.0 })?;
            q.visual.SetIsVisible(false)?;
            front.Children()?.InsertAtTop(&q.visual)?;
            quick.push(q);
        }

        let user_name = display_name();
        log_info!("profile: {user_name}");
        Ok(Self {
            fill,
            divider,
            quick,
            quick_text: gfx.text_format(QUICK_TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            avatar,
            name,
            power,
            gear,
            user_name,
            name_width: Cell::new(0.0),
            layout: RefCell::new(layout.clone()),
            name_format: gfx.text_format(NAME_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            glyph_format: gfx.icon_format(16.0)?,
            avatar_glyph_format: gfx.icon_format(15.0)?,
        })
    }

    /// The profile's hover / focus area: hugs the avatar and the name.
    pub fn profile_rect(&self) -> Rect {
        let l = self.layout.borrow();
        let mut r = l.profile;
        r.w = (l.name.x - r.x + self.name_width.get() + 10.0).min(l.profile.w);
        r
    }

    pub fn power_rect(&self) -> Rect {
        self.layout.borrow().power
    }

    pub fn settings_rect(&self) -> Rect {
        self.layout.borrow().settings
    }

    /// The separate footer buttons (empty with the power-button style).
    pub fn quick_rects(&self) -> Vec<Rect> {
        self.layout.borrow().quick.clone()
    }

    /// The active theme changed: the fill and divider colors. Call
    /// `refresh` afterwards (it repaints and places everything).
    pub fn set_theme(&self) -> Result<()> {
        let p = gfx::theme().palette;
        for (v, paint) in [(&self.fill, p.footer), (&self.divider, p.divider)] {
            v.Brush()?
                .cast::<windows::UI::Composition::CompositionColorBrush>()?
                .SetColor(gfx::paint(paint))?;
        }
        Ok(())
    }

    /// Positions and paints everything (layout or DPI change).
    pub fn refresh(&self, gfx: &Gfx, layout: &Layout) -> Result<()> {
        self.avatar.resize(gfx, AVATAR_SIZE)?;
        self.name.resize(gfx, layout.name.size())?;
        self.power.resize(gfx, layout.power.size())?;
        self.gear.resize(gfx, layout.settings.size())?;
        for (i, q) in self.quick.iter().enumerate() {
            match layout.quick.get(i) {
                Some(r) => {
                    q.resize(gfx, r.size())?;
                    q.visual.SetIsVisible(true)?;
                }
                None => q.visual.SetIsVisible(false)?,
            }
        }
        self.power.visual.SetIsVisible(layout.quick.is_empty())?;
        self.move_to(gfx, layout)?;
        self.redraw(gfx)
    }

    /// Only moves things (the menu got taller or shorter): no repaint.
    pub fn move_to(&self, gfx: &Gfx, layout: &Layout) -> Result<()> {
        *self.layout.borrow_mut() = layout.clone();
        let f = layout.footer;
        let s = gfx.scale();
        self.fill.SetOffset(Vector3 {
            X: 0.0,
            Y: gfx::snap(f.y, s),
            Z: 0.0,
        })?;
        self.fill.SetSize(f.size())?;
        self.divider.SetOffset(Vector3 {
            X: 0.0,
            Y: gfx::snap(f.y, s),
            Z: 0.0,
        })?;
        self.divider.SetSize(Vector2 {
            X: f.w,
            Y: gfx::theme().shape.divider,
        })?;
        self.avatar
            .set_offset(gfx, layout.avatar.x, layout.avatar.y)?;
        self.name.set_offset(gfx, layout.name.x, layout.name.y)?;
        self.power.set_offset(gfx, layout.power.x, layout.power.y)?;
        for (q, r) in self.quick.iter().zip(&layout.quick) {
            q.set_offset(gfx, r.x, r.y)?;
        }
        self.gear
            .set_offset(gfx, layout.settings.x, layout.settings.y)
    }

    /// The footer's background: its top edge is where the footer starts,
    /// even while it glides.
    pub fn fill_visual(&self) -> Result<Visual> {
        self.fill.cast()
    }

    /// What moves when the menu's height changes (layout animation).
    pub fn motion_objects(&self) -> Result<Vec<(CompositionObject, Kind)>> {
        let mut list = Vec::new();
        for v in [&self.fill, &self.divider] {
            list.push((v.cast()?, Kind::Visual));
        }
        for c in [&self.avatar, &self.name, &self.power, &self.gear] {
            list.push((c.visual.cast()?, Kind::Visual));
        }
        for c in &self.quick {
            list.push((c.visual.cast()?, Kind::Visual));
        }
        Ok(list)
    }

    fn redraw(&self, gfx: &Gfx) -> Result<()> {
        // Avatar: the account picture, or a person icon in a soft circle.
        let px = gfx.px(AVATAR);
        let picture = account_picture(px).and_then(|path| {
            gfx.load_image(&path, px as u32)
                .inspect_err(|e| log_warn!("account picture {path}: {e}"))
                .ok()
        });
        self.avatar.draw(gfx, |ctx| {
            let full = gfx::d2d_rect(Rect::new(0.0, 0.0, AVATAR, AVATAR));
            // SAFETY: drawing calls with live objects and local structs.
            unsafe {
                if let Some(bitmap) = &picture {
                    ctx.DrawBitmap(
                        bitmap,
                        Some(&full),
                        1.0,
                        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                        None,
                        None,
                    );
                    return Ok(());
                }
                let chip = gfx::theme().palette.chip;
                let bg = gfx::brush(ctx, chip.color, (chip.alpha * 1.4).min(1.0))?;
                let half = AVATAR / 2.0;
                ctx.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: half, Y: half },
                        radiusX: half,
                        radiusY: half,
                    },
                    &bg,
                );
            }
            draw_text(
                ctx,
                gfx,
                PERSON_GLYPH,
                &self.avatar_glyph_format,
                Rect::new(0.0, 0.0, AVATAR, AVATAR),
                DWRITE_TEXT_ALIGNMENT_CENTER,
                text_primary(),
            )
        })?;

        let name_size = self.name.size();
        let layout = gfx.text_layout(
            &self.user_name,
            &self.name_format,
            name_size,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            true,
        )?;
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: valid out-pointer to a local.
        unsafe { layout.GetMetrics(&mut m)? };
        self.name_width.set(m.width.min(name_size.X));
        self.name.draw(gfx, |ctx| {
            let brush = gfx::brush(ctx, text_primary(), 1.0)?;
            gfx::with_glow(ctx, |ctx| {
                // SAFETY: draws a live text layout with a live brush.
                unsafe { ctx.DrawTextLayout(Vector2::zero(), &layout, &brush, gfx::TEXT_OPTIONS) };
                Ok(())
            })
        })?;

        for (canvas, glyph) in [(&self.power, POWER_GLYPH), (&self.gear, GEAR_GLYPH)] {
            canvas.draw(gfx, |ctx| {
                gfx::with_glow(ctx, |ctx| {
                    draw_text(
                        ctx,
                        gfx,
                        glyph,
                        &self.glyph_format,
                        Rect::new(0.0, 0.0, POWER_BUTTON, POWER_BUTTON),
                        DWRITE_TEXT_ALIGNMENT_CENTER,
                        text_primary(),
                    )
                })
            })?;
        }
        self.paint_quick(gfx)
    }

    /// Paints Lock / Restart / Shut down in the theme's footer style.
    fn paint_quick(&self, gfx: &Gfx) -> Result<()> {
        let theme = gfx::theme();
        let p = theme.palette;
        let radius = theme.shape.button_radius;
        let rects = self.quick_rects();
        for (i, ((canvas, action), rect)) in
            self.quick.iter().zip(QUICK_ACTIONS).zip(&rects).enumerate()
        {
            let full = Rect::new(0.0, 0.0, rect.w, rect.h);
            let danger = action == PowerAction::ShutDown;
            canvas.draw(gfx, |ctx| {
                if danger {
                    gfx::round_rect(ctx, full, radius, Some((p.danger, 1.0)), None)?;
                } else if theme.footer == FooterStyle::Buttons {
                    let chip = (p.chip.color, p.chip.alpha);
                    gfx::round_rect(ctx, full, radius, Some(chip), None)?;
                }
                match theme.footer {
                    FooterStyle::Text => draw_text(
                        ctx,
                        gfx,
                        layout::QUICK_LABELS[i],
                        &self.quick_text,
                        full,
                        DWRITE_TEXT_ALIGNMENT_CENTER,
                        if danger {
                            gfx::on_fill(p.danger)
                        } else {
                            text_primary()
                        },
                    ),
                    _ => {
                        let color = match action {
                            PowerAction::ShutDown => gfx::on_fill(p.danger),
                            PowerAction::Lock => p.lock,
                            _ => p.restart,
                        };
                        draw_text(
                            ctx,
                            gfx,
                            action.glyph(),
                            &self.glyph_format,
                            full,
                            DWRITE_TEXT_ALIGNMENT_CENTER,
                            color,
                        )
                    }
                }
            })?;
        }
        Ok(())
    }
}

const AVATAR_SIZE: Vector2 = Vector2 {
    X: AVATAR,
    Y: AVATAR,
};

// ---------------------------------------------------------------------------
// Power strip
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripState {
    Closed,
    Buttons,
    /// "Restart now?  [Cancel] [Restart]"
    Confirm(PowerAction),
}

const STRIP_GLYPH_SIZE: f32 = 18.0;
const STRIP_LABEL_SIZE: f32 = 11.0;

pub struct PowerStrip {
    root: ContainerVisual,
    panel: RoundRect,
    hover: RoundRect,
    content: Canvas,
    /// Where the strip is, in menu coordinates.
    rect: Cell<Rect>,
    state: Cell<StripState>,
    glyph_format: IDWriteTextFormat,
    label_format: IDWriteTextFormat,
    question_format: IDWriteTextFormat,
}

impl PowerStrip {
    pub fn new(c: &Compositor, gfx: &Gfx, parent: &ContainerVisual, rect: Rect) -> Result<Self> {
        let root = c.CreateContainerVisual()?;
        root.SetOpacity(0.0)?;
        root.SetIsVisible(false)?;
        let panel = RoundRect::new(c, 14.0, 1.0)?;
        let hover = RoundRect::new(c, 10.0, 0.0)?;
        hover.show(false)?;
        let content = Canvas::new(c, gfx, rect.size())?;
        root.Children()?.InsertAtTop(&panel.visual)?;
        root.Children()?.InsertAtTop(&hover.visual)?;
        root.Children()?.InsertAtTop(&content.visual)?;
        parent.Children()?.InsertAtTop(&root)?;
        let strip = Self {
            root,
            panel,
            hover,
            content,
            rect: Cell::new(rect),
            state: Cell::new(StripState::Closed),
            glyph_format: gfx.icon_format(STRIP_GLYPH_SIZE)?,
            label_format: gfx.text_format(STRIP_LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            question_format: gfx.text_format(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
        };
        strip.set_theme()?;
        Ok(strip)
    }

    /// The active theme's popup colors and corners. Call `refresh`
    /// afterwards (it places and repaints).
    pub fn set_theme(&self) -> Result<()> {
        let t = gfx::theme();
        self.panel.set_fill(gfx::paint(t.palette.popup))?;
        self.panel.set_stroke(gfx::paint(t.palette.popup_border))?;
        self.panel.set_radius(t.shape.popup_radius);
        self.hover.set_fill(gfx::paint(t.palette.hover))?;
        self.hover.set_radius(t.shape.button_radius);
        Ok(())
    }

    pub fn state(&self) -> StripState {
        self.state.get()
    }

    pub fn is_open(&self) -> bool {
        self.state.get() != StripState::Closed
    }

    pub fn rect(&self) -> Rect {
        self.rect.get()
    }

    /// The buttons that are showing, in menu coordinates.
    pub fn buttons(&self) -> Vec<Rect> {
        let r = self.rect.get();
        let local: Vec<Rect> = match self.state.get() {
            StripState::Confirm(_) => layout::confirm_buttons(r).to_vec(),
            _ => layout::strip_buttons(r).to_vec(),
        };
        local
            .into_iter()
            .map(|b| Rect::new(r.x + b.x, r.y + b.y, b.w, b.h))
            .collect()
    }

    /// Which button is under the point (menu coordinates).
    pub fn hit(&self, p: windows_numerics::Vector2) -> Option<usize> {
        self.buttons().iter().position(|b| b.contains(p))
    }

    pub fn set_hover(&self, button: Option<usize>) -> Result<()> {
        let r = self.rect.get();
        match button.and_then(|i| self.buttons().get(i).copied()) {
            Some(b) => {
                self.hover
                    .set_rect(Rect::new(b.x - r.x, b.y - r.y, b.w, b.h))?;
                self.hover.show(true)
            }
            None => self.hover.show(false),
        }
    }

    /// Positions it (and repaints). `grow_from` is the point it grows out
    /// of (the power button's center), in menu coordinates.
    pub fn refresh(&self, gfx: &Gfx, rect: Rect, grow_from: Vector2) -> Result<()> {
        self.move_to(gfx, rect, grow_from)?;
        self.panel.set_rect(Rect::new(0.0, 0.0, rect.w, rect.h))?;
        self.content.resize(gfx, rect.size())?;
        self.redraw(gfx)
    }

    /// The root visual, for the layout animation.
    pub fn motion_object(&self) -> Result<CompositionObject> {
        self.root.cast()
    }

    /// Only moves it (the menu got taller or shorter): no repaint.
    pub fn move_to(&self, gfx: &Gfx, rect: Rect, grow_from: Vector2) -> Result<()> {
        self.rect.set(rect);
        let s = gfx.scale();
        self.root.SetOffset(Vector3 {
            X: gfx::snap(rect.x, s),
            Y: gfx::snap(rect.y, s),
            Z: 0.0,
        })?;
        self.root.SetSize(rect.size())?;
        self.root.SetCenterPoint(Vector3 {
            X: (grow_from.X - rect.x).clamp(0.0, rect.w),
            Y: rect.h,
            Z: 0.0,
        })
    }

    fn redraw(&self, gfx: &Gfx) -> Result<()> {
        let r = self.rect.get();
        let state = self.state.get();
        self.content.draw(gfx, |ctx| match state {
            StripState::Confirm(action) => {
                let [cancel, confirm] = layout::confirm_buttons(r);
                draw_text(
                    ctx,
                    gfx,
                    action.question(),
                    &self.question_format,
                    Rect::new(16.0, 0.0, cancel.x - 24.0, r.h),
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    text_primary(),
                )?;
                let t = gfx::theme();
                let chip = t.palette.chip;
                let danger = t.palette.danger;
                let radius = t.shape.button_radius.min(8.0);
                for (b, fill, alpha, label, color) in [
                    (cancel, chip.color, chip.alpha, "Cancel", text_primary()),
                    (confirm, danger, 0.95, action.label(), gfx::on_fill(danger)),
                ] {
                    gfx::round_rect(ctx, b, radius, Some((fill, alpha)), None)?;
                    draw_text(
                        ctx,
                        gfx,
                        label,
                        &self.label_format,
                        b,
                        DWRITE_TEXT_ALIGNMENT_CENTER,
                        color,
                    )?;
                }
                Ok(())
            }
            _ => {
                for (action, b) in PowerAction::ALL.iter().zip(layout::strip_buttons(r)) {
                    let glyph = Rect::new(b.x, b.y + 6.0, b.w, 24.0);
                    let label = Rect::new(b.x + 2.0, b.y + 32.0, b.w - 4.0, 16.0);
                    draw_text(
                        ctx,
                        gfx,
                        action.glyph(),
                        &self.glyph_format,
                        glyph,
                        DWRITE_TEXT_ALIGNMENT_CENTER,
                        text_primary(),
                    )?;
                    draw_text(
                        ctx,
                        gfx,
                        action.label(),
                        &self.label_format,
                        label,
                        DWRITE_TEXT_ALIGNMENT_CENTER,
                        text_secondary(),
                    )?;
                }
                Ok(())
            }
        })
    }

    fn visual(&self) -> Result<Visual> {
        self.root.cast()
    }

    pub fn open(&self, gfx: &Gfx, animator: &Animator, reduced_motion: bool) -> Result<()> {
        self.open_in(gfx, animator, StripState::Buttons, reduced_motion)
    }

    /// Opens it straight into "Restart now?" (the separate footer buttons).
    pub fn open_confirm(
        &self,
        gfx: &Gfx,
        animator: &Animator,
        action: PowerAction,
        reduced_motion: bool,
    ) -> Result<()> {
        self.open_in(gfx, animator, StripState::Confirm(action), reduced_motion)
    }

    fn open_in(
        &self,
        gfx: &Gfx,
        animator: &Animator,
        state: StripState,
        reduced_motion: bool,
    ) -> Result<()> {
        let was_closed = !self.is_open();
        self.state.set(state);
        self.redraw(gfx)?;
        self.set_hover(None)?;
        let v = self.visual()?;
        if was_closed && v.Opacity()? < 0.01 {
            v.SetScale(Vector3 {
                X: POP_FROM_SCALE,
                Y: POP_FROM_SCALE,
                Z: 1.0,
            })?;
        }
        v.SetIsVisible(true)?;
        animator.pop_in(&v, reduced_motion)
    }

    pub fn close(&self, animator: &Animator, reduced_motion: bool) -> Result<()> {
        if !self.is_open() {
            return Ok(());
        }
        self.state.set(StripState::Closed);
        self.set_hover(None)?;
        animator.pop_out(&self.visual()?, reduced_motion)
    }

    /// Closed at once, no animation (when the menu opens again).
    pub fn reset(&self) -> Result<()> {
        self.state.set(StripState::Closed);
        self.set_hover(None)?;
        let v = self.visual()?;
        v.StopAnimation(windows::core::h!("Opacity"))?;
        v.StopAnimation(windows::core::h!("Scale"))?;
        v.SetOpacity(0.0)?;
        v.SetIsVisible(false)
    }

    /// Switches between the buttons and the "Are you sure?" view.
    pub fn set_state(&self, gfx: &Gfx, state: StripState) -> Result<()> {
        self.state.set(state);
        self.set_hover(None)?;
        self.redraw(gfx)
    }
}
