//! View transforms in physical screen pixels. Convert logical/DIP coordinates
//! at the UI boundary, not here. Source coordinates are integer video pixels.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalOffset {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PhysicalRect {
    /// Half-open bounds: left/top included, right/bottom excluded.
    pub fn contains(self, point: PhysicalPoint) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }

    fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
            && (self.x + self.width).is_finite()
            && (self.y + self.height).is_finite()
            && self.x + self.width > self.x
            && self.y + self.height > self.y
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePixel {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourcePoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaleMode {
    /// Fit the full source into the viewport before applying zoom and pan.
    Fit,
    /// One source pixel per physical screen pixel at zoom 1, independent of DPI.
    ActualPixels,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    EmptySource,
    InvalidBounds,
    InvalidZoom,
    InvalidPan,
    InvalidTransform,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptySource => "source dimensions must be nonzero",
            Self::InvalidBounds => "viewport bounds must be finite and positive",
            Self::InvalidZoom => "zoom must be finite and positive",
            Self::InvalidPan => "pan must be finite",
            Self::InvalidTransform => "view transform is not representable in physical pixels",
        })
    }
}

impl std::error::Error for GeometryError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    source: SourceSize,
    bounds: PhysicalRect,
    image_rect: PhysicalRect,
    scale: f64,
}

impl Viewport {
    /// Pan is a physical-pixel offset from the centered image. It is deliberately
    /// not clamped: the caller can implement its own panning policy.
    pub fn new(
        source: SourceSize,
        bounds: PhysicalRect,
        mode: ScaleMode,
        zoom: f64,
        pan: PhysicalOffset,
    ) -> Result<Self, GeometryError> {
        if source.width == 0 || source.height == 0 {
            return Err(GeometryError::EmptySource);
        }
        if !bounds.is_valid() {
            return Err(GeometryError::InvalidBounds);
        }
        if !zoom.is_finite() || zoom <= 0.0 {
            return Err(GeometryError::InvalidZoom);
        }
        if !pan.x.is_finite() || !pan.y.is_finite() {
            return Err(GeometryError::InvalidPan);
        }
        let source_width = f64::from(source.width);
        let source_height = f64::from(source.height);
        let base_scale = match mode {
            ScaleMode::Fit => (bounds.width / source_width).min(bounds.height / source_height),
            ScaleMode::ActualPixels => 1.0,
        };
        let scale = base_scale * zoom;
        let width = source_width * scale;
        let height = source_height * scale;
        let image_rect = PhysicalRect {
            x: bounds.x + (bounds.width - width) / 2.0 + pan.x,
            y: bounds.y + (bounds.height - height) / 2.0 + pan.y,
            width,
            height,
        };
        if !scale.is_finite() || scale <= 0.0 || !image_rect.is_valid() {
            return Err(GeometryError::InvalidTransform);
        }
        Ok(Self {
            source,
            bounds,
            image_rect,
            scale,
        })
    }

    pub fn source(&self) -> SourceSize {
        self.source
    }

    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }

    /// The full image rectangle; it can extend outside the clipped viewport.
    pub fn image_rect(&self) -> PhysicalRect {
        self.image_rect
    }

    /// Physical screen pixels per source pixel.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// Continuous coordinates for zoom anchors; mouse reports still use floor.
    pub fn screen_to_source_point(&self, point: PhysicalPoint) -> Option<SourcePoint> {
        if !point.x.is_finite()
            || !point.y.is_finite()
            || !self.bounds.contains(point)
            || !self.image_rect.contains(point)
        {
            return None;
        }
        Some(SourcePoint {
            x: (point.x - self.image_rect.x) / self.image_rect.width * f64::from(self.source.width),
            y: (point.y - self.image_rect.y) / self.image_rect.height
                * f64::from(self.source.height),
        })
    }

    /// Returns no pixel for letterboxing, outside the viewport, or nonfinite
    /// points. Valid points use floor, never nearest-neighbor rounding.
    pub fn screen_to_source(&self, point: PhysicalPoint) -> Option<SourcePixel> {
        let point = self.screen_to_source_point(point)?;
        Some(SourcePixel {
            x: (point.x.floor() as u32).min(self.source.width - 1),
            y: (point.y.floor() as u32).min(self.source.height - 1),
        })
    }

    /// Explicit native-pixel views align the actual transform, not just painting.
    pub fn aligned_to_physical_pixels(mut self) -> Self {
        fn align(origin: f64, start: f64, extent: f64, image: f64) -> f64 {
            let rounded = origin.round();
            let minimum = (start + extent - image).ceil();
            let maximum = start.floor();
            if image >= extent && minimum <= maximum {
                rounded.clamp(minimum, maximum)
            } else {
                rounded
            }
        }
        self.image_rect.x = align(
            self.image_rect.x,
            self.bounds.x,
            self.bounds.width,
            self.image_rect.width,
        );
        self.image_rect.y = align(
            self.image_rect.y,
            self.bounds.y,
            self.bounds.height,
            self.image_rect.height,
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: SourceSize = SourceSize {
        width: 1920,
        height: 1080,
    };
    const SQUARE: PhysicalRect = PhysicalRect {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 1000.0,
    };

    fn point(x: f64, y: f64) -> PhysicalPoint {
        PhysicalPoint { x, y }
    }

    fn fit() -> Viewport {
        Viewport::new(HD, SQUARE, ScaleMode::Fit, 1.0, PhysicalOffset::default()).unwrap()
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn continuous_points_keep_subpixel_anchor_precision() {
        let view = fit();
        let pixel = SourcePoint {
            x: 417.375,
            y: 511.625,
        };
        let image = view.image_rect();
        let screen = point(
            image.x + pixel.x * view.scale(),
            image.y + pixel.y * view.scale(),
        );
        let actual = view.screen_to_source_point(screen).unwrap();
        close(actual.x, pixel.x);
        close(actual.y, pixel.y);
        assert_eq!(
            view.screen_to_source(screen),
            Some(SourcePixel { x: 417, y: 511 })
        );
    }

    #[test]
    fn fit_retains_fifteen_source_columns_at_each_edge() {
        let view = fit();
        let image = view.image_rect();
        for x in (0..15).chain(1905..1920) {
            let screen = point(
                image.x + (f64::from(x) + 0.5) * view.scale(),
                image.y + 30.5 * view.scale(),
            );
            assert_eq!(
                view.screen_to_source(screen),
                Some(SourcePixel { x, y: 30 })
            );
        }
    }

    #[test]
    fn explicit_pixel_alignment_keeps_unit_scale_and_authoritative_mapping() {
        for dpi in [1.0, 1.25, 1.5, 2.0] {
            let bounds = PhysicalRect {
                x: 25.25 * dpi,
                y: 61.125 * dpi,
                width: 1001.0,
                height: 701.0,
            };
            let view = Viewport::new(
                HD,
                bounds,
                ScaleMode::ActualPixels,
                1.0,
                PhysicalOffset::default(),
            )
            .unwrap()
            .aligned_to_physical_pixels();
            let image = view.image_rect();
            close(view.scale(), 1.0);
            close(image.x.fract(), 0.0);
            close(image.y.fract(), 0.0);
            close(image.width, 1920.0);
            close(image.height, 1080.0);
            let screen = point(bounds.x + 500.0, bounds.y + 350.0);
            assert_eq!(
                view.screen_to_source(screen),
                Some(SourcePixel {
                    x: (screen.x - image.x).floor() as u32,
                    y: (screen.y - image.y).floor() as u32,
                })
            );
            assert!(image.x <= bounds.x && image.x + image.width >= bounds.x + bounds.width);
        }
    }

    #[test]
    fn hd_fits_square_with_letterbox_bars() {
        let view = fit();
        let rect = view.image_rect();
        close(view.scale(), 1000.0 / 1920.0);
        close(rect.x, 0.0);
        close(rect.y, 218.75);
        close(rect.width, 1000.0);
        close(rect.height, 562.5);
        assert_eq!(
            view.screen_to_source(point(0.0, 218.75)),
            Some(SourcePixel { x: 0, y: 0 })
        );
        assert_eq!(
            view.screen_to_source(point(500.0, 500.0)),
            Some(SourcePixel { x: 960, y: 540 })
        );
        assert_eq!(
            view.screen_to_source(point(999.99, 781.24)),
            Some(SourcePixel { x: 1919, y: 1079 })
        );
        for point in [
            point(0.0, 218.74),
            point(500.0, 781.25),
            point(1000.0, 500.0),
            point(-0.01, 500.0),
        ] {
            assert_eq!(view.screen_to_source(point), None);
        }
    }

    #[test]
    fn fit_mapping_at_100_125_150_200_percent_dpi() {
        for dpi_scale in [1.0, 1.25, 1.5, 2.0] {
            // The UI converts its logical bounds and pointer into physical pixels.
            let bounds = PhysicalRect {
                x: 80.0 * dpi_scale,
                y: 40.0 * dpi_scale,
                width: 1000.0 * dpi_scale,
                height: 1000.0 * dpi_scale,
            };
            let view =
                Viewport::new(HD, bounds, ScaleMode::Fit, 1.0, PhysicalOffset::default()).unwrap();
            let physical = |x, y| point((80.0 + x) * dpi_scale, (40.0 + y) * dpi_scale);
            close(view.scale(), 1000.0 * dpi_scale / 1920.0);
            assert_eq!(
                view.screen_to_source(physical(500.0, 500.0)),
                Some(SourcePixel { x: 960, y: 540 })
            );
            assert_eq!(
                view.screen_to_source(physical(250.0, 359.375)),
                Some(SourcePixel { x: 480, y: 270 })
            );
            assert_eq!(view.screen_to_source(physical(10.0, 100.0)), None);
            assert_eq!(view.screen_to_source(physical(1000.0, 500.0)), None);
        }
    }

    #[test]
    fn actual_pixels_are_physical_not_dpi_scaled() {
        for dpi_scale in [1.0, 1.25, 1.5, 2.0] {
            let bounds = PhysicalRect {
                width: 1000.0 * dpi_scale,
                height: 1000.0 * dpi_scale,
                ..SQUARE
            };
            let view = Viewport::new(
                HD,
                bounds,
                ScaleMode::ActualPixels,
                1.0,
                PhysicalOffset::default(),
            )
            .unwrap();
            let rect = view.image_rect();
            close(view.scale(), 1.0);
            close(rect.width, 1920.0);
            close(rect.height, 1080.0);
            assert_eq!(
                view.screen_to_source(point(bounds.width / 2.0, bounds.height / 2.0)),
                Some(SourcePixel { x: 960, y: 540 })
            );
            let center_plus_pixel = point(bounds.width / 2.0 + 1.0, bounds.height / 2.0 + 1.0);
            assert_eq!(
                view.screen_to_source(center_plus_pixel),
                Some(SourcePixel { x: 961, y: 541 })
            );
        }
    }

    #[test]
    fn zoom_centers_then_pan_offsets_in_physical_pixels() {
        let view = Viewport::new(
            HD,
            SQUARE,
            ScaleMode::Fit,
            2.0,
            PhysicalOffset { x: 100.0, y: -50.0 },
        )
        .unwrap();
        let rect = view.image_rect();
        close(rect.x, -400.0);
        close(rect.y, -112.5);
        close(rect.width, 2000.0);
        close(rect.height, 1125.0);
        assert_eq!(
            view.screen_to_source(point(600.0, 450.0)),
            Some(SourcePixel { x: 960, y: 540 })
        );
        assert_eq!(
            view.screen_to_source(point(0.0, 0.0)),
            Some(SourcePixel { x: 384, y: 108 })
        );
        // The full image exists here, but it is clipped outside the viewport.
        assert_eq!(view.screen_to_source(point(-1.0, 450.0)), None);
    }

    #[test]
    fn panning_can_move_the_entire_image_outside_the_viewport() {
        let view = Viewport::new(
            HD,
            SQUARE,
            ScaleMode::Fit,
            1.0,
            PhysicalOffset { x: 2000.0, y: 0.0 },
        )
        .unwrap();
        assert_eq!(view.screen_to_source(point(500.0, 500.0)), None);
        assert_eq!(view.screen_to_source(point(2500.0, 500.0)), None);
    }

    #[test]
    fn portrait_source_gets_horizontal_letterboxing() {
        let source = SourceSize {
            width: 1080,
            height: 1920,
        };
        let view = Viewport::new(
            source,
            SQUARE,
            ScaleMode::Fit,
            1.0,
            PhysicalOffset::default(),
        )
        .unwrap();
        close(view.image_rect().x, 218.75);
        close(view.image_rect().y, 0.0);
        assert_eq!(
            view.screen_to_source(point(218.75, 0.0)),
            Some(SourcePixel { x: 0, y: 0 })
        );
        assert_eq!(view.screen_to_source(point(781.25, 500.0)), None);
    }

    #[test]
    fn actual_pixel_zoom_and_half_open_floor_mapping() {
        let source = SourceSize {
            width: 4,
            height: 2,
        };
        let bounds = PhysicalRect {
            x: 10.0,
            y: 20.0,
            width: 8.0,
            height: 4.0,
        };
        let view = Viewport::new(
            source,
            bounds,
            ScaleMode::ActualPixels,
            2.0,
            PhysicalOffset::default(),
        )
        .unwrap();
        assert_eq!(
            view.screen_to_source(point(10.0, 20.0)),
            Some(SourcePixel { x: 0, y: 0 })
        );
        assert_eq!(
            view.screen_to_source(point(11.999, 21.999)),
            Some(SourcePixel { x: 0, y: 0 })
        );
        assert_eq!(
            view.screen_to_source(point(12.0, 22.0)),
            Some(SourcePixel { x: 1, y: 1 })
        );
        assert_eq!(
            view.screen_to_source(point(17.999, 23.999)),
            Some(SourcePixel { x: 3, y: 1 })
        );
        assert_eq!(view.screen_to_source(point(18.0, 22.0)), None);
        assert_eq!(view.screen_to_source(point(12.0, 24.0)), None);
    }

    #[test]
    fn mapped_pixels_stay_in_bounds_across_zoom_pan_and_dpi() {
        for dpi_scale in [1.0, 1.25, 1.5, 2.0] {
            for mode in [ScaleMode::Fit, ScaleMode::ActualPixels] {
                for zoom in [0.25, 1.0, 1.75, 4.0] {
                    for pan in [
                        PhysicalOffset::default(),
                        PhysicalOffset {
                            x: 350.0,
                            y: -225.0,
                        },
                    ] {
                        let bounds = PhysicalRect {
                            width: 1000.0 * dpi_scale,
                            height: 1000.0 * dpi_scale,
                            ..SQUARE
                        };
                        let view = Viewport::new(HD, bounds, mode, zoom, pan).unwrap();
                        for x in -1..=21 {
                            for y in -1..=21 {
                                let point = point(
                                    f64::from(x) * bounds.width / 20.0,
                                    f64::from(y) * bounds.height / 20.0,
                                );
                                if let Some(pixel) = view.screen_to_source(point) {
                                    assert!(pixel.x < HD.width && pixel.y < HD.height);
                                    assert!(bounds.contains(point));
                                    assert!(view.image_rect().contains(point));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_parameters_and_nonfinite_points_are_rejected() {
        let build =
            |source, bounds, zoom, pan| Viewport::new(source, bounds, ScaleMode::Fit, zoom, pan);
        let no_pan = PhysicalOffset::default();
        for source in [
            SourceSize {
                width: 0,
                height: 1,
            },
            SourceSize {
                width: 1,
                height: 0,
            },
        ] {
            assert_eq!(
                build(source, SQUARE, 1.0, no_pan),
                Err(GeometryError::EmptySource)
            );
        }
        for width in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                build(HD, PhysicalRect { width, ..SQUARE }, 1.0, no_pan),
                Err(GeometryError::InvalidBounds)
            );
        }
        for height in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                build(HD, PhysicalRect { height, ..SQUARE }, 1.0, no_pan),
                Err(GeometryError::InvalidBounds)
            );
        }
        for zoom in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                build(HD, SQUARE, zoom, no_pan),
                Err(GeometryError::InvalidZoom)
            );
        }
        assert_eq!(
            build(
                HD,
                SQUARE,
                1.0,
                PhysicalOffset {
                    x: f64::NAN,
                    y: 0.0
                }
            ),
            Err(GeometryError::InvalidPan)
        );
        assert_eq!(
            build(
                HD,
                SQUARE,
                1.0,
                PhysicalOffset {
                    x: 0.0,
                    y: f64::INFINITY
                }
            ),
            Err(GeometryError::InvalidPan)
        );
        assert_eq!(
            build(HD, SQUARE, f64::MAX, no_pan),
            Err(GeometryError::InvalidTransform)
        );
        assert_eq!(
            build(HD, SQUARE, f64::MIN_POSITIVE, no_pan),
            Err(GeometryError::InvalidTransform)
        );
        let view = fit();
        for point in [
            point(f64::NAN, 500.0),
            point(500.0, f64::INFINITY),
            point(f64::NEG_INFINITY, 500.0),
        ] {
            assert_eq!(view.screen_to_source(point), None);
        }
    }

    #[test]
    fn unrepresentable_bounds_are_rejected() {
        for bounds in [
            PhysicalRect {
                x: f64::MAX,
                width: f64::MAX,
                ..SQUARE
            },
            PhysicalRect {
                y: f64::MAX,
                height: f64::MAX,
                ..SQUARE
            },
            PhysicalRect {
                x: f64::MAX,
                width: 1.0,
                ..SQUARE
            },
            PhysicalRect {
                x: f64::NAN,
                ..SQUARE
            },
        ] {
            assert_eq!(
                Viewport::new(HD, bounds, ScaleMode::Fit, 1.0, PhysicalOffset::default()),
                Err(GeometryError::InvalidBounds)
            );
        }
    }
}
