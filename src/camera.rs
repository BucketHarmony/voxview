//! Orbit camera.
//!
//! Right-handed with Z up, matching MagicaVoxel and Veloren, so a model's
//! "up" in the editor is its up here.

use crate::scene::Bounds;
use glam::camera::rh::{proj::directx, view::look_at_mat4};
use glam::{Mat4, Vec3};

/// Default azimuth: looking at the model from front-right.
const DEFAULT_YAW: f32 = -70f32 * std::f32::consts::PI / 180.0;
/// Default elevation: slightly above the model.
const DEFAULT_PITCH: f32 = 25f32 * std::f32::consts::PI / 180.0;
/// Keeps the camera off the poles, where `up` becomes ambiguous.
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.01;
const ORBIT_SENSITIVITY: f32 = 0.008;
const ZOOM_PER_NOTCH: f32 = 1.12;

/// One of the six axis-aligned directions to look from.
///
/// Named for where the camera goes, not where it points: [`Axis::Front`] puts
/// the camera in front of the model looking back at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// Rotation about +Z, radians.
    pub yaw: f32,
    /// Elevation above the XY plane, radians.
    pub pitch: f32,
    pub fov_y: f32,
    /// Parallel projection instead of perspective.
    ///
    /// Perspective is the better picture; orthographic is the better
    /// measurement, because two voxels the same size are the same size on
    /// screen wherever they are. Comparing a model against a reference, or
    /// checking that a wall is flush, is the ordinary work of a voxel asset
    /// browser, so both are here.
    pub orthographic: bool,
    /// Longest dimension of what we are looking at; sets the depth range.
    scene_scale: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        OrbitCamera {
            target: Vec3::ZERO,
            distance: 64.0,
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            fov_y: 45f32.to_radians(),
            orthographic: false,
            scene_scale: 64.0,
        }
    }
}

impl OrbitCamera {
    /// The default camera, with the projection the settings asked for.
    pub fn with_projection(orthographic: bool) -> OrbitCamera {
        OrbitCamera {
            orthographic,
            ..OrbitCamera::default()
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.offset()
    }

    fn offset(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * cy, cp * sy, sp) * self.distance
    }

    /// Unit vector from the eye towards the target.
    pub fn forward(&self) -> Vec3 {
        -self.offset().normalize_or(Vec3::NEG_X)
    }

    /// Screen-right in world space.
    pub fn right(&self) -> Vec3 {
        self.forward().cross(self.up_hint()).normalize_or(Vec3::Y)
    }

    /// Screen-up in world space.
    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward())
    }

    /// The world direction that should end up pointing up the screen.
    ///
    /// +Z everywhere except straight up and straight down, where +Z is
    /// parallel to the view and says nothing about the roll. There the
    /// horizontal direction the camera came from stands in, which puts +Y up
    /// in the default top view -- the same orientation Blender's numpad 7
    /// gives, which is the muscle memory this borrows from.
    fn up_hint(&self) -> Vec3 {
        if self.pitch.cos().abs() > 1e-3 {
            return Vec3::Z;
        }
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cy, sy, 0.0) * -self.pitch.signum()
    }

    pub fn view(&self) -> Mat4 {
        look_at_mat4(self.eye(), self.target, self.up_hint())
    }

    /// `wgpu`'s clip space is Y-up with depth in `0..1` -- the Direct3D/Metal
    /// convention -- so that is the projection glam variant to use. The Vulkan
    /// one would be Y-down and flip the image.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        let span = self.scene_scale.max(self.distance);
        let aspect = aspect.max(0.01);
        let far = (span * 8.0).max(self.distance * 4.0);
        if self.orthographic {
            let half_h = self.view_height() * 0.5;
            let half_w = half_h * aspect;
            // A parallel projection has no eye to be behind, so the near plane
            // goes behind the camera instead of in front of it. Otherwise a
            // model larger than the orbit distance -- which is every model,
            // zoomed in -- has its front half sliced off.
            directx::orthographic(-half_w, half_w, -half_h, half_h, -far, far)
        } else {
            let near = (span * 0.001).clamp(0.01, 1.0);
            directx::perspective(self.fov_y, aspect, near, far)
        }
    }

    /// World height of the viewport at the orbit target.
    ///
    /// Shared by both projections, which is what makes the toggle between
    /// them leave the model the same size on screen, and what lets panning
    /// and zooming stay one piece of arithmetic.
    fn view_height(&self) -> f32 {
        2.0 * self.distance * (self.fov_y * 0.5).tan()
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection(aspect) * self.view()
    }

    /// Drag with the left mouse button.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        // Dragging right spins the model to the right, so the camera swings
        // the opposite way; dragging down tips the model's top towards us.
        self.yaw -= dx * ORBIT_SENSITIVITY;
        self.pitch = (self.pitch + dy * ORBIT_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        self.yaw = self.yaw.rem_euclid(std::f32::consts::TAU);
    }

    /// Drag with the right or middle mouse button. `viewport_height` keeps the
    /// grabbed point under the cursor regardless of zoom or window size.
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let world_per_pixel = self.view_height() / viewport_height.max(1.0);
        self.target += (-self.right() * dx + self.up() * dy) * world_per_pixel;
    }

    /// Mouse wheel; `notches` is positive when scrolling away from the user.
    pub fn zoom(&mut self, notches: f32) {
        let limit = self.scene_scale.max(1.0);
        self.distance =
            (self.distance * ZOOM_PER_NOTCH.powf(-notches)).clamp(limit * 0.005, limit * 200.0);
    }

    /// Point at `bounds` and back off far enough to see all of it, keeping the
    /// current orbit angles.
    pub fn frame(&mut self, bounds: &Bounds) {
        self.scene_scale = bounds.diagonal();
        self.target = bounds.centre();
        let radius = bounds.diagonal() * 0.5;
        // 1.25 leaves a margin so the model does not touch the window edge.
        self.distance = (radius / (self.fov_y * 0.5).sin()) * 1.25;
    }

    /// Frame `bounds` from the default angles.
    pub fn reset(&mut self, bounds: &Bounds) {
        self.yaw = DEFAULT_YAW;
        self.pitch = DEFAULT_PITCH;
        self.frame(bounds);
    }

    /// Swing round to look down one of the six axes.
    ///
    /// The distance and the target are left alone: this is a change of
    /// viewpoint, not a re-frame, so a detail you had zoomed in on is still
    /// the thing on screen afterwards.
    pub fn look_along(&mut self, axis: Axis) {
        let half = std::f32::consts::FRAC_PI_2;
        let (yaw, pitch) = match axis {
            // The camera sits on the negative Y side and looks towards +Y,
            // which is the face a MagicaVoxel model presents as its front.
            Axis::Front => (-half, 0.0),
            Axis::Back => (half, 0.0),
            Axis::Right => (0.0, 0.0),
            Axis::Left => (std::f32::consts::PI, 0.0),
            // Straight down and straight up. `up_hint` keeps the roll
            // defined at both poles, so these are exact rather than nearly.
            Axis::Top => (-half, half),
            Axis::Bottom => (-half, -half),
        };
        self.yaw = yaw.rem_euclid(std::f32::consts::TAU);
        self.pitch = pitch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn basis_is_right_handed_with_z_up() {
        let cam = OrbitCamera {
            yaw: 0.0,
            pitch: 0.0,
            ..Default::default()
        };
        // Eye on +X looking back along -X.
        assert!(approx(cam.forward(), Vec3::NEG_X));
        assert!(approx(cam.right(), Vec3::Y));
        assert!(approx(cam.up(), Vec3::Z));
    }

    #[test]
    fn framing_centres_and_backs_off() {
        let bounds = Bounds {
            min: Vec3::new(-4.0, -4.0, 0.0),
            max: Vec3::new(4.0, 4.0, 8.0),
        };
        let mut cam = OrbitCamera::default();
        cam.frame(&bounds);
        assert!(approx(cam.target, Vec3::new(0.0, 0.0, 4.0)));
        assert!(cam.distance > bounds.diagonal() * 0.5);
        // The whole bounding sphere must fall inside the vertical field of view.
        let half_angle = (bounds.diagonal() * 0.5 / cam.distance).asin();
        assert!(half_angle < cam.fov_y * 0.5);
    }

    #[test]
    fn pitch_never_reaches_the_pole() {
        let mut cam = OrbitCamera::default();
        for _ in 0..1000 {
            cam.orbit(0.0, 100.0);
        }
        assert!(cam.pitch < std::f32::consts::FRAC_PI_2);
        assert!(cam.up().length() > 0.5);
    }

    #[test]
    fn every_axis_view_looks_down_that_axis() {
        let cases = [
            (Axis::Front, Vec3::Y),
            (Axis::Back, Vec3::NEG_Y),
            (Axis::Right, Vec3::NEG_X),
            (Axis::Left, Vec3::X),
            (Axis::Top, Vec3::NEG_Z),
            (Axis::Bottom, Vec3::Z),
        ];
        let mut cam = OrbitCamera::default();
        for (axis, forward) in cases {
            cam.look_along(axis);
            assert!(
                approx(cam.forward(), forward),
                "{axis:?} should look along {forward}, got {}",
                cam.forward()
            );
            // And the frame stays a frame: no degenerate basis at the poles.
            assert!((cam.right().length() - 1.0).abs() < 1e-3, "{axis:?} right");
            assert!((cam.up().length() - 1.0).abs() < 1e-3, "{axis:?} up");
            assert!(cam.view().is_finite(), "{axis:?} view matrix");
        }
    }

    #[test]
    fn the_top_view_puts_y_up_the_screen() {
        // The orientation Blender's numpad 7 gives, which is what anyone
        // reaching for this key already expects.
        let mut cam = OrbitCamera::default();
        cam.look_along(Axis::Top);
        assert!(approx(cam.up(), Vec3::Y), "up was {}", cam.up());
        assert!(approx(cam.right(), Vec3::X), "right was {}", cam.right());
    }

    #[test]
    fn an_axis_view_keeps_the_zoom_and_the_target() {
        let mut cam = OrbitCamera {
            target: Vec3::new(3.0, -2.0, 1.0),
            distance: 17.5,
            ..Default::default()
        };
        cam.look_along(Axis::Left);
        assert!(approx(cam.target, Vec3::new(3.0, -2.0, 1.0)));
        assert_eq!(cam.distance, 17.5);
    }

    #[test]
    fn the_two_projections_agree_about_size_at_the_target() {
        // Toggling the projection should not make the model jump, so a point
        // in the target plane has to land in the same place in both.
        let mut cam = OrbitCamera::default();
        cam.frame(&Bounds {
            min: Vec3::splat(-8.0),
            max: Vec3::splat(8.0),
        });
        let on_screen = cam.target + cam.up() * 3.0 + cam.right() * 2.0;
        let project = |cam: &OrbitCamera| {
            let clip = cam.view_projection(16.0 / 9.0) * on_screen.extend(1.0);
            glam::Vec2::new(clip.x / clip.w, clip.y / clip.w)
        };
        let perspective = project(&cam);
        cam.orthographic = true;
        let orthographic = project(&cam);
        assert!(
            (perspective - orthographic).length() < 1e-3,
            "{perspective} and {orthographic} disagree"
        );
    }

    #[test]
    fn the_orthographic_near_plane_does_not_slice_the_model() {
        // Zoomed inside the bounding sphere, half the model is behind the
        // camera. A parallel projection has no eye to be behind, so it must
        // still be drawn.
        let mut cam = OrbitCamera::default();
        cam.frame(&Bounds {
            min: Vec3::splat(-32.0),
            max: Vec3::splat(32.0),
        });
        cam.orthographic = true;
        cam.distance = 1.0;
        let behind = cam.eye() - cam.forward() * 16.0;
        let clip = cam.view_projection(1.0) * behind.extend(1.0);
        let depth = clip.z / clip.w;
        assert!(
            (0.0..=1.0).contains(&depth),
            "a point behind the camera landed at depth {depth}"
        );
    }

    #[test]
    fn zoom_is_bounded() {
        let mut cam = OrbitCamera::default();
        for _ in 0..500 {
            cam.zoom(1.0);
        }
        assert!(cam.distance > 0.0);
        for _ in 0..1000 {
            cam.zoom(-1.0);
        }
        assert!(cam.distance.is_finite());
    }
}
