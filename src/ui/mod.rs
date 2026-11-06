//! Composition setup: Compositor + DesktopWindowTarget hosted in our HWND.
//!
//! Everything inside the root visual is laid out in DIPs; the root visual is
//! scaled by the monitor's DPI factor. The D3D/D2D devices for text and icons
//! arrive in milestone 3.

pub mod shell;

use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{Compositor, ContainerVisual};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use crate::window::SHADOW_MARGIN;

pub struct Ui {
    _compositor: Compositor,
    // Keeps our visual tree attached to the window.
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    _shell: shell::Shell,
}

impl Ui {
    pub fn new(hwnd: HWND, menu_size: Vector2) -> Result<Self> {
        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: `hwnd` is our live top-level window, created on this thread.
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, true)? };

        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        let shell = shell::Shell::new(&compositor, menu_size)?;
        shell.visual().SetOffset(Vector3 {
            X: SHADOW_MARGIN,
            Y: SHADOW_MARGIN,
            Z: 0.0,
        })?;
        root.Children()?.InsertAtTop(shell.visual())?;

        Ok(Self {
            _compositor: compositor,
            _target: target,
            root,
            _shell: shell,
        })
    }

    /// Applies the monitor's DPI scale to the whole tree.
    pub fn set_scale(&self, scale: f32) -> Result<()> {
        self.root.SetScale(Vector3 {
            X: scale,
            Y: scale,
            Z: 1.0,
        })
    }
}
