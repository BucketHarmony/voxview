# voxview

A standalone viewer for MagicaVoxel `.vox` files. It opens any file MagicaVoxel
writes, renders it with the file's own palette, and reloads when the file
changes on disk — which is the point: it is meant to sit on a second monitor
while you edit assets or while a pipeline writes them.

Single crate, single binary. No game engine.

## Build

```sh
cargo build --release
```

Stable Rust, 2024 edition. The binary lands in `target/release/voxview`
(`voxview.exe` on Windows).

Rendering goes through `wgpu` on Vulkan (Linux, both Wayland and X11) and DX12
or Vulkan (Windows). Set `WGPU_BACKEND=vulkan` or `WGPU_BACKEND=dx12` to force
one, and `WGPU_POWER_PREF=low` to pick the integrated GPU on a laptop.

## Usage

```sh
voxview model.vox            # open one file
voxview assets/voxel/npc/    # open the first .vox in a directory, page with [ and ]
voxview model.vox --stats    # print a summary and exit, no window
voxview --write-fixtures dir # write the synthetic test fixtures and exit
```

Naming a single file still lists its siblings, so `[` and `]` page through the
rest of the directory from wherever you started, and `M` opens a menu over them.
Directories of a few hundred files -- Veloren ships several -- are best browsed
from the menu, where you can type part of a name to narrow the list.

The open file is watched. Save from MagicaVoxel and the model re-parses and
re-meshes in place, without moving the camera. If the new bytes do not parse,
the last good render stays on screen and the error appears in the HUD and on
stderr.

## Keys

| Key | Action |
| --- | --- |
| Left-drag | Orbit |
| Right-drag, middle-drag | Pan |
| Scroll | Zoom |
| `F` | Frame the model, keeping the current angle |
| `Home` | Reset the camera to the default angle and framing |
| `G` | Ground grid at z = 0 |
| `B` | Bounding box |
| `A` | Axis gizmo (X red, Y green, Z blue) |
| `O` | Ambient occlusion |
| `T` | Toggle the dark and light background |
| `P` | Save a PNG next to the model |
| `[`, `]` | Previous, next `.vox` in the directory |
| `M`, `Tab` | Open and close the file menu |
| `R` | Reload now |
| `Esc`, `Q` | Quit |

### In the file menu

| Key | Action |
| --- | --- |
| Up, Down | Move the cursor, loading each file as you pass it |
| PgUp, PgDn | Jump a screenful |
| Click a row | Load that file |
| Scroll | Scroll the list, without loading anything |
| any letter | Add it to the filter |
| Backspace | Remove the last filter character |
| Enter | Load the highlighted file and close |
| `Esc` | Clear the filter, or close if there is none |
| `Tab` | Close, leaving the current file on screen |

The menu lists the directory, marks the file on screen with `>` and the cursor
with a highlight bar, and shows how many entries are above and below the view.
Arrowing through it previews each file; typing only moves the cursor, so you
can narrow a long list down before committing to a load.

While the menu is open the single-letter shortcuts above belong to the filter,
so `G` types a `g` rather than toggling the grid, and `M` will not close the
menu it opened -- `Esc` or `Tab` do that. `Home`, `F` and the mouse still work
on the camera, and `Esc` closes the menu rather than quitting the viewer.

`P` writes `<name>_<YYYYMMDD-HHMMSS>.png` beside the model, at window
resolution, with a transparent background and without the HUD or the overlays —
so it drops straight into a contact sheet or a wiki page.

Coordinates are right-handed with **Z up**, matching MagicaVoxel and Veloren:
a model's up in the editor is its up here.

## The two design decisions

### Face colours: palette indices, not vertex colours

Each vertex carries one packed `u32` — palette index, face direction and an
ambient-occlusion term — and the fragment shader resolves the colour against a
256-entry uniform. The alternative, baking RGB into the vertices, would have
been marginally simpler.

Indices win for two reasons. A palette edit, which is a common thing to do to a
finished model, then costs a single 4 KiB buffer write instead of re-running the
mesher; that matters because the hot-reload path re-parses on every save. And it
keeps the vertex at 16 bytes — position plus one word — where RGB plus a normal
plus an AO byte would have pushed it past 24, which is real bandwidth on the
integrated GPUs this is expected to run on. Nothing else in the format wants
per-vertex colour: `.vox` is indexed by construction, so following that costs
nothing and keeps the door open for the palette-remap tool the code already
marks an extension point for.

### Meshing: greedy, with occlusion in the merge key

Each model is greedily meshed into one vertex and one index buffer and drawn in
a single call, with its scene-graph transform supplied by a dynamic-offset
uniform. The mesher sweeps each axis slice by slice, builds a mask of exposed
faces, and merges each maximal rectangle of identical faces into one quad — the
standard approach, and on real assets it collapses the large flat regions that
dominate them.

The wrinkle is that the merge key includes the per-face ambient occlusion, not
just the palette index. Occlusion is one value per face, so merging two faces
that differ in it would silently apply one face's shading to the whole
rectangle, flattening exactly the creases and inside corners the term exists to
show. This costs some merging on busy surfaces and costs nothing at all on the
shapes the tests pin down: an isolated cube has uniform occlusion on every side,
so a 1×1×1 and a solid 2×2×2 both still collapse to six quads and twelve
triangles.

Occlusion comes from the eight cells around the face in the plane in front — the
usual two-sides-and-a-diagonal term at each of the four corners, summed — and is
quantised into the packed vertex word. `O` switches the shader's occlusion
factor off rather than re-meshing, so toggling it costs nothing.

## Testing

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The three fixtures under `tests/fixtures/` — a solid cube, an 8³ checkerboard
and a two-model scene with translations — are written from code by
`voxview::fixtures` and rewritten by the test run, so nothing here needs
MagicaVoxel installed. `cargo test --release` additionally asserts the meshing
budget: a 126³ model in under 200 ms.

Malformed input is covered by tests that truncate and corrupt a fixture at
thousands of offsets. Bad files produce an error in the HUD and on stderr; they
never crash the viewer.

## Not in v1

No editing, painting, export to other formats, materials or emissives, and no
animation. `MATL` chunks are parsed and counted in the HUD but do not affect
shading; `rOBJ` and `rCAM` are parsed and ignored.

Two extension points are marked in the source with `// EXTENSION:`: loading
Veloren RON manifests, which describe multi-part assemblies with per-part
offsets (`src/loader.rs`), and a palette remap tool (`src/palette.rs`).

## Licence

MIT OR Apache-2.0.
