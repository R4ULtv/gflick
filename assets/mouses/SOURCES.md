# Device artwork

This directory contains one transparent PNG per supported model and enclosure finish,
named `<model>-<finish>.png`. `apps/settings/build.rs` prefilters the masters at build
time; the application does not read these source files at runtime.

The images are official Logitech product artwork. GFlick is not affiliated with or
endorsed by Logitech. Keep any replacement artwork limited to material that the project
has permission to redistribute, and record its source URL and license or usage basis in
the table below when known.

| Files | Source | Notes |
| --- | --- | --- |
| `pro-x-superlight-*.png` | Logitech product artwork | White, black, red, and magenta finishes; original source URLs were not recorded |
| `pro-x-superlight-2-*.png` | Logitech product artwork | White, black, cyan, and magenta finishes; original source URLs were not recorded |
| `g305-lightspeed-*.png` | Logitech product artwork | White, black, blue, lilac, and mint finishes; original source URLs were not recorded |

## Build-time processing

The build script measures the alpha bounding box and crops transparent framing before
creating 1x and 2x BGRA textures. Each texture is scaled to the 330-point enclosure
height used by the Settings preview. Button hotspots in
`apps/settings/src/preview.rs` are normalized to that cropped enclosure, so reframing a
master does not move the controls.

Masters must be RGBA8 PNGs with a fully transparent background, no shadow or backdrop,
and a straight top-down pose. Finishes of one model must use the same pose because they
share one hotspot table. Aim for an enclosure at least 660 pixels tall so the 2x bake
does not upscale it.

Current master dimensions are:

| Model | Approximate dimensions | Finishes |
| --- | ---: | --- |
| PRO X Superlight | 421 x 834 px | 4 |
| PRO X Superlight 2 | 797 x 1,579–1,580 px | 4 |
| G305 Lightspeed | 882–883 x 1,585–1,586 px | 5 |

## Color swatches

The finish catalog and hand-picked UI swatches live in `DeviceModel::colors` and
`DeviceModel::swatch` in `crates/gflick-protocol/src/lib.rs`. They are presentation
values chosen for the dark interface, not colors sampled from these image files.
