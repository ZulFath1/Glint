//! Painting text and icons.
//!
//! The compositor (DWM) can only show shapes, colors and *surfaces*. A
//! surface is a small picture we paint ourselves with Direct2D (shapes,
//! bitmaps) and DirectWrite (text). We paint each one once, and again only
//! when its content changes (typing, a DPI change). Motion never repaints.
//!
//! One D3D11 device (WARP, the software renderer; see `create_devices`), one
//! D2D device and one `CompositionGraphicsDevice` serve the whole app. Surfaces are sized in physical pixels for the current DPI
//! scale, and we draw into them in DIPs (D2D does the scaling), so text
//! stays sharp at 125–200%.

use std::cell::{Cell, RefCell};

use windows::Foundation::{Size, TypedEventHandler};
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::SizeInt32;
use windows::UI::Color;
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, CompositionStretch, Compositor,
    SpriteVisual,
};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Shadow, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_PROPERTY_TYPE_VECTOR4, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_DEVICE_REMOVED, IDXGIDevice};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositionGraphicsDeviceInterop, ICompositorInterop,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Error, HRESULT, Interface, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use super::layout::Rect;
use crate::config::Rgb;

/// Posted to the UI window when the GPU device was replaced: every surface
/// has to be painted again.
pub const WM_APP_REDRAW: u32 = WM_APP + 5;

/// How all text is drawn: with color fonts on, so emoji show in color.
pub const TEXT_OPTIONS: D2D1_DRAW_TEXT_OPTIONS = D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;

/// Text colors (CLAUDE.md section 5).
const TEXT_PRIMARY: Rgb = Rgb::new(0xf4, 0xf4, 0xf5);
const TEXT_SECONDARY: Rgb = Rgb::new(0xd4, 0xd4, 0xd8);

thread_local! {
    /// Windows high contrast is on: all text in this color, and no glow.
    /// (Only the UI thread paints, so a per-thread value is enough.)
    static HIGH_CONTRAST_TEXT: Cell<Option<Rgb>> = const { Cell::new(None) };
}

/// Switches every text color to the high contrast one (`Some`), or back to
/// ours. Surfaces have to be repainted afterwards.
pub fn set_high_contrast_text(text: Option<Rgb>) {
    HIGH_CONTRAST_TEXT.with(|c| c.set(text));
}

fn high_contrast_text() -> Option<Rgb> {
    HIGH_CONTRAST_TEXT.with(Cell::get)
}

/// Main text: names, titles, labels.
pub fn text_primary() -> Rgb {
    high_contrast_text().unwrap_or(TEXT_PRIMARY)
}

/// Quieter text: artist, times, placeholder. In high contrast, all text
/// uses the one system text color.
pub fn text_secondary() -> Rgb {
    high_contrast_text().unwrap_or(TEXT_SECONDARY)
}

const TEXT_FONT: windows::core::PCWSTR = w!("Segoe UI Variable Text");
const ICON_FONT: windows::core::PCWSTR = w!("Segoe Fluent Icons");

/// The soft text shadow ("glow") behind text and icons, so they stay
/// readable on colourless glass over a bright wallpaper.
const GLOW_BLUR: f32 = 2.0;
const GLOW_ALPHA: f32 = 0.45;
const GLOW_DROP: f32 = 0.5;

/// D2D says "recreate the target" when the GPU device is lost.
const D2DERR_RECREATE_TARGET: HRESULT = HRESULT(0x8899_000C_u32 as i32);

/// A composition color from 0–255 RGB and a 0–1 alpha.
pub fn color(c: Rgb, alpha: f32) -> Color {
    Color {
        A: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        R: c.r,
        G: c.g,
        B: c.b,
    }
}

/// A Direct2D color from 0–255 RGB and a 0–1 alpha.
pub fn d2d_color(c: Rgb, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: alpha,
    }
}

pub fn d2d_rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

/// Rounds a DIP coordinate to the nearest physical pixel, so bitmaps and
/// text are not smeared across two pixels at 125% or 175%.
pub fn snap(dip: f32, scale: f32) -> f32 {
    (dip * scale).round() / scale
}

struct Devices {
    /// Kept to ask "is the GPU still there?" (`GetDeviceRemovedReason`).
    d3d: ID3D11Device,
    d2d: ID2D1Device,
    /// A device context that is never drawn into; it creates bitmaps that
    /// every surface (all on the same D2D device) can then draw.
    resources: ID2D1DeviceContext,
}

fn create_devices(factory: &ID2D1Factory1) -> Result<Devices> {
    let create = |driver| {
        let mut device: Option<ID3D11Device> = None;
        // SAFETY: valid out-pointer to a local; no adapter, no software module.
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, // needed by Direct2D
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .and_then(|()| device.ok_or_else(Error::empty))
    };
    // WARP: Windows' own software renderer, on purpose. We only *paint*
    // small pictures, rarely (typing, a song change, a DPI change); all
    // motion is DWM's, on the GPU, either way. A GPU device would load the
    // graphics driver into Glint. Measured in milestone 7 on an NVIDIA
    // laptop: WARP = 44 MB less private memory, 44 fewer threads (the
    // driver's), 3x less idle CPU (driver threads waking up), no 2-handle
    // leak per device reset (a driver bug), and a driver update can't take
    // the device away. Painting the whole menu: 14.9 ms vs 13.8 ms.
    // The GPU only if WARP is missing (it never should be).
    let d3d = create(D3D_DRIVER_TYPE_WARP).or_else(|e| {
        log_warn!("no WARP device ({e}); using the GPU");
        create(D3D_DRIVER_TYPE_HARDWARE)
    })?;
    let dxgi: IDXGIDevice = d3d.cast()?;
    // SAFETY: plain COM calls on live interfaces.
    unsafe {
        let d2d = factory.CreateDevice(&dxgi)?;
        let resources = d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        Ok(Devices {
            d3d,
            d2d: d2d.cast()?,
            resources,
        })
    }
}

pub struct Gfx {
    factory: ID2D1Factory1,
    devices: RefCell<Devices>,
    graphics: CompositionGraphicsDevice,
    replaced_token: i64,
    pub dwrite: IDWriteFactory,
    pub wic: IWICImagingFactory,
    /// The DPI scale every surface is currently painted at.
    scale: Cell<f32>,
}

impl Gfx {
    pub fn new(compositor: &Compositor, hwnd: HWND, scale: f32) -> Result<Self> {
        // SAFETY: creates process-local factories; no pointers are kept.
        let (factory, dwrite, wic) = unsafe {
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            (factory, dwrite, wic)
        };
        let devices = create_devices(&factory)?;
        let interop: ICompositorInterop = compositor.cast()?;
        // SAFETY: hands our live D2D device to the compositor.
        let graphics = unsafe { interop.CreateGraphicsDevice(&devices.d2d)? };

        // When the device is replaced, every surface is empty again; ask
        // the UI thread to paint everything.
        let target = hwnd.0 as isize;
        let replaced_token =
            graphics.RenderingDeviceReplaced(&TypedEventHandler::new(move |_, _| {
                // SAFETY: posts a plain message (no pointers) to our window.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(target as *mut _)),
                        WM_APP_REDRAW,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
                Ok(())
            }))?;

        Ok(Self {
            factory,
            devices: RefCell::new(devices),
            graphics,
            replaced_token,
            dwrite,
            wic,
            scale: Cell::new(scale),
        })
    }

    pub fn scale(&self) -> f32 {
        self.scale.get()
    }

    pub fn set_scale(&self, scale: f32) {
        self.scale.set(scale);
    }

    /// DIPs → whole physical pixels at the current scale.
    pub fn px(&self, dip: f32) -> i32 {
        ((dip * self.scale.get()).ceil() as i32).max(1)
    }

    fn surface_size(&self, size: Vector2) -> SizeInt32 {
        SizeInt32 {
            Width: self.px(size.X),
            Height: self.px(size.Y),
        }
    }

    pub fn create_surface(&self, size: Vector2) -> Result<CompositionDrawingSurface> {
        let px = self.surface_size(size);
        self.graphics.CreateDrawingSurface(
            Size {
                Width: px.Width as f32,
                Height: px.Height as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
    }

    /// Paints `surface` (whose size is `size` DIPs). `paint` draws in DIPs
    /// starting at (0, 0); the surface is cleared to transparent first.
    pub fn draw(
        &self,
        surface: &CompositionDrawingSurface,
        paint: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
    ) -> Result<()> {
        let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
        let mut offset = POINT::default();
        // SAFETY: BeginDraw gets a valid out-pointer for the offset; the whole
        // surface is updated (no update rect).
        let ctx: ID2D1DeviceContext = match unsafe { interop.BeginDraw(None, &mut offset) } {
            Ok(ctx) => ctx,
            Err(e) => {
                self.check_device_lost(&e);
                return Err(e);
            }
        };
        let scale = self.scale.get();
        // SAFETY: plain state changes on the device context we were given.
        unsafe {
            // Surfaces live in a shared atlas; `offset` is where ours starts
            // (in pixels).
            ctx.SetDpi(96.0 * scale, 96.0 * scale);
            ctx.SetTransform(&Matrix3x2::translation(
                offset.x as f32 / scale,
                offset.y as f32 / scale,
            ));
            // ClearType needs an opaque background; ours is transparent.
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            ctx.Clear(Some(&D2D1_COLOR_F::default()));
        }
        let painted = paint(&ctx);
        // SAFETY: ends the BeginDraw above (always, even if painting failed).
        let ended = unsafe { interop.EndDraw() };
        if let Err(e) = &ended {
            self.check_device_lost(e);
        }
        painted.and(ended)
    }

    /// If the GPU device is gone (driver update, GPU reset...), make a new
    /// one. The compositor then fires `RenderingDeviceReplaced`, and we
    /// repaint everything.
    fn check_device_lost(&self, e: &Error) {
        if e.code() != D2DERR_RECREATE_TARGET && e.code() != DXGI_ERROR_DEVICE_REMOVED {
            return;
        }
        log_warn!("graphics device lost ({e}); recreating it");
        self.recreate_devices();
    }

    /// Direct2D keeps scratch GPU textures (for the glow effect, text) in a
    /// cache it only empties when asked. Measured in milestone 7: without
    /// this, private bytes crept up about 1.4 MB per 1,000 open/close cycles.
    /// Called when the menu is hidden, so nothing is repainted meanwhile.
    pub fn release_caches(&self) {
        // SAFETY: a plain call on the live device; 0 ms = everything unused now.
        unsafe { self.devices.borrow().d2d.ClearResources(0) };
    }

    /// Checks that the GPU device still works (a driver update, a GPU reset,
    /// sleep or remote desktop can take it away while the menu is hidden),
    /// and makes a new one if not. Cheap: one call, no drawing. Run before
    /// opening, so a lost device is noticed before anything is painted.
    pub fn ensure_device(&self) {
        // SAFETY: a plain query on a live device.
        let reason = unsafe { self.devices.borrow().d3d.GetDeviceRemovedReason() };
        if let Err(e) = reason {
            log_warn!("graphics device was removed ({e}); recreating it");
            self.recreate_devices();
        }
    }

    /// Debug: runs exactly what a real device loss runs (new D3D and D2D
    /// devices, handed to the compositor, which then fires
    /// `RenderingDeviceReplaced`), without needing a real GPU reset.
    pub fn simulate_device_loss(&self) {
        log_warn!("simulating a graphics device loss");
        self.recreate_devices();
    }

    /// Makes new devices and hands them to the compositor. The old ones are
    /// dropped (released) when replaced.
    fn recreate_devices(&self) {
        match create_devices(&self.factory) {
            Ok(devices) => {
                let swapped = self
                    .graphics
                    .cast::<ICompositionGraphicsDeviceInterop>()
                    // SAFETY: hands the new, live D2D device to the compositor.
                    .and_then(|i| unsafe { i.SetRenderingDevice(&devices.d2d) });
                match swapped {
                    Ok(()) => *self.devices.borrow_mut() = devices,
                    Err(e) => log_error!("SetRenderingDevice failed: {e}"),
                }
            }
            Err(e) => log_error!("recreating the graphics device failed: {e}"),
        }
    }

    /// A WIC picture → a D2D bitmap every surface can draw.
    pub fn bitmap_from_wic(&self, source: &IWICBitmapSource) -> Result<ID2D1Bitmap1> {
        // SAFETY: plain COM calls on live interfaces.
        unsafe {
            let converter = self.wic.CreateFormatConverter()?;
            converter.Initialize(
                source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )?;
            self.devices
                .borrow()
                .resources
                .CreateBitmapFromWicBitmap(&converter, None)
        }
    }

    /// Premultiplied BGRA pixels (`w` × `h`, no row padding) → a D2D bitmap.
    pub fn bitmap_from_pixels(&self, w: u32, h: u32, bgra: &[u8]) -> Result<ID2D1Bitmap1> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        if bgra.len() < (w * h * 4) as usize {
            return Err(Error::from(windows::Win32::Foundation::E_INVALIDARG));
        }
        // SAFETY: `bgra` holds at least h rows of w*4 bytes (checked above);
        // D2D copies the pixels during the call.
        unsafe {
            self.devices.borrow().resources.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                Some(bgra.as_ptr().cast()),
                w * 4,
                &props,
            )
        }
    }

    /// Decodes an image file (JPEG, PNG...) straight to `px` × `px`.
    pub fn load_image(&self, path: &str, px: u32) -> Result<ID2D1Bitmap1> {
        let path = windows::core::HSTRING::from(path);
        // SAFETY: plain COM calls on live interfaces; `path` outlives them.
        unsafe {
            let decoder = self.wic.CreateDecoderFromFilename(
                &path,
                None,
                windows::Win32::Foundation::GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let scaler = self.wic.CreateBitmapScaler()?;
            scaler.Initialize(&frame, px, px, WICBitmapInterpolationModeFant)?;
            self.bitmap_from_wic(&scaler.cast()?)
        }
    }

    pub fn text_format(&self, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        self.format(TEXT_FONT, size, weight)
    }

    /// For the icons in Segoe Fluent Icons (search, power, lock...).
    pub fn icon_format(&self, size: f32) -> Result<IDWriteTextFormat> {
        self.format(
            ICON_FONT,
            size,
            windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
        )
    }

    fn format(
        &self,
        family: windows::core::PCWSTR,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
    ) -> Result<IDWriteTextFormat> {
        // SAFETY: plain COM calls; the strings are 'static.
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            Ok(format)
        }
    }

    /// A single line of text in a box; too-long text ends in "…".
    pub fn text_layout(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        size: Vector2,
        align: DWRITE_TEXT_ALIGNMENT,
        ellipsis: bool,
    ) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain COM calls; `wide` outlives the call (DWrite copies it).
        unsafe {
            let layout =
                self.dwrite
                    .CreateTextLayout(&wide, format, size.X.max(0.0), size.Y.max(0.0))?;
            layout.SetTextAlignment(align)?;
            if ellipsis {
                let sign = self.dwrite.CreateEllipsisTrimmingSign(format)?;
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                layout.SetTrimming(&trimming, &sign)?;
            }
            Ok(layout)
        }
    }
}

impl Drop for Gfx {
    fn drop(&mut self) {
        let _ = self
            .graphics
            .RemoveRenderingDeviceReplaced(self.replaced_token);
    }
}

pub fn brush(ctx: &ID2D1DeviceContext, c: Rgb, alpha: f32) -> Result<ID2D1SolidColorBrush> {
    // SAFETY: plain COM call with a pointer to a local color.
    unsafe { ctx.CreateSolidColorBrush(&d2d_color(c, alpha), None) }
}

/// Draws what `paint` draws, with a soft dark glow behind it.
///
/// `paint` is first recorded into a command list (a replayable list of
/// drawing commands), which then goes through D2D's Shadow effect (the
/// glow) and is finally drawn again on top, sharp.
pub fn with_glow(
    ctx: &ID2D1DeviceContext,
    paint: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
) -> Result<()> {
    // High contrast: crisp text only (the glow is decoration).
    if high_contrast_text().is_some() {
        return paint(ctx);
    }
    // SAFETY: plain COM calls on the live device context. The original target
    // and transform are put back before anything else is drawn.
    unsafe {
        let mut transform = Matrix3x2::default();
        ctx.GetTransform(&mut transform);
        let target = ctx.GetTarget()?;
        let list = match ctx.CreateCommandList() {
            Ok(list) => list,
            Err(_) => return paint(ctx), // no glow is better than nothing
        };
        ctx.SetTarget(&list);
        ctx.SetTransform(&Matrix3x2::identity());
        let painted = paint(ctx);
        ctx.SetTarget(&target);
        ctx.SetTransform(&transform);
        list.Close()?;
        painted?;

        if let Ok(shadow) = ctx.CreateEffect(&CLSID_D2D1Shadow) {
            shadow.SetInput(0, &list, true);
            shadow.SetValue(
                D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &GLOW_BLUR.to_le_bytes(),
            )?;
            let rgba: Vec<u8> = [0.0f32, 0.0, 0.0, GLOW_ALPHA]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            shadow.SetValue(
                D2D1_SHADOW_PROP_COLOR.0 as u32,
                D2D1_PROPERTY_TYPE_VECTOR4,
                &rgba,
            )?;
            let output = shadow.GetOutput()?;
            ctx.DrawImage(
                &output,
                Some(&Vector2 {
                    X: 0.0,
                    Y: GLOW_DROP,
                }),
                None,
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
        }
        ctx.DrawImage(
            &list,
            None,
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
    }
    Ok(())
}

/// A painted picture shown by a sprite visual. The visual is sized in DIPs;
/// the surface behind it in physical pixels.
pub struct Canvas {
    pub visual: SpriteVisual,
    surface: CompositionDrawingSurface,
    size: Cell<Vector2>,
}

impl Canvas {
    pub fn new(compositor: &Compositor, gfx: &Gfx, size: Vector2) -> Result<Self> {
        let surface = gfx.create_surface(size)?;
        let brush = compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        let visual = compositor.CreateSpriteVisual()?;
        visual.SetBrush(&brush)?;
        visual.SetSize(size)?;
        Ok(Self {
            visual,
            surface,
            size: Cell::new(size),
        })
    }

    pub fn size(&self) -> Vector2 {
        self.size.get()
    }

    /// New size (DIPs) and/or new DPI scale. Repaint afterwards.
    pub fn resize(&self, gfx: &Gfx, size: Vector2) -> Result<()> {
        self.size.set(size);
        self.visual.SetSize(size)?;
        self.surface.Resize(gfx.surface_size(size))
    }

    /// Places the canvas at (x, y) DIPs, snapped to whole pixels.
    pub fn set_offset(&self, gfx: &Gfx, x: f32, y: f32) -> Result<()> {
        let s = gfx.scale();
        self.visual.SetOffset(windows_numerics::Vector3 {
            X: snap(x, s),
            Y: snap(y, s),
            Z: 0.0,
        })
    }

    pub fn draw(
        &self,
        gfx: &Gfx,
        paint: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
    ) -> Result<()> {
        gfx.draw(&self.surface, paint)
    }
}
