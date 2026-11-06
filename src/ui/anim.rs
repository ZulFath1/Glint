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
    AnimationDelayBehavior, AnimationIterationBehavior, CompositionBatchTypes,
    CompositionEasingFunction, CompositionScopedBatch, Compositor, ScalarKeyFrameAnimation,
    Vector3KeyFrameAnimation, Visual,
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
    stagger: Stagger,
    pop: Pop,
    caret_blink: ScalarKeyFrameAnimation,
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
            stagger: Stagger::new(c, &overshoot, &linear, cfg.stagger_ms)?,
            pop: Pop::new(c, &overshoot, &ease_in, &linear)?,
            caret_blink: caret_blink(c, &linear)?,
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

    /// Pops the pinned icons in, one after another. `visuals` are the inner
    /// (animated) visual of each icon, in order.
    pub fn stagger_in(&self, visuals: &[Visual]) -> Result<()> {
        let s = &self.stagger;
        for (i, v) in visuals.iter().enumerate() {
            let delay = ms(STAGGER_FIRST_MS + i as u32 * s.step_ms);
            s.opacity.SetDelayTime(delay)?;
            s.offset.SetDelayTime(delay)?;
            s.scale.SetDelayTime(delay)?;
            v.StartAnimation(h!("Opacity"), &s.opacity)?;
            v.StartAnimation(h!("Offset"), &s.offset)?;
            v.StartAnimation(h!("Scale"), &s.scale)?;
        }
        Ok(())
    }

    /// Shows the icons at rest, with no animation (reduced motion).
    pub fn stagger_done(visuals: &[Visual]) -> Result<()> {
        for v in visuals {
            v.StopAnimation(h!("Opacity"))?;
            v.StopAnimation(h!("Offset"))?;
            v.StopAnimation(h!("Scale"))?;
            v.SetOpacity(1.0)?;
            v.SetOffset(Vector3::zero())?;
            v.SetScale(ONE)?;
        }
        Ok(())
    }

    /// Pops a small popup in from wherever it is now.
    pub fn pop_in(&self, visual: &Visual, reduced_motion: bool) -> Result<()> {
        if reduced_motion {
            visual.StopAnimation(h!("Scale"))?;
            visual.SetScale(ONE)?;
            return visual.StartAnimation(h!("Opacity"), &self.fade_in);
        }
        visual.StartAnimation(h!("Scale"), &self.pop.in_scale)?;
        visual.StartAnimation(h!("Opacity"), &self.pop.in_opacity)
    }

    /// Shrinks and fades a small popup out, from wherever it is now.
    pub fn pop_out(&self, visual: &Visual, reduced_motion: bool) -> Result<()> {
        if reduced_motion {
            return visual.StartAnimation(h!("Opacity"), &self.fade_out);
        }
        visual.StartAnimation(h!("Scale"), &self.pop.out_scale)?;
        visual.StartAnimation(h!("Opacity"), &self.pop.out_opacity)
    }

    /// Restarts the caret blink, beginning "on" (call after each keystroke).
    pub fn blink(&self, caret: &Visual) -> Result<()> {
        caret.StartAnimation(h!("Opacity"), &self.caret_blink)
    }

    /// Stops the blink (menu hidden, or focus elsewhere): no work for DWM.
    pub fn stop_blink(caret: &Visual, visible: bool) -> Result<()> {
        caret.StopAnimation(h!("Opacity"))?;
        caret.SetOpacity(if visible { 1.0 } else { 0.0 })
    }
}

// ---------------------------------------------------------------------------
// Pinned icons: the staggered pop-in
// ---------------------------------------------------------------------------

/// The first icon starts this long after the menu starts opening.
const STAGGER_FIRST_MS: u32 = 70;
/// Each icon's own animation.
const STAGGER_MS: u32 = 300;
const STAGGER_FADE_MS: u32 = 160;
/// Icons rise this far (DIPs) and grow from this scale.
const STAGGER_RISE: f32 = 10.0;
const STAGGER_FROM_SCALE: f32 = 0.85;

/// Three animations reused for every icon. Composition copies an animation
/// when it starts, so changing `DelayTime` for the next icon is safe.
struct Stagger {
    opacity: ScalarKeyFrameAnimation,
    offset: Vector3KeyFrameAnimation,
    scale: Vector3KeyFrameAnimation,
    step_ms: u32,
}

impl Stagger {
    fn new(
        c: &Compositor,
        overshoot: &CompositionEasingFunction,
        linear: &CompositionEasingFunction,
        step_ms: u32,
    ) -> Result<Self> {
        let opacity = c.CreateScalarKeyFrameAnimation()?;
        opacity.InsertKeyFrame(0.0, 0.0)?;
        opacity.InsertKeyFrameWithEasingFunction(1.0, 1.0, linear)?;
        opacity.SetDuration(ms(STAGGER_FADE_MS))?;

        let offset = c.CreateVector3KeyFrameAnimation()?;
        offset.InsertKeyFrame(
            0.0,
            Vector3 {
                X: 0.0,
                Y: STAGGER_RISE,
                Z: 0.0,
            },
        )?;
        offset.InsertKeyFrameWithEasingFunction(1.0, Vector3::zero(), overshoot)?;
        offset.SetDuration(ms(STAGGER_MS))?;

        let scale = c.CreateVector3KeyFrameAnimation()?;
        scale.InsertKeyFrame(
            0.0,
            Vector3 {
                X: STAGGER_FROM_SCALE,
                Y: STAGGER_FROM_SCALE,
                Z: 1.0,
            },
        )?;
        scale.InsertKeyFrameWithEasingFunction(1.0, ONE, overshoot)?;
        scale.SetDuration(ms(STAGGER_MS))?;

        // Hold the "before" values during the delay, so icons that have not
        // started yet stay hidden instead of showing at full size.
        opacity.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        offset.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        scale.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        Ok(Self {
            opacity,
            offset,
            scale,
            step_ms,
        })
    }
}

// ---------------------------------------------------------------------------
// Small popups (the power strip): pop in / pop out
// ---------------------------------------------------------------------------

const POP_IN_MS: u32 = 220;
const POP_IN_FADE_MS: u32 = 90;
const POP_OUT_MS: u32 = 120;
/// A popup grows from (and shrinks back to) this scale.
pub const POP_FROM_SCALE: f32 = 0.9;

struct Pop {
    in_scale: Vector3KeyFrameAnimation,
    in_opacity: ScalarKeyFrameAnimation,
    out_scale: Vector3KeyFrameAnimation,
    out_opacity: ScalarKeyFrameAnimation,
}

impl Pop {
    fn new(
        c: &Compositor,
        overshoot: &CompositionEasingFunction,
        ease_in: &CompositionEasingFunction,
        linear: &CompositionEasingFunction,
    ) -> Result<Self> {
        let small = Vector3 {
            X: POP_FROM_SCALE,
            Y: POP_FROM_SCALE,
            Z: 1.0,
        };
        let in_scale = c.CreateVector3KeyFrameAnimation()?;
        in_scale.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        in_scale.InsertKeyFrameWithEasingFunction(1.0, ONE, overshoot)?;
        in_scale.SetDuration(ms(POP_IN_MS))?;

        let in_opacity = c.CreateScalarKeyFrameAnimation()?;
        in_opacity.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        in_opacity.InsertKeyFrameWithEasingFunction(1.0, 1.0, linear)?;
        in_opacity.SetDuration(ms(POP_IN_FADE_MS))?;

        let out_scale = c.CreateVector3KeyFrameAnimation()?;
        out_scale.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        out_scale.InsertKeyFrameWithEasingFunction(1.0, small, ease_in)?;
        out_scale.SetDuration(ms(POP_OUT_MS))?;

        let out_opacity = c.CreateScalarKeyFrameAnimation()?;
        out_opacity.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        out_opacity.InsertKeyFrameWithEasingFunction(1.0, 0.0, linear)?;
        out_opacity.SetDuration(ms(POP_OUT_MS))?;
        Ok(Self {
            in_scale,
            in_opacity,
            out_scale,
            out_opacity,
        })
    }
}

// ---------------------------------------------------------------------------
// The search box caret: 530 ms on, 530 ms off, forever
// ---------------------------------------------------------------------------

const CARET_HALF_MS: u32 = 530;

fn caret_blink(
    c: &Compositor,
    linear: &CompositionEasingFunction,
) -> Result<ScalarKeyFrameAnimation> {
    // On for the first half, off for the second. The tiny 0.5 → 0.501 step
    // lasts about 1 ms, less than a frame, so it looks like an instant blink.
    let a = c.CreateScalarKeyFrameAnimation()?;
    a.InsertKeyFrame(0.0, 1.0)?;
    a.InsertKeyFrameWithEasingFunction(0.5, 1.0, linear)?;
    a.InsertKeyFrameWithEasingFunction(0.501, 0.0, linear)?;
    a.InsertKeyFrameWithEasingFunction(1.0, 0.0, linear)?;
    a.SetDuration(ms(2 * CARET_HALF_MS))?;
    a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
    Ok(a)
}
