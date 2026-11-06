//! Open/close animations (CLAUDE.md section 4).
//!
//! These are composition keyframe animations: we describe the curve once and
//! DWM plays it at the monitor's refresh rate. Our process does no work per
//! frame. Every animation starts from `this.StartingValue` (the property's
//! current value), so reopening during a close (or the other way round)
//! continues smoothly from wherever the menu is instead of jumping.
//!
//! The animation objects are created once and reused for every open/close.

use windows::Foundation::TimeSpan;
use windows::UI::Composition::{
    CompositionBatchTypes, CompositionEasingFunction, CompositionScopedBatch, Compositor,
    ScalarKeyFrameAnimation, Vector3KeyFrameAnimation, Visual,
};
use windows::core::{Interface, Result, h};
use windows_numerics::{Vector2, Vector3};

use crate::config;

/// Reduced motion ("Animation effects" off): fade only, this long.
const REDUCED_MS: u32 = 120;
/// Open: the opacity reaches 1 after this long.
const OPEN_FADE_MS: u32 = 110;
/// Close: the opacity fades out over the last this-many ms.
const CLOSE_FADE_MS: u32 = 130;

const ONE: Vector3 = Vector3 {
    X: 1.0,
    Y: 1.0,
    Z: 1.0,
};
const ZERO_SCALE: Vector3 = Vector3 {
    X: 0.0,
    Y: 0.0,
    Z: 1.0,
};

pub struct Animator {
    compositor: Compositor,
    open_scale: Vector3KeyFrameAnimation,
    open_opacity: ScalarKeyFrameAnimation,
    close_scale: Vector3KeyFrameAnimation,
    close_opacity: ScalarKeyFrameAnimation,
    fade_in: ScalarKeyFrameAnimation,
    fade_out: ScalarKeyFrameAnimation,
}

fn ms(value: u32) -> TimeSpan {
    TimeSpan {
        Duration: i64::from(value) * 10_000, // 100 ns ticks
    }
}

fn cubic(c: &Compositor, x1: f32, y1: f32, x2: f32, y2: f32) -> Result<CompositionEasingFunction> {
    let f =
        c.CreateCubicBezierEasingFunction(Vector2 { X: x1, Y: y1 }, Vector2 { X: x2, Y: y2 })?;
    f.cast()
}

impl Animator {
    pub fn new(c: &Compositor, cfg: &config::Animation) -> Result<Self> {
        let linear: CompositionEasingFunction = c.CreateLinearEasingFunction()?.cast()?;
        // cubic-bezier(0.2, 1.25, 0.35, 1): fast start, slight overshoot.
        let overshoot = cubic(c, 0.2, cfg.overshoot, 0.35, 1.0)?;
        // cubic-bezier(0.4, 0, 1, 1): ease-in, no overshoot.
        let ease_in = cubic(c, 0.4, 0.0, 1.0, 1.0)?;

        let open_ms = cfg.open_ms.max(1);
        let close_ms = cfg.close_ms.max(1);

        let open_scale = c.CreateVector3KeyFrameAnimation()?;
        open_scale.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        open_scale.InsertKeyFrameWithEasingFunction(1.0, ONE, &overshoot)?;
        open_scale.SetDuration(ms(open_ms))?;

        let open_opacity = c.CreateScalarKeyFrameAnimation()?;
        open_opacity.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        open_opacity.InsertKeyFrameWithEasingFunction(1.0, 1.0, &linear)?;
        open_opacity.SetDuration(ms(OPEN_FADE_MS.min(open_ms)))?;

        let close_scale = c.CreateVector3KeyFrameAnimation()?;
        close_scale.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        close_scale.InsertKeyFrameWithEasingFunction(1.0, ZERO_SCALE, &ease_in)?;
        close_scale.SetDuration(ms(close_ms))?;

        // Hold the opacity, then fade during the last CLOSE_FADE_MS.
        let close_opacity = c.CreateScalarKeyFrameAnimation()?;
        let hold = close_ms.saturating_sub(CLOSE_FADE_MS) as f32 / close_ms as f32;
        close_opacity.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        if hold > 0.0 {
            close_opacity.InsertExpressionKeyFrame(hold, h!("this.StartingValue"))?;
        }
        close_opacity.InsertKeyFrameWithEasingFunction(1.0, 0.0, &linear)?;
        close_opacity.SetDuration(ms(close_ms))?;

        let fade = |to: f32| -> Result<ScalarKeyFrameAnimation> {
            let a = c.CreateScalarKeyFrameAnimation()?;
            a.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            a.InsertKeyFrameWithEasingFunction(1.0, to, &linear)?;
            a.SetDuration(ms(REDUCED_MS))?;
            Ok(a)
        };

        Ok(Self {
            compositor: c.clone(),
            open_scale,
            open_opacity,
            close_scale,
            close_opacity,
            fade_in: fade(1.0)?,
            fade_out: fade(0.0)?,
        })
    }

    /// The fully closed state: used before opening a hidden window.
    pub fn reset_closed(visual: &Visual) -> Result<()> {
        visual.StopAnimation(h!("Scale"))?;
        visual.StopAnimation(h!("Opacity"))?;
        visual.SetScale(ZERO_SCALE)?;
        visual.SetOpacity(0.0)
    }

    pub fn open(&self, visual: &Visual, reduced_motion: bool) -> Result<()> {
        if reduced_motion {
            visual.StopAnimation(h!("Scale"))?;
            visual.SetScale(ONE)?;
            return visual.StartAnimation(h!("Opacity"), &self.fade_in);
        }
        visual.StartAnimation(h!("Scale"), &self.open_scale)?;
        visual.StartAnimation(h!("Opacity"), &self.open_opacity)
    }

    /// Starts the close animation. The returned batch's `Completed` event
    /// fires when it ends (or when a reopen interrupts it).
    pub fn close(&self, visual: &Visual, reduced_motion: bool) -> Result<CompositionScopedBatch> {
        let batch = self
            .compositor
            .CreateScopedBatch(CompositionBatchTypes::Animation)?;
        if reduced_motion {
            visual.StartAnimation(h!("Opacity"), &self.fade_out)?;
        } else {
            visual.StartAnimation(h!("Scale"), &self.close_scale)?;
            visual.StartAnimation(h!("Opacity"), &self.close_opacity)?;
        }
        batch.End()?;
        Ok(batch)
    }
}
