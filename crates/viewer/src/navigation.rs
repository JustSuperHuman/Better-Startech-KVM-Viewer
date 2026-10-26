use cart_core::geometry::{
    PhysicalOffset, PhysicalPoint, PhysicalRect, ScaleMode, SourcePoint, SourceSize, Viewport,
};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionMode {
    Control,
    Navigate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureAction {
    Acquire,
    Release,
    None,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureConditions {
    pub focused: bool,
    pub pointer_in_window: bool,
    pub pointer_in_image: bool,
    pub ready: bool,
    pub blocked: bool,
    pub host_neutral: bool,
    pub dragging: bool,
}

pub fn capture_action(
    mode: InteractionMode,
    captured: bool,
    conditions: CaptureConditions,
) -> CaptureAction {
    let eligible = mode == InteractionMode::Control
        && conditions.focused
        && conditions.pointer_in_window
        && conditions.ready
        && !conditions.blocked;
    if captured {
        if eligible && (conditions.pointer_in_image || conditions.dragging) {
            CaptureAction::None
        } else {
            CaptureAction::Release
        }
    } else if eligible && conditions.pointer_in_image && conditions.host_neutral {
        CaptureAction::Acquire
    } else {
        CaptureAction::None
    }
}

pub fn wheel_lines(value: f64, pixels: bool) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    let lines = if pixels { value / 40.0 } else { value };
    (lines != 0.0).then(|| lines.clamp(-12.0, 12.0))
}

/// Local middle-button panning. A press it accepts is owned through its
/// release, even after motion stops, so neither half reaches the target.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MiddlePan {
    #[default]
    Idle,
    Panning {
        last: PhysicalPoint,
    },
    /// Motion was cancelled; the outstanding middle release is still ours.
    Held,
}

impl MiddlePan {
    pub fn owns_release(self) -> bool {
        self != Self::Idle
    }

    pub fn active(self) -> bool {
        matches!(self, Self::Panning { .. })
    }

    /// Returns whether the press stays local. A press while a release is
    /// still owned means that release was missed: consume it without motion.
    pub fn press(&mut self, point: PhysicalPoint, eligible: bool) -> bool {
        match self {
            Self::Idle if eligible && point.x.is_finite() && point.y.is_finite() => {
                *self = Self::Panning { last: point };
                true
            }
            Self::Idle => false,
            Self::Panning { .. } | Self::Held => {
                *self = Self::Held;
                true
            }
        }
    }

    /// Physical-pixel motion since the last event, never accumulated twice.
    pub fn moved(&mut self, point: PhysicalPoint) -> Option<PhysicalOffset> {
        let Self::Panning { last } = self else {
            return None;
        };
        if !point.x.is_finite() || !point.y.is_finite() {
            *self = Self::Held;
            return None;
        }
        let delta = PhysicalOffset {
            x: point.x - last.x,
            y: point.y - last.y,
        };
        *last = point;
        Some(delta)
    }

    /// Returns whether this release belonged to a local pan.
    pub fn release(&mut self) -> bool {
        std::mem::take(self) != Self::Idle
    }

    pub fn cancel(&mut self) {
        if self.active() {
            *self = Self::Held;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    source: SourcePoint,
    screen: PhysicalPoint,
}

pub struct Navigation {
    pub mode: ScaleMode,
    pub zoom: f64,
    pub pan: PhysicalOffset,
    target_zoom: f64,
    anchor: Option<Anchor>,
    pixel_lock: bool,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            mode: ScaleMode::Fit,
            zoom: 1.0,
            pan: PhysicalOffset::default(),
            target_zoom: 1.0,
            anchor: None,
            pixel_lock: false,
        }
    }
}

impl Navigation {
    pub fn reset(&mut self, mode: ScaleMode) {
        self.mode = mode;
        self.zoom = 1.0;
        self.pan = PhysicalOffset::default();
        self.target_zoom = 1.0;
        self.anchor = None;
        self.pixel_lock = mode == ScaleMode::ActualPixels;
    }

    pub fn is_base_view(&self, mode: ScaleMode) -> bool {
        self.mode == mode
            && (self.zoom - 1.0).abs() < 1e-6
            && (self.target_zoom - 1.0).abs() < 1e-6
            && (mode != ScaleMode::ActualPixels || self.pixel_lock)
    }

    pub fn cancel(&mut self, displayed: Option<Viewport>) {
        if self.anchor.is_some()
            && let Some(view) = displayed
        {
            self.pan = effective_pan(view);
        }
        self.target_zoom = self.zoom;
        self.anchor = None;
    }

    pub fn zoom_at(&mut self, view: Viewport, point: PhysicalPoint, lines: f64) -> bool {
        if !lines.is_finite() || !view.bounds().contains(point) {
            return false;
        }
        let Some(source) = view.screen_to_source_point(point) else {
            return false;
        };
        if self.pixel_lock {
            self.pan = effective_pan(view);
            self.pixel_lock = false;
        }
        let target = (self.target_zoom * (lines.clamp(-12.0, 12.0) * 0.14).exp()).clamp(0.25, 8.0);
        if target == self.target_zoom {
            return false;
        }
        self.anchor = Some(Anchor {
            source,
            screen: point,
        });
        self.target_zoom = target;
        true
    }

    pub fn zoom_center(&mut self, view: Viewport, inward: bool) {
        let bounds = view.bounds();
        let center = PhysicalPoint {
            x: bounds.x + bounds.width / 2.0,
            y: bounds.y + bounds.height / 2.0,
        };
        let lines = 1.25_f64.ln() / 0.14 * if inward { 1.0 } else { -1.0 };
        self.zoom_at(view, center, lines);
    }

    pub fn advance(&mut self, elapsed: Duration) {
        let difference = self.target_zoom.ln() - self.zoom.ln();
        if difference.abs() <= 1e-5 {
            self.zoom = self.target_zoom;
            return;
        }
        let seconds = elapsed.as_secs_f64().min(0.1);
        let amount = 1.0 - (-seconds / 0.075).exp();
        self.zoom = (self.zoom.ln() + difference * amount).exp();
    }

    pub fn pan_by(&mut self, delta: PhysicalOffset, displayed: Option<Viewport>) {
        if !delta.x.is_finite() || !delta.y.is_finite() {
            return;
        }
        self.cancel(displayed);
        self.pan.x += delta.x;
        self.pan.y += delta.y;
    }

    pub fn viewport(&mut self, source: SourceSize, bounds: PhysicalRect) -> Option<Viewport> {
        let mut view = Viewport::new(source, bounds, self.mode, self.zoom, self.pan).ok()?;
        if let Some(anchor) = self.anchor {
            let image = view.image_rect();
            self.pan = PhysicalOffset {
                x: anchor.screen.x
                    - anchor.source.x * view.scale()
                    - (bounds.x + (bounds.width - image.width) / 2.0),
                y: anchor.screen.y
                    - anchor.source.y * view.scale()
                    - (bounds.y + (bounds.height - image.height) / 2.0),
            };
        }
        clamp_pan(&mut self.pan, view);
        view = Viewport::new(source, bounds, self.mode, self.zoom, self.pan).ok()?;
        if self.pixel_lock {
            view = view.aligned_to_physical_pixels();
        }
        if self.zoom == self.target_zoom {
            self.anchor = None;
        }
        Some(view)
    }
}

fn effective_pan(view: Viewport) -> PhysicalOffset {
    let image = view.image_rect();
    let bounds = view.bounds();
    PhysicalOffset {
        x: image.x - bounds.x - (bounds.width - image.width) / 2.0,
        y: image.y - bounds.y - (bounds.height - image.height) / 2.0,
    }
}

pub fn clamp_pan(pan: &mut PhysicalOffset, view: Viewport) {
    let image = view.image_rect();
    let bounds = view.bounds();
    let limit_x = ((image.width - bounds.width) / 2.0).max(0.0);
    let limit_y = ((image.height - bounds.height) / 2.0).max(0.0);
    pan.x = pan.x.clamp(-limit_x, limit_x);
    pan.y = pan.y.clamp(-limit_y, limit_y);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: SourceSize = SourceSize {
        width: 1920,
        height: 1080,
    };
    const BOUNDS: PhysicalRect = PhysicalRect {
        x: 25.25,
        y: 60.5,
        width: 1001.0,
        height: 701.0,
    };

    fn ready() -> CaptureConditions {
        CaptureConditions {
            focused: true,
            pointer_in_window: true,
            pointer_in_image: true,
            ready: true,
            host_neutral: true,
            ..Default::default()
        }
    }

    #[test]
    fn automatic_capture_has_explicit_gates_and_a_latched_local_mode() {
        assert_eq!(
            capture_action(InteractionMode::Control, false, ready()),
            CaptureAction::Acquire
        );
        assert_eq!(
            capture_action(InteractionMode::Navigate, false, ready()),
            CaptureAction::None
        );
        assert_eq!(
            capture_action(InteractionMode::Navigate, true, ready()),
            CaptureAction::Release
        );
        let cases = [
            CaptureConditions {
                focused: false,
                ..ready()
            },
            CaptureConditions {
                pointer_in_window: false,
                ..ready()
            },
            CaptureConditions {
                pointer_in_image: false,
                ..ready()
            },
            CaptureConditions {
                ready: false,
                ..ready()
            },
            CaptureConditions {
                blocked: true,
                ..ready()
            },
            CaptureConditions {
                host_neutral: false,
                ..ready()
            },
        ];
        for conditions in cases {
            assert_eq!(
                capture_action(InteractionMode::Control, false, conditions),
                CaptureAction::None
            );
        }
    }

    #[test]
    fn drag_out_preserves_only_existing_ownership_within_the_window() {
        let dragging = CaptureConditions {
            pointer_in_image: false,
            dragging: true,
            ..ready()
        };
        assert_eq!(
            capture_action(InteractionMode::Control, true, dragging),
            CaptureAction::None
        );
        assert_eq!(
            capture_action(
                InteractionMode::Control,
                true,
                CaptureConditions {
                    dragging: false,
                    ..dragging
                }
            ),
            CaptureAction::Release
        );
        for conditions in [
            CaptureConditions {
                pointer_in_window: false,
                ..dragging
            },
            CaptureConditions {
                focused: false,
                ..dragging
            },
            CaptureConditions {
                blocked: true,
                ..dragging
            },
            CaptureConditions {
                ready: false,
                ..dragging
            },
        ] {
            assert_eq!(
                capture_action(InteractionMode::Control, true, conditions),
                CaptureAction::Release
            );
        }
    }

    #[test]
    fn blocked_idle_does_not_repeatedly_cancel_paste_or_special_keys() {
        for _ in 0..100 {
            assert_eq!(
                capture_action(
                    InteractionMode::Control,
                    false,
                    CaptureConditions {
                        blocked: true,
                        ..ready()
                    }
                ),
                CaptureAction::None
            );
        }
    }

    #[test]
    fn high_resolution_wheel_input_is_fractional_finite_and_bounded() {
        assert_eq!(wheel_lines(4.0, true), Some(0.1));
        assert_eq!(wheel_lines(0.125, false), Some(0.125));
        assert_eq!(wheel_lines(1e9, false), Some(12.0));
        assert_eq!(wheel_lines(-1e9, true), Some(-12.0));
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0] {
            assert_eq!(wheel_lines(value, false), None);
        }
    }

    #[test]
    fn fractional_anchor_stays_put_through_animation_at_every_dpi() {
        for dpi in [1.0, 1.25, 1.5, 2.0] {
            let bounds = PhysicalRect {
                x: BOUNDS.x * dpi,
                y: BOUNDS.y * dpi,
                width: BOUNDS.width * dpi,
                height: BOUNDS.height * dpi,
            };
            let mut navigation = Navigation::default();
            navigation.reset(ScaleMode::ActualPixels);
            navigation.zoom = 2.0;
            navigation.target_zoom = 2.0;
            navigation.pixel_lock = false;
            let initial = navigation.viewport(SOURCE, bounds).unwrap();
            let point = PhysicalPoint {
                x: bounds.x + bounds.width * 0.55,
                y: bounds.y + bounds.height * 0.53,
            };
            let source = initial.screen_to_source_point(point).unwrap();
            assert!(navigation.zoom_at(initial, point, 2.0));
            for _ in 0..60 {
                navigation.advance(Duration::from_millis(16));
                let view = navigation.viewport(SOURCE, bounds).unwrap();
                let actual = view.screen_to_source_point(point).unwrap();
                assert!((actual.x - source.x).abs() < 1e-7);
                assert!((actual.y - source.y).abs() < 1e-7);
            }
        }
    }

    #[test]
    fn zoom_converges_independently_of_frame_rate_without_overshoot() {
        let run = |frames: u32| {
            let mut navigation = Navigation::default();
            let view = navigation.viewport(SOURCE, BOUNDS).unwrap();
            navigation.zoom_center(view, true);
            for _ in 0..frames {
                navigation.advance(Duration::from_secs_f64(1.0 / f64::from(frames)));
            }
            navigation.zoom
        };
        assert!((run(60) - run(144)).abs() < 1e-5);
        assert!((run(60) - 1.25).abs() < 1e-5);
    }

    #[test]
    fn explicit_native_size_aligns_but_animated_zoom_is_not_snapped() {
        let mut navigation = Navigation::default();
        navigation.reset(ScaleMode::ActualPixels);
        let view = navigation.viewport(SOURCE, BOUNDS).unwrap();
        assert_eq!(view.scale(), 1.0);
        assert_eq!(view.image_rect().x.fract(), 0.0);
        assert_eq!(view.image_rect().y.fract(), 0.0);
        navigation.zoom_at(view, PhysicalPoint { x: 500.0, y: 400.0 }, 0.1);
        let unchanged = navigation.viewport(SOURCE, BOUNDS).unwrap();
        assert_eq!(unchanged.image_rect(), view.image_rect());
        navigation.advance(Duration::from_millis(16));
        let animated = navigation.viewport(SOURCE, BOUNDS).unwrap();
        assert!(animated.scale() > 1.0);
        assert!(!navigation.pixel_lock);
        navigation.zoom = 1.0;
        navigation.target_zoom = 1.0;
        assert!(!navigation.is_base_view(ScaleMode::ActualPixels));
        navigation.reset(ScaleMode::ActualPixels);
        assert!(navigation.is_base_view(ScaleMode::ActualPixels));
    }

    #[test]
    fn native_panning_retains_fractional_motion_and_resets_cancel_zoom() {
        let mut navigation = Navigation::default();
        navigation.reset(ScaleMode::ActualPixels);
        let initial = navigation.viewport(SOURCE, BOUNDS).unwrap();
        for _ in 0..4 {
            navigation.pan_by(PhysicalOffset { x: 0.3, y: 0.3 }, None);
            navigation.viewport(SOURCE, BOUNDS).unwrap();
        }
        let moved = navigation.viewport(SOURCE, BOUNDS).unwrap();
        assert_eq!(moved.image_rect().x - initial.image_rect().x, 1.0);
        assert!((navigation.pan.x - 1.2).abs() < 1e-9);
        navigation.zoom_center(moved, true);
        navigation.reset(ScaleMode::Fit);
        navigation.advance(Duration::from_secs(10));
        assert_eq!(navigation.zoom, 1.0);
        assert_eq!(navigation.pan, PhysicalOffset::default());
    }

    #[test]
    fn middle_pan_owns_each_release_and_never_restarts_cancelled_motion() {
        let point = |x, y| PhysicalPoint { x, y };
        let mut pan = MiddlePan::default();
        assert!(!pan.press(point(10.0, 10.0), false));
        assert_eq!(pan, MiddlePan::Idle);
        assert!(!pan.release(), "an unowned release is not consumed");

        assert!(pan.press(point(10.0, 10.0), true));
        assert!(pan.owns_release() && pan.active());
        assert_eq!(
            pan.moved(point(13.5, 8.25)),
            Some(PhysicalOffset { x: 3.5, y: -1.75 })
        );
        assert_eq!(
            pan.moved(point(14.0, 8.25)),
            Some(PhysicalOffset { x: 0.5, y: 0.0 })
        );
        assert!(pan.release());
        assert_eq!(pan, MiddlePan::Idle);

        // Focus loss, F12, resize or leaving the window stop motion, but the
        // eventual release still belongs to the pan and re-entry cannot resume.
        assert!(pan.press(point(0.0, 0.0), true));
        pan.cancel();
        assert!(pan.owns_release() && !pan.active());
        assert_eq!(pan.moved(point(50.0, 50.0)), None);
        assert!(pan.release());

        // A missed release fails closed: the next press is consumed without
        // motion, and its own release re-arms ordinary input.
        assert!(pan.press(point(0.0, 0.0), true));
        assert!(pan.press(point(5.0, 5.0), false));
        assert_eq!(pan, MiddlePan::Held);
        assert_eq!(pan.moved(point(9.0, 9.0)), None);
        assert!(pan.release());
        assert!(!pan.owns_release());

        assert!(pan.press(point(0.0, 0.0), true));
        assert_eq!(pan.moved(point(f64::NAN, 0.0)), None);
        assert_eq!(pan, MiddlePan::Held);
    }

    #[test]
    fn middle_pan_deltas_move_the_view_like_primary_drag_at_every_dpi() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let bounds = PhysicalRect {
                x: 0.0,
                y: 0.0,
                width: 800.0 * scale,
                height: 500.0 * scale,
            };
            let mut navigation = Navigation::default();
            navigation.reset(ScaleMode::ActualPixels);
            let start = navigation.viewport(SOURCE, bounds).unwrap();
            let mut pan = MiddlePan::default();
            assert!(pan.press(PhysicalPoint { x: 400.0, y: 300.0 }, true));
            for step in 1..=8 {
                let delta = pan
                    .moved(PhysicalPoint {
                        x: 400.0 + f64::from(step) * 0.75,
                        y: 300.0,
                    })
                    .unwrap();
                navigation.pan_by(delta, Some(start));
            }
            let view = navigation.viewport(SOURCE, bounds).unwrap();
            // Winit positions are already physical: six pixels, not 6 x DPI.
            assert!((navigation.pan.x - 6.0).abs() < 1e-9);
            assert_eq!(view.scale(), 1.0);
            assert_eq!(view.image_rect().x, view.image_rect().x.round());
        }
    }
}
