# voxview

A standalone viewer and browser for MagicaVoxel `.vox` files. It opens any file
MagicaVoxel writes, renders it with the file's own palette, and reloads when the
file changes on disk — which is the point: it is meant to sit on a second
monitor while you edit assets or while a pipeline writes them. Point it at a
directory instead and it walks the whole tree, so a few thousand assets are one
window rather than a few thousand file-open dialogs.

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
voxview model.vox            # open one file in the viewer
voxview assets/voxygen/      # browse the tree in the library
voxview model.vox --stats    # print a summary and exit, no window
voxview --write-fixtures dir # write the synthetic test fixtures and exit
```

Naming a file means "show me this", so it opens the viewer. Naming a directory
means "show me what is here", so it opens the library: a thumbnail grid over
every `.vox` under that directory, however deep. `Enter` or a double-click move
from one to the other, and `Esc` goes back.

The file on screen in the viewer is watched. Save from MagicaVoxel and the model
re-parses and re-meshes in place, without moving the camera. If the new bytes do
not parse, the last good render stays on screen and the error appears in the HUD
and on stderr.

## The library

The scan runs on a background thread, so the window is up and usable while it
counts; the title bar says how many files it has found and the status bar says
when it is done. Thumbnails are rendered on the GPU, only for the cells you can
actually see, and cached between runs under `%LOCALAPPDATA%\voxview\thumbnails`
(Windows) or `~/.cache/voxview/thumbnails` (Linux, or `$XDG_CACHE_HOME`). The
cache key includes the file's size and modification time, so an edited model
re-renders on its own.

Down the left is the folder tree with a count beside each folder, then
collections, then the filter facets — extent, palette source, and state
(changed in the last 24 hours, failed to parse). Filters compose; the counts
beside them are live and show what would still match. Across the top is the
breadcrumb, a find box that matches on the path, the grid/list switch and the
cell-size slider. Down the right is the inspector for whatever is selected: its
preview, dimensions, voxel and triangle counts, model count, palette swatches
and file size, or a summary when several assets are selected.

Veloren names whole families of sprites `0.vox` .. `6.vox` inside one folder, so
two things follow from that. Cell captions carry the folder when it is not the
one being browsed — `carrot/0`, not `0`. And **stack variants** (on by default)
folds `name-1.vox`, `name-2.vox` and friends into one cell with a `×n` badge;
click the badge to open the stack, or press `S` to stop folding.

Collections cut across folders: select some assets, press `C`, name the
collection. They live for the run only — v1 does not write them to disk.

## Keys

### In the library

| Key | Action |
| --- | --- |
| Arrows | Move the cursor; `Home`, `End` jump to the ends |
| `[`, `]` | Move the cursor one cell |
| Click | Select; `Ctrl`-click adds, `Shift`-click extends |
| Double-click, `Enter` | Open the asset in the viewer |
| Space | Peek: a large preview over the grid, without leaving it |
| `/` | Jump to the find box |
| `V` | Switch between the grid and the list |
| `S` | Fold or unfold variant stacks |
| `C` | Put the selection in a collection |
| `Ctrl-A` | Select everything that matches the filters |
| `P` | Save a 512×512 PNG beside each selected file |
| `Esc` | Close the overlay, else clear the filters, else quit |
| `Q` | Quit |

### In the viewer

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
| `[`, `]` | Previous, next asset |
| `M`, `Tab` | Open and close the text file menu |
| `R` | Reload now |
| `Esc` | Back to the library |
| `Q` | Quit |

`[` and `]` follow the library's filtered, sorted order rather than a raw
directory listing, so filtering to "everything over 32 voxels that changed
today" and then paging through it works the way you would expect. The filmstrip
along the bottom of the viewer shows where you are in that order.

`M` opens the older text-mode file menu over the current directory. It predates
the library and is kept because it is quick: type part of a name to narrow it,
arrow through it to preview each file, `Enter` to commit, `Esc` or `Tab` to
close. While it is open the single-letter shortcuts belong to the filter, so `G`
types a `g` rather than toggling the grid.

`P` in the viewer writes `<name>_<YYYYMMDD-HHMMSS>.png` beside the model, at
window resolution, with a transparent background and without the HUD or the
overlays — so it drops straight into a contact sheet or a wiki page. `P` in the
library does the same for every selected asset at 512×512, which is how you get
a sheet of a hundred sprites without opening any of them.

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
never crash the viewer. In the library a file that will not parse gets a warning
marker on its cell and is counted in the status bar rather than stopping the
scan.

## Not in v1

No editing, painting, export to other formats, materials or emissives, and no
animation. `MATL` chunks are parsed and counted in the HUD but do not affect
shading; `rOBJ` and `rCAM` are parsed and ignored. Collections are not saved
between runs.

Two extension points are marked in the source with `// EXTENSION:`: loading
Veloren RON manifests, which describe multi-part assemblies with per-part
offsets (`src/loader.rs`), and a palette remap tool (`src/palette.rs`).

## Licence

MIT OR Apache-2.0.
