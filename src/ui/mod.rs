//! Composition setup: Compositor + DesktopWindowTarget hosted in our HWND.
//!
//! Everything inside the root visual is laid out in DIPs; the root visual is
//! scaled by the monitor's DPI factor. The D3D/D2D devices for text and icons
//! arrive in milestone 3.

pub mod anim;
pub mod background;
pub mod shell;

use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionScopedBatch, Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use crate::config::Config;
use crate::window::{Edge, Placement};
use anim::Animator;
use background::{Applied, SystemEffects};

pub struct Ui {
    _compositor: Compositor,
    // Keeps our visual tree attached to the window.
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    shell: shell::Shell,
    menu: Visual,
    animator: Animator,
}

impl Ui {
    pub fn new(hwnd: HWND, menu_size: Vector2, config: &Config) -> Result<Self> {
        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: `hwnd` is our live top-level window, created on this thread.
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, true)? };

        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        let shell = shell::Shell::new(&compositor, menu_size)?;
        root.Children()?.InsertAtTop(shell.host())?;
        let menu = shell.host_visual()?;
        Animator::reset_closed(&menu)?;

        let animator = Animator::new(&compositor, &config.animation)?;

        Ok(Self {
            _compositor: compositor,
            _target: target,
            root,
            shell,
            menu,
            animator,
        })
    }

    /// Applies the DPI scale and the menu's position inside the window, and
    /// puts the scale origin on the edge nearest the taskbar.
    pub fn set_layout(&self, p: &Placement) -> Result<()> {
        self.root.SetScale(Vector3 {
            X: p.scale,
            Y: p.scale,
            Z: 1.0,
        })?;
        self.menu.SetOffset(Vector3 {
            X: p.menu_x as f32 / p.scale,
            Y: p.menu_y as f32 / p.scale,
            Z: 0.0,
        })?;
        let s = self.shell.size();
        let (x, y) = match p.edge {
            Edge::Bottom => (s.X / 2.0, s.Y),
            Edge::Top => (s.X / 2.0, 0.0),
            // Vertical taskbars: grow from the corner next to Start.
            Edge::Left => (0.0, 0.0),
            Edge::Right => (s.X, 0.0),
        };
        self.menu.SetCenterPoint(Vector3 { X: x, Y: y, Z: 0.0 })
    }

    pub fn apply_background(&self, config: &Config, effects: SystemEffects) -> Result<Applied> {
        self.shell.background.apply(&config.background, effects)
    }

    /// Call before showing a fully hidden window, so the open starts at 0.
    pub fn reset_closed(&self) -> Result<()> {
        Animator::reset_closed(&self.menu)
    }

    pub fn animate_open(&self, reduced_motion: bool) -> Result<()> {
        self.animator.open(&self.menu, reduced_motion)
    }

    pub fn animate_close(&self, reduced_motion: bool) -> Result<CompositionScopedBatch> {
        self.animator.close(&self.menu, reduced_motion)
    }
}
