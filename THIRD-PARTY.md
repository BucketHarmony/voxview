# Third-party licences

voxview itself is MIT OR Apache-2.0 (see `LICENSE-MIT` and `LICENSE-APACHE`).
It links a few hundred crates, and a release build embeds some of their data in
the binary, so redistributing that binary carries their terms as well. This file
records the ones that ask for anything beyond the usual permissive notice.

## Fonts embedded by egui

`egui`'s `default_fonts` feature is on, so `epaint_default_fonts` bakes four
typefaces into the binary. Its SPDX expression is
`(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` — the `AND` is the part
that matters: those font licences apply on top of the crate's own, and cannot
be chosen away.

| Font | Licence | Text |
| --- | --- | --- |
| Ubuntu-Light | Ubuntu Font Licence 1.0 | `fonts/UFL.txt` in `epaint_default_fonts` |
| NotoEmoji-Regular | SIL Open Font Licence 1.1 | `fonts/OFL.txt` |
| Hack-Regular | MIT (Hack variant) | `fonts/Hack-Regular.txt` |
| emoji-icon-font | MIT | `fonts/emoji-icon-font-mit-license.txt` |

Both OFL-1.1 and the Ubuntu Font Licence permit embedding and redistribution
inside a program. What they do not permit is selling the fonts on their own, and
OFL-1.1 forbids reusing the reserved font name for a modified version. Ship the
licence texts alongside any binary you distribute.

If that is unwelcome, turning off `default_fonts` in `Cargo.toml` and calling
`Context::set_fonts` with a font of your own removes all four, and with them
this whole section. voxview's own HUD font is not affected: it is a 5×7 bitmap
atlas written by hand in `src/font.rs`.

## Dual and multi-licensed dependencies

Two crates in the tree offer a copyleft option alongside a permissive one.
voxview takes the permissive side of each, as the expressions allow:

| Crate | Expression | Taken as |
| --- | --- | --- |
| `self_cell` | `Apache-2.0 OR GPL-2.0-only` | Apache-2.0 |
| `r-efi` | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | MIT |

`notify` is CC0-1.0, a public-domain dedication, which asks for nothing.

Everything else in the tree is MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause,
ISC, Zlib, 0BSD, Unlicense, Unicode-3.0, or a choice among them. No GPL, LGPL
or MPL code is linked.

To regenerate this picture after a dependency change:

```sh
cargo metadata --format-version 1 --all-features |
  python -c "import json,sys,collections; \
    m=json.load(sys.stdin); \
    c=collections.Counter(p.get('license') or '?' for p in m['packages']); \
    [print(v,k) for k,v in c.most_common()]"
```

## The MagicaVoxel default palette

`src/palette.rs` embeds MagicaVoxel's 256-entry default palette, used when a
file carries no `RGBA` chunk. It is a table of colour values — the numbers a
`.vox` file means when it says index 37 — rather than creative work, and a
viewer that did not have it could not open those files correctly. It is included
on that basis.
