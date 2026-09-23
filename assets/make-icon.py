"""Redraw voxview's icon: one isometric voxel, subdivided 2x2 per face.

    python assets/make-icon.py

Writes `voxview.png` (256x256, for the window and for Linux desktop files)
and `voxview.ico` (seven sizes, for the Windows executable) beside itself.
Needs Pillow, which nothing else here does -- the icon is checked in, so this
only has to run when the icon changes.

Everything is drawn at 8x and downsampled, because the only antialiasing that
looks right at 16 pixels is a lot of supersampling.
"""

import os

from PIL import Image, ImageDraw

SS = 8
N = 256 * SS

# Lit from above and to the left, the same way the viewer lights a model.
TOP = (0x7A, 0xD1, 0xF0, 255)
LEFT = (0x3C, 0x8C, 0xB8, 255)
RIGHT = (0x28, 0x62, 0x8C, 255)
EDGE = (0x12, 0x33, 0x4A, 255)

SIZES = [256, 128, 64, 48, 32, 24, 16]
# Below this the subdivision lines are thinner than a pixel and turn the icon
# to mud, so the small sizes are drawn from a plain cube instead.
DETAIL_FLOOR = 32


def cube(draw, cx, cy, w, d, subdivide=True):
    """An isometric cube centred on `(cx, cy)`, `2w` wide with vertical side `d`."""
    h = w / 2.0
    top = [(cx, cy - d / 2 - h), (cx + w, cy - d / 2),
           (cx, cy - d / 2 + h), (cx - w, cy - d / 2)]
    left = [(cx - w, cy - d / 2), (cx, cy - d / 2 + h),
            (cx, cy + d / 2 + h), (cx - w, cy + d / 2)]
    right = [(cx + w, cy - d / 2), (cx, cy - d / 2 + h),
             (cx, cy + d / 2 + h), (cx + w, cy + d / 2)]
    for face, fill in ((top, TOP), (left, LEFT), (right, RIGHT)):
        draw.polygon(face, fill=fill)

    line = max(2, int(N * 0.012))
    for face in (top, left, right) if subdivide else ():
        mid = [((face[i][0] + face[(i + 1) % 4][0]) / 2,
                (face[i][1] + face[(i + 1) % 4][1]) / 2) for i in range(4)]
        draw.line([mid[0], mid[2]], fill=EDGE, width=line, joint='curve')
        draw.line([mid[1], mid[3]], fill=EDGE, width=line, joint='curve')

    outline = max(3, int(N * 0.022))
    silhouette = [top[0], top[1], right[3], right[2], left[3], top[3]]
    draw.line(silhouette + [silhouette[0]], fill=EDGE,
              width=outline, joint='curve')
    # The three edges meeting at the near corner, which are what makes it read
    # as a cube rather than a hexagon.
    for corner in (top[1], top[3], left[2]):
        draw.line([top[2], corner], fill=EDGE, width=outline, joint='curve')


def draw_at(subdivide):
    img = Image.new('RGBA', (N, N), (0, 0, 0, 0))
    w = N * 0.40
    cube(ImageDraw.Draw(img), N / 2, N / 2 + N * 0.02, w, w, subdivide)
    return img


def main():
    detailed, plain = draw_at(True), draw_at(False)
    frames = [(detailed if s >= DETAIL_FLOOR else plain).resize((s, s), Image.LANCZOS)
              for s in SIZES]

    out = os.path.dirname(os.path.abspath(__file__))
    frames[0].save(os.path.join(out, 'voxview.png'))
    # append_images is what makes the ICO take the frames as drawn; without it
    # Pillow resizes the base image itself and the plain small sizes are lost.
    frames[0].save(os.path.join(out, 'voxview.ico'), format='ICO',
                   sizes=[(s, s) for s in SIZES], append_images=frames[1:])
    print('wrote voxview.png and voxview.ico to', out)


main()
