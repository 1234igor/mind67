//! Free camera over an infinite world: pan, zoom-at-cursor, eased fly-to,
//! and post-drag inertia. Pure math — no GPUI — so it is unit-testable.

pub const MIN_ZOOM: f32 = 0.15;
pub const MAX_ZOOM: f32 = 4.0;

/// Keep a camera centre finite. A NaN or infinite centre is not a cosmetic
/// glitch: `to_screen` returns NaN for every node so the canvas goes blank and
/// hit-testing stops working, and the autosave writes the pose out as JSON
/// `null`, which the loader rejects — the map opens read-only from then on.
fn finite_center(c: (f32, f32), fallback: (f32, f32)) -> (f32, f32) {
    (
        if c.0.is_finite() { c.0 } else { fallback.0 },
        if c.1.is_finite() { c.1 } else { fallback.1 },
    )
}

/// How fast the camera converges on its target (seconds to ~63% of the gap).
const EASE_TAU: f32 = 0.11;
/// Velocity decay per second while coasting (physical feel, not a hard stop).
const FRICTION: f32 = 0.0016;
/// Below this world-space speed the coast is over.
const REST_SPEED: f32 = 6.0;

/// Screen rectangle the camera draws into, in window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Viewport {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x,
            y,
            w: w.max(1.0),
            h: h.max(1.0),
        }
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// World point currently shown at the viewport center.
    center: (f32, f32),
    zoom: f32,
    target_center: (f32, f32),
    target_zoom: f32,
    /// World units per second, used for post-drag coasting.
    vel: (f32, f32),
    viewport: Viewport,
}

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

impl Camera {
    pub fn new() -> Self {
        Self {
            center: (0.0, 0.0),
            zoom: 1.0,
            target_center: (0.0, 0.0),
            target_zoom: 1.0,
            vel: (0.0, 0.0),
            viewport: Viewport::new(0.0, 0.0, 800.0, 600.0),
        }
    }

    pub fn set_viewport(&mut self, vp: Viewport) {
        self.viewport = vp;
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn center(&self) -> (f32, f32) {
        self.center
    }

    /// Restore a saved camera without animating into place.
    pub fn set_pose(&mut self, center: (f32, f32), zoom: f32) {
        let center = finite_center(center, self.center);
        let zoom = if zoom.is_finite() {
            zoom.clamp(MIN_ZOOM, MAX_ZOOM)
        } else {
            self.zoom
        };
        self.center = center;
        self.target_center = center;
        self.zoom = zoom;
        self.target_zoom = zoom;
        self.vel = (0.0, 0.0);
    }

    pub fn to_screen(&self, wx: f32, wy: f32) -> (f32, f32) {
        let (cx, cy) = self.viewport.center();
        (
            cx + (wx - self.center.0) * self.zoom,
            cy + (wy - self.center.1) * self.zoom,
        )
    }

    pub fn to_world(&self, sx: f32, sy: f32) -> (f32, f32) {
        let (cx, cy) = self.viewport.center();
        (
            self.center.0 + (sx - cx) / self.zoom,
            self.center.1 + (sy - cy) / self.zoom,
        )
    }

    /// World-space rectangle currently visible: (min_x, min_y, max_x, max_y).
    /// The culling primitive — every frame asks for this before deciding what
    /// to build.
    pub fn visible_world(&self) -> (f32, f32, f32, f32) {
        let (x0, y0) = self.to_world(self.viewport.x, self.viewport.y);
        let (x1, y1) = self.to_world(
            self.viewport.x + self.viewport.w,
            self.viewport.y + self.viewport.h,
        );
        (x0, y0, x1, y1)
    }

    /// Direct manipulation: drag/scroll the world by a screen-space delta.
    /// Cancels any in-flight fly-to so the hand always wins.
    pub fn pan_by_screen(&mut self, dx: f32, dy: f32) {
        self.center.0 -= dx / self.zoom;
        self.center.1 -= dy / self.zoom;
        self.target_center = self.center;
        self.target_zoom = self.zoom;
    }

    /// Give the camera a parting shove (world units/sec) to coast on.
    pub fn set_velocity(&mut self, vx: f32, vy: f32) {
        self.vel = (vx, vy);
    }

    pub fn stop(&mut self) {
        self.vel = (0.0, 0.0);
        self.target_center = self.center;
        self.target_zoom = self.zoom;
    }

    /// Zoom by `factor`, keeping the world point under `(sx, sy)` pinned.
    pub fn zoom_at(&mut self, sx: f32, sy: f32, factor: f32) {
        let before = self.to_world(sx, sy);
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        self.zoom = new_zoom;
        let after = self.to_world(sx, sy);
        self.center.0 += before.0 - after.0;
        self.center.1 += before.1 - after.1;
        self.target_center = self.center;
        self.target_zoom = self.zoom;
        self.vel = (0.0, 0.0);
    }

    /// Eased zoom about a screen point. Unlike `zoom_at`, which snaps, this
    /// nudges the *target* and pins the world point under `(sx, sy)` in the
    /// target frame — so a wheel notch or a pinch glides to its new scale
    /// instead of jumping, and the point under the cursor still stays put once
    /// the ease settles. Accumulates: several notches in a row compound before
    /// the camera has finished catching up.
    pub fn zoom_toward(&mut self, sx: f32, sy: f32, factor: f32) {
        let before = self.to_world_at_target(sx, sy);
        let new_zoom = (self.target_zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - self.target_zoom).abs() < f32::EPSILON {
            return;
        }
        self.target_zoom = new_zoom;
        let after = self.to_world_at_target(sx, sy);
        self.target_center.0 += before.0 - after.0;
        self.target_center.1 += before.1 - after.1;
        self.vel = (0.0, 0.0);
    }

    /// Animated zoom about the viewport center.
    pub fn zoom_by(&mut self, factor: f32) {
        let (cx, cy) = self.viewport.center();
        let before = self.to_world_at_target(cx, cy);
        self.target_zoom = (self.target_zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.to_world_at_target(cx, cy);
        self.target_center.0 += before.0 - after.0;
        self.target_center.1 += before.1 - after.1;
        self.vel = (0.0, 0.0);
    }

    fn to_world_at_target(&self, sx: f32, sy: f32) -> (f32, f32) {
        let (cx, cy) = self.viewport.center();
        (
            self.target_center.0 + (sx - cx) / self.target_zoom,
            self.target_center.1 + (sy - cy) / self.target_zoom,
        )
    }

    /// Ease toward a pose.
    pub fn fly_to(&mut self, center: (f32, f32), zoom: f32) {
        self.target_center = finite_center(center, self.center);
        self.target_zoom = if zoom.is_finite() {
            zoom.clamp(MIN_ZOOM, MAX_ZOOM)
        } else {
            self.target_zoom
        };
        self.vel = (0.0, 0.0);
    }

    /// Frame a world rectangle with padding, animated.
    pub fn fly_to_fit(&mut self, world: (f32, f32, f32, f32), pad: f32) {
        let (zoom, center) = self.fit_pose(world, pad);
        self.fly_to(center, zoom);
    }

    /// Frame a world rectangle immediately (startup, no animation).
    pub fn snap_to_fit(&mut self, world: (f32, f32, f32, f32), pad: f32) {
        let (zoom, center) = self.fit_pose(world, pad);
        self.set_pose(center, zoom);
    }

    fn fit_pose(&self, world: (f32, f32, f32, f32), pad: f32) -> (f32, (f32, f32)) {
        let (min_x, min_y, max_x, max_y) = world;
        let w = (max_x - min_x).max(1.0);
        let h = (max_y - min_y).max(1.0);
        let avail_w = (self.viewport.w - pad * 2.0).max(1.0);
        let avail_h = (self.viewport.h - pad * 2.0).max(1.0);
        // Fitting only ever zooms *out* to reveal the whole map — never past
        // 100%. Without the cap, a one-node or empty map (whose extent is tiny)
        // frames at MAX_ZOOM, blowing a single node up to fill the window.
        let zoom = (avail_w / w)
            .min(avail_h / h)
            .min(1.0)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        // Two coordinates near f32::MAX sum to infinity before the halving, so
        // take the midpoint as an offset from `min` instead. An infinite centre
        // makes every `to_screen` NaN and gets written to the save file as
        // `null`, which the loader then refuses — a blank window and a map that
        // never opens again.
        let mid = |lo: f32, hi: f32| lo + (hi - lo) * 0.5;
        (zoom, (mid(min_x, max_x), mid(min_y, max_y)))
    }

    /// Pan just enough to bring a world point inside the comfortable inner
    /// region (`inset` = fraction of each edge kept clear). Returns true if the
    /// camera had to move — navigation that stays on screen shouldn't lurch.
    pub fn ensure_visible(&mut self, wx: f32, wy: f32, inset: f32) -> bool {
        let inset = inset.clamp(0.0, 0.45);
        let half_w = self.viewport.w * (0.5 - inset) / self.target_zoom;
        let half_h = self.viewport.h * (0.5 - inset) / self.target_zoom;
        let mut target = self.target_center;
        let mut moved = false;
        if wx < target.0 - half_w {
            target.0 = wx + half_w;
            moved = true;
        } else if wx > target.0 + half_w {
            target.0 = wx - half_w;
            moved = true;
        }
        if wy < target.1 - half_h {
            target.1 = wy + half_h;
            moved = true;
        } else if wy > target.1 + half_h {
            target.1 = wy - half_h;
            moved = true;
        }
        if moved {
            self.target_center = target;
            self.vel = (0.0, 0.0);
        }
        moved
    }

    pub fn is_animating(&self) -> bool {
        let dx = self.target_center.0 - self.center.0;
        let dy = self.target_center.1 - self.center.1;
        let moving = (dx * dx + dy * dy).sqrt() > 0.05
            || (self.target_zoom / self.zoom).ln().abs() > 0.0005;
        moving || self.speed() > REST_SPEED
    }

    fn speed(&self) -> f32 {
        (self.vel.0 * self.vel.0 + self.vel.1 * self.vel.1).sqrt()
    }

    /// Advance the animation by `dt` seconds. Returns true if anything moved.
    pub fn step(&mut self, dt: f32) -> bool {
        if dt <= 0.0 {
            return false;
        }
        let mut moved = false;

        // Coasting after a flick — only while there is no fly-to pending.
        if self.speed() > REST_SPEED {
            self.center.0 += self.vel.0 * dt;
            self.center.1 += self.vel.1 * dt;
            let decay = FRICTION.powf(dt);
            self.vel.0 *= decay;
            self.vel.1 *= decay;
            self.target_center = self.center;
            moved = true;
            if self.speed() <= REST_SPEED {
                self.vel = (0.0, 0.0);
            }
            return moved;
        }
        self.vel = (0.0, 0.0);

        let t = 1.0 - (-dt / EASE_TAU).exp();
        let dx = self.target_center.0 - self.center.0;
        let dy = self.target_center.1 - self.center.1;
        if (dx * dx + dy * dy).sqrt() > 0.05 {
            self.center.0 += dx * t;
            self.center.1 += dy * t;
            moved = true;
        } else {
            self.center = self.target_center;
        }

        // Zoom interpolates geometrically so each step feels equally large.
        let ratio = (self.target_zoom / self.zoom).ln();
        if ratio.abs() > 0.0005 {
            self.zoom *= (ratio * t).exp();
            self.zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
            moved = true;
        } else {
            self.zoom = self.target_zoom;
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        let mut c = Camera::new();
        c.set_viewport(Viewport::new(0.0, 0.0, 800.0, 600.0));
        c
    }

    fn settle(c: &mut Camera) {
        for _ in 0..2000 {
            if !c.step(1.0 / 120.0) {
                break;
            }
        }
    }

    #[test]
    fn screen_world_roundtrip() {
        let mut c = cam();
        c.set_pose((37.0, -12.0), 1.7);
        let (sx, sy) = c.to_screen(120.0, 45.0);
        let (wx, wy) = c.to_world(sx, sy);
        assert!((wx - 120.0).abs() < 0.01, "wx = {wx}");
        assert!((wy - 45.0).abs() < 0.01, "wy = {wy}");
    }

    #[test]
    fn zoom_at_cursor_pins_world_point() {
        let mut c = cam();
        let (sx, sy) = (700.0, 100.0);
        let before = c.to_world(sx, sy);
        c.zoom_at(sx, sy, 1.8);
        let after = c.to_world(sx, sy);
        assert!((before.0 - after.0).abs() < 0.01);
        assert!((before.1 - after.1).abs() < 0.01);
        assert!(c.zoom() > 1.0);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut c = cam();
        for _ in 0..50 {
            c.zoom_at(400.0, 300.0, 2.0);
        }
        assert!((c.zoom() - MAX_ZOOM).abs() < 1e-3);
        for _ in 0..100 {
            c.zoom_at(400.0, 300.0, 0.5);
        }
        assert!((c.zoom() - MIN_ZOOM).abs() < 1e-3);
    }

    #[test]
    fn pan_moves_world_opposite_the_hand() {
        let mut c = cam();
        c.pan_by_screen(100.0, 0.0);
        // Dragging right pushes the camera left over the world.
        assert!(c.center().0 < 0.0);
        assert!(!c.is_animating(), "direct pan must not queue an animation");
    }

    #[test]
    fn fly_to_converges() {
        let mut c = cam();
        c.fly_to((500.0, -250.0), 2.0);
        assert!(c.is_animating());
        settle(&mut c);
        assert!(!c.is_animating());
        assert!((c.center().0 - 500.0).abs() < 0.2);
        assert!((c.center().1 + 250.0).abs() < 0.2);
        assert!((c.zoom() - 2.0).abs() < 0.01);
    }

    #[test]
    fn ensure_visible_only_moves_when_offscreen() {
        let mut c = cam();
        assert!(!c.ensure_visible(10.0, 10.0, 0.15));
        assert!(c.ensure_visible(5000.0, 0.0, 0.15));
        settle(&mut c);
        let (sx, _) = c.to_screen(5000.0, 0.0);
        assert!(sx > 0.0 && sx < 800.0, "point should be on screen: {sx}");
    }

    #[test]
    fn fit_frames_the_whole_map() {
        let mut c = cam();
        c.snap_to_fit((-1000.0, -500.0, 1000.0, 500.0), 40.0);
        let (x0, y0, x1, y1) = c.visible_world();
        assert!(x0 <= -1000.0 && x1 >= 1000.0);
        assert!(y0 <= -500.0 && y1 >= 500.0);
    }

    #[test]
    fn inertia_coasts_then_rests() {
        let mut c = cam();
        c.set_velocity(900.0, 0.0);
        assert!(c.is_animating());
        settle(&mut c);
        assert!(!c.is_animating());
        assert!(c.center().0 > 0.0, "should have drifted");
    }
}
