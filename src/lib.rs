//! `voxview` -- a standalone MagicaVoxel `.vox` viewer.
//!
//! The library half holds everything that is testable without a GPU: parsing,
//! scene-graph flattening, palettes and greedy meshing. The binary adds the
//! window, renderer and file watcher.

pub mod app;
pub mod camera;
pub mod fixtures;
pub mod font;
pub mod gfx;
pub mod hud;
pub mod loader;
pub mod menu;
pub mod mesh;
pub mod model;
pub mod overlay;
pub mod palette;
pub mod scene;
pub mod watch;
