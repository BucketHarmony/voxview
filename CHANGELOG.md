# Changelog

Notable changes, newest first. Versions follow [semantic
versioning](https://semver.org): the library API is what the major number
promises, and a change to the settings file or the thumbnail cache format
counts as one.

## 1.0.0

The first release. Everything below is new, so it is grouped by what it does
rather than by what changed.

### The viewer

* Opens any `.vox` file MagicaVoxel writes: the full chunk set, the
  `nTRN`/`nGRP`/`nSHP` scene graph, layers, and the file's own palette.
* Greedy meshing, one draw call per model, with per-face ambient occlusion in
  the merge key so creases survive being merged.
* Orbit camera, right-handed and Z up to match MagicaVoxel. Axis views on the
  Blender number keys, `Ctrl` for the opposite side, `5` for orthographic.
* Ground grid, bounding box, axis gizmo, a dark/light background, and a HUD
  with the model's dimensions, counts and timings.
* Multisampling from off to 8×, as far as the adapter will go.
* Hot reload: the file on screen is watched, and a save re-parses and re-meshes
  in place without moving the camera. A file that will not parse leaves the
  last good render up and puts the error in the HUD and on stderr.
* Screenshots to PNG at window resolution, transparent background, no chrome.

### Materials

* `MATL` chunks reach the shader as a 256-entry table beside the palette.
  `_emit` glows at `_emit × (_flux + 1)`, `_metal` takes a per-fragment
  highlight whose tightness comes from `_rough`, and `_glass` and `_media` are
  drawn in a second blended pass.
* The blended pass is not sorted against itself; see the README for what that
  does and does not cost.

### The library

* Point voxview at a directory and it walks the tree on a background thread,
  with a thumbnail grid that renders only the cells on screen.
* Thumbnails are cached on disk, keyed by size and modification time, and the
  cache is capped at 256 MB and trimmed least-recently-used.
* Folder tree, collections, and filters on extent, palette source and state,
  with live counts. A find box over the path, grid and list views, and an
  inspector for the selection.
* Variant stacks fold `name-1.vox`, `name-2.vox` into one cell; captions carry
  the folder, because a few hundred sprite families all called `0.vox` are not
  otherwise distinguishable.
* `P` saves a 512×512 PNG for every selected asset.
* File names in non-Latin scripts draw properly, using a font borrowed from the
  system and loaded only when a name needs it.

### Everything else

* Window geometry, view settings, library state and collections persist between
  runs, in a line-based text file meant to be edited by hand.
* Malformed input is an error in the HUD and on stderr, never a crash — pinned
  by tests that truncate and corrupt a fixture at thousands of offsets.
* CI on Linux, Windows and macOS, including a headless render against Mesa's
  software Vulkan driver.
* Dual licensed MIT or Apache-2.0.
