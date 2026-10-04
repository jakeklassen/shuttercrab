//! How the main window's screenshot sits in its canvas: the zoom, and
//! which part shows when it is bigger than the canvas. Only geometry, in
//! logical pixels, so it can be tested alone.
//!
//! Zoom is relative to full size, a screenshot pixel per screen pixel (1.0).
//! A new screenshot is fitted: shown whole, scaled down to fit the canvas
//! but never up.

/// The zoom steps Ctrl+plus and Ctrl+minus go through.
const STEPS: [f32; 14] = [
    0.1,
    0.25,
    1. / 3.,
    0.5,
    2. / 3.,
    0.75,
    1.,
    1.25,
    1.5,
    2.,
    3.,
    4.,
    6.,
    8.,
];

/// The space between a fitted screenshot and the canvas's edges, logical
/// pixels: its padding and a one-pixel border.
pub const MARGIN: f32 = 17.;

/// A point or a size, logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Xy {
    pub x: f32,
    pub y: f32,
}

impl Xy {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Where the screenshot is drawn in the canvas: its top-left corner and
/// size, logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub origin: Xy,
    pub size: Xy,
}

/// The screenshot's zoom and position in a canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShotView {
    /// The screenshot's size at full size, logical pixels.
    image: Xy,
    /// `None` while fitted; otherwise the zoom.
    zoom: Option<f32>,
    /// The screenshot point, at full size, shown at the canvas's centre.
    centre: Xy,
}

impl ShotView {
    /// A screenshot of `image` logical pixels at full size, fitted.
    pub fn new(image: Xy) -> Self {
        Self {
            image,
            zoom: None,
            centre: Xy::new(image.x / 2., image.y / 2.),
        }
    }

    /// The zoom that shows the whole screenshot in `canvas`, at most full
    /// size.
    fn fit(&self, canvas: Xy) -> f32 {
        let room = Xy::new(canvas.x - 2. * MARGIN, canvas.y - 2. * MARGIN);
        (room.x / self.image.x)
            .min(room.y / self.image.y)
            .clamp(0.01, 1.)
    }

    /// The zoom now: fitted, or chosen.
    pub fn zoom(&self, canvas: Xy) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit(canvas))
    }

    pub fn is_fitted(&self) -> bool {
        self.zoom.is_none()
    }

    /// Whether the screenshot is bigger than `canvas`, so it can be moved.
    pub fn can_pan(&self, canvas: Xy) -> bool {
        let zoom = self.zoom(canvas);
        self.image.x * zoom > canvas.x || self.image.y * zoom > canvas.y
    }

    /// Where the screenshot is drawn in `canvas`.
    pub fn placement(&self, canvas: Xy) -> Placement {
        let zoom = self.zoom(canvas);
        let size = Xy::new(self.image.x * zoom, self.image.y * zoom);
        let centre = self.clamped_centre(canvas);
        Placement {
            origin: Xy::new(
                canvas.x / 2. - centre.x * zoom,
                canvas.y / 2. - centre.y * zoom,
            ),
            size,
        }
    }

    /// The centre, kept so the screenshot covers the canvas on each axis it
    /// is bigger than, and centred on each axis it fits.
    fn clamped_centre(&self, canvas: Xy) -> Xy {
        let zoom = self.zoom(canvas);
        let axis = |centre: f32, image: f32, canvas: f32| {
            let half = canvas / 2. / zoom;
            if image * zoom <= canvas {
                image / 2.
            } else {
                centre.clamp(half, image - half)
            }
        };
        Xy::new(
            axis(self.centre.x, self.image.x, canvas.x),
            axis(self.centre.y, self.image.y, canvas.y),
        )
    }

    /// Show the whole screenshot again.
    pub fn fit_to_canvas(&mut self) {
        *self = Self::new(self.image);
    }

    /// Zoom to `zoom`, keeping the screenshot point under `at` (a canvas
    /// point; its centre if `None`) where it is.
    pub fn zoom_to(&mut self, zoom: f32, at: Option<Xy>, canvas: Xy) {
        let before = self.zoom(canvas);
        let centre = self.clamped_centre(canvas);
        let at = at.unwrap_or(Xy::new(canvas.x / 2., canvas.y / 2.));
        // The screenshot point under `at`, which stays there.
        let point = Xy::new(
            centre.x + (at.x - canvas.x / 2.) / before,
            centre.y + (at.y - canvas.y / 2.) / before,
        );
        let zoom = zoom.clamp(STEPS[0], STEPS[STEPS.len() - 1]);
        self.zoom = Some(zoom);
        self.centre = Xy::new(
            point.x - (at.x - canvas.x / 2.) / zoom,
            point.y - (at.y - canvas.y / 2.) / zoom,
        );
        self.centre = self.clamped_centre(canvas);
    }

    /// One step in (`steps` > 0) or out, around `at` as [`Self::zoom_to`].
    pub fn step(&mut self, steps: i32, at: Option<Xy>, canvas: Xy) {
        let now = self.zoom(canvas);
        let next = if steps > 0 {
            STEPS.into_iter().find(|s| *s > now * 1.001)
        } else {
            STEPS.into_iter().rev().find(|s| *s < now / 1.001)
        };
        if let Some(zoom) = next {
            self.zoom_to(zoom, at, canvas);
        }
    }

    /// Move the screenshot by `by` logical pixels on screen, as far as it
    /// goes.
    pub fn pan(&mut self, by: Xy, canvas: Xy) {
        let zoom = self.zoom(canvas);
        let centre = self.clamped_centre(canvas);
        self.centre = Xy::new(centre.x - by.x / zoom, centre.y - by.y / zoom);
        self.centre = self.clamped_centre(canvas);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: Xy = Xy { x: 1000., y: 600. };

    #[test]
    fn a_big_screenshot_is_fitted_and_centred() {
        let view = ShotView::new(Xy::new(2560., 1440.));
        let zoom = view.zoom(CANVAS);
        // Width limits: 1000 less the margins over 2560.
        assert!((zoom - (1000. - 34.) / 2560.).abs() < 1e-6);
        let placed = view.placement(CANVAS);
        assert!((placed.origin.x - MARGIN).abs() < 1e-3);
        assert!((placed.origin.y - (600. - 1440. * zoom) / 2.).abs() < 1e-3);
        assert!(!view.can_pan(CANVAS));
    }

    #[test]
    fn a_small_screenshot_stays_at_full_size() {
        let view = ShotView::new(Xy::new(300., 200.));
        assert_eq!(view.zoom(CANVAS), 1.);
        assert_eq!(view.placement(CANVAS).origin, Xy::new(350., 200.));
    }

    #[test]
    fn steps_go_through_the_list_and_fit_comes_back() {
        let mut view = ShotView::new(Xy::new(2560., 1440.));
        // Fitted at about 0.39: in goes to a half, out to a third.
        view.step(1, None, CANVAS);
        assert_eq!(view.zoom(CANVAS), 0.5);
        view.step(-1, None, CANVAS);
        view.step(-1, None, CANVAS);
        assert_eq!(view.zoom(CANVAS), 0.25);
        for _ in 0..20 {
            view.step(1, None, CANVAS);
        }
        assert_eq!(view.zoom(CANVAS), 8.);
        view.fit_to_canvas();
        assert!(view.is_fitted());
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let mut view = ShotView::new(Xy::new(2560., 1440.));
        view.zoom_to(1., None, CANVAS);
        let at = Xy::new(700., 400.);
        let before = view.placement(CANVAS);
        let point = Xy::new(at.x - before.origin.x, at.y - before.origin.y);
        view.zoom_to(2., Some(at), CANVAS);
        let after = view.placement(CANVAS);
        assert!((after.origin.x + point.x * 2. - at.x).abs() < 1e-3);
        assert!((after.origin.y + point.y * 2. - at.y).abs() < 1e-3);
    }

    #[test]
    fn panning_stops_at_the_edges() {
        let mut view = ShotView::new(Xy::new(2560., 1440.));
        view.zoom_to(1., None, CANVAS);
        assert!(view.can_pan(CANVAS));
        view.pan(Xy::new(100_000., 100_000.), CANVAS);
        assert_eq!(view.placement(CANVAS).origin, Xy::new(0., 0.));
        view.pan(Xy::new(-100_000., -100_000.), CANVAS);
        assert_eq!(
            view.placement(CANVAS).origin,
            Xy::new(1000. - 2560., 600. - 1440.)
        );
    }
}
