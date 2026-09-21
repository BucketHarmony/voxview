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

#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// Rotation about +Z, radians.
    pub yaw: f32,
    /// Elevation above the XY plane, radians.
    pub pitch: f32,
    pub fov_y: f32,
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
            scene_scale: 64.0,
        }
    }
}

impl OrbitCamera {
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
        self.forward().cross(Vec3::Z).normalize_or(Vec3::Y)
    }

    /// Screen-up in world space.
    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward())
    }

    pub fn view(&self) -> Mat4 {
        look_at_mat4(self.eye(), self.target, Vec3::Z)
    }

    /// `wgpu`'s clip space is Y-up with depth in `0..1` -- the Direct3D/Metal
    /// convention -- so that is the projection glam variant to use. The Vulkan
    /// one would be Y-down and flip the image.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        let span = self.scene_scale.max(self.distance);
        let near = (span * 0.001).clamp(0.01, 1.0);
        let far = (span * 8.0).max(self.distance * 4.0);
        directx::perspective(self.fov_y, aspect.max(0.01), near, far)
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
        let world_per_pixel =
            2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_height.max(1.0);
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
