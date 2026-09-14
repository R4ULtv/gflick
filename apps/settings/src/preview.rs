//! Device photo, prefiltered by `build.rs` for the display's pixel density.
use std::collections::HashMap;
use std::sync::Arc;

use gflick_protocol::DeviceColor;
use gpui_kit::{
    Context, IntoElement, ObjectFit, Pixels, Render, RenderImage, Window, div, img, prelude::*, px,
};

/// An enclosure-only photo baked at ascending display densities.
struct Photo {
    densities: &'static [Baked],
}

/// One density of a photo: straight-alpha BGRA, `width` by `height` pixels.
struct Baked {
    scale_factor: f32,
    width: u32,
    height: u32,
    bgra: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/photos.rs"));

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MouseModel {
    Superlight,
    Superlight2,
    G305,
}

/// A control center normalized to model-specific enclosure bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonHotspot {
    x: f32,
    y: f32,
    /// The side of the artwork this control's callout is written on. Controls
    /// are called out on the side of the mouse the finger reaches them from.
    pub side: Side,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// One entry of a hotspot table.
const fn hotspot(x: f32, y: f32, side: Side) -> ButtonHotspot {
    ButtonHotspot { x, y, side }
}

const SUPERLIGHT_2_BUTTONS: [ButtonHotspot; 5] = [
    hotspot(0.247, 0.198, Side::Left),  // left click
    hotspot(0.749, 0.198, Side::Right), // right click
    hotspot(0.498, 0.184, Side::Right), // wheel click
    hotspot(0.050, 0.396, Side::Left),  // back
    hotspot(0.050, 0.532, Side::Left),  // forward
];

// Both Superlight generations share this shell and photographed alignment.
const SUPERLIGHT_BUTTONS: [ButtonHotspot; 5] = SUPERLIGHT_2_BUTTONS;

const G305_BUTTONS: [ButtonHotspot; 6] = [
    hotspot(0.255, 0.217, Side::Left),  // left click
    hotspot(0.752, 0.217, Side::Right), // right click
    hotspot(0.499, 0.184, Side::Right), // wheel click
    hotspot(0.051, 0.407, Side::Left),  // back
    hotspot(0.051, 0.537, Side::Left),  // forward
    hotspot(0.499, 0.357, Side::Right), // DPI button
];

/// Shared callout artwork dimensions; `build.rs` bakes against `ART_HEIGHT`.
pub const ART_WIDTH: Pixels = px(288.0);
pub const ART_HEIGHT: Pixels = px(330.0);
/// The enclosure's height on a card banner, which is a glance rather than a map.
const CARD_HEIGHT: Pixels = px(256.0);

/// One control's callout anchor: where its line lands on the artwork column,
/// in points from the column's top-left corner.
pub struct ButtonAnchor {
    pub side: Side,
    pub x: Pixels,
    pub y: Pixels,
}

impl MouseModel {
    pub fn for_device(device: &gflick_protocol::DeviceSummary) -> Option<Self> {
        device.model().map(|model| match model {
            gflick_protocol::DeviceModel::Superlight => Self::Superlight,
            gflick_protocol::DeviceModel::Superlight2 => Self::Superlight2,
            gflick_protocol::DeviceModel::G305 => Self::G305,
        })
    }
    fn photo(self, color: DeviceColor) -> &'static Photo {
        match self {
            Self::Superlight => match color {
                DeviceColor::White => &SUPERLIGHT_WHITE,
                DeviceColor::Red => &SUPERLIGHT_RED,
                DeviceColor::Magenta => &SUPERLIGHT_MAGENTA,
                _ => &SUPERLIGHT,
            },
            Self::Superlight2 => match color {
                DeviceColor::White => &SUPERLIGHT_2_WHITE,
                DeviceColor::Magenta => &SUPERLIGHT_2_MAGENTA,
                DeviceColor::Cyan => &SUPERLIGHT_2_CYAN,
                _ => &SUPERLIGHT_2,
            },
            Self::G305 => match color {
                DeviceColor::White => &G305_WHITE,
                DeviceColor::Blue => &G305_BLUE,
                DeviceColor::Lilac => &G305_LILAC,
                DeviceColor::Mint => &G305_MINT,
                _ => &G305,
            },
        }
    }
    /// The bake to paint on a display of this density: the first that is at
    /// least as dense, so the GPU shrinks rather than blurs when it has to.
    fn baked(self, color: DeviceColor, scale_factor: f32) -> &'static Baked {
        let densities = self.photo(color).densities;
        densities
            .iter()
            .find(|baked| baked.scale_factor >= scale_factor)
            .unwrap_or(&densities[densities.len() - 1])
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Superlight => "PRO X SUPERLIGHT",
            Self::Superlight2 => "PRO X SUPERLIGHT 2",
            Self::G305 => "G305 LIGHTSPEED",
        }
    }

    fn button_hotspots(self) -> &'static [ButtonHotspot] {
        match self {
            Self::Superlight => &SUPERLIGHT_BUTTONS,
            Self::Superlight2 => &SUPERLIGHT_2_BUTTONS,
            Self::G305 => &G305_BUTTONS,
        }
    }

    /// Maps normalized control hotspots into the artwork column.
    pub fn button_anchors(self) -> Vec<ButtonAnchor> {
        let photo = self.photo_rect();
        self.button_hotspots()
            .iter()
            .map(|hotspot| ButtonAnchor {
                side: hotspot.side,
                x: photo.0 + photo.1 * hotspot.x,
                y: ART_HEIGHT * hotspot.y,
            })
            .collect()
    }

    /// The enclosure inside the artwork column: its left edge and width.
    /// Every finish of a model is shot the same, so one of them is enough.
    fn photo_rect(self) -> (Pixels, Pixels) {
        let width = ART_HEIGHT * self.aspect(DeviceColor::Black);
        ((ART_WIDTH - width) / 2.0, width)
    }

    /// The enclosure's width over its height, from the bake it was cropped to.
    fn aspect(self, color: DeviceColor) -> f32 {
        let baked = &self.photo(color).densities[0];
        baked.width as f32 / baked.height as f32
    }
}

/// Which frame the artwork is painted in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Art {
    /// A wide card banner, at the photo's own logical height.
    Card,
    /// The buttons page's tall column, filled by the enclosure.
    Callouts,
}

pub struct MousePreview {
    model: MouseModel,
    color: DeviceColor,
    art: Art,
    /// Textures built from the baked pixels, keyed by model and bake height, so
    /// switching back to a device does not copy them again.
    textures: HashMap<(MouseModel, DeviceColor, u32), Arc<RenderImage>>,
}

impl MousePreview {
    pub fn new(model: MouseModel) -> Self {
        Self {
            model,
            color: DeviceColor::Black,
            art: Art::Card,
            textures: HashMap::new(),
        }
    }

    pub fn set_view(
        &mut self,
        model: MouseModel,
        color: DeviceColor,
        art: Art,
        cx: &mut Context<Self>,
    ) {
        if self.model != model || self.color != color || self.art != art {
            self.color = color;
            self.model = model;
            self.art = art;
            cx.notify();
        }
    }

    fn texture(&mut self, scale_factor: f32) -> Arc<RenderImage> {
        let baked = self.model.baked(self.color, scale_factor);
        self.textures
            .entry((self.model, self.color, baked.height))
            .or_insert_with(|| {
                let pixels =
                    image::RgbaImage::from_raw(baked.width, baked.height, baked.bgra.to_vec())
                        .expect("baked photo covers its own dimensions");
                Arc::new(RenderImage::new([image::Frame::new(pixels)]))
            })
            .clone()
    }
}

impl Render for MousePreview {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let aspect = self.model.aspect(self.color);
        let texture = self.texture(window.scale_factor());
        if self.art == Art::Callouts {
            let (left, width) = self.model.photo_rect();
            return div()
                .relative()
                .w(ART_WIDTH)
                .h(ART_HEIGHT)
                .flex_shrink_0()
                .overflow_hidden()
                .child(
                    img(texture)
                        .absolute()
                        .left(left)
                        .top(px(0.0))
                        .w(width)
                        .h(ART_HEIGHT)
                        .flex_shrink_0()
                        .object_fit(ObjectFit::Contain),
                );
        }
        // Callers own captions so pages do not repeat an existing device heading.
        div()
            .w_full()
            .h(px(280.0))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .child(
                img(texture)
                    .w(CARD_HEIGHT * aspect)
                    .h(CARD_HEIGHT)
                    .flex_shrink_0()
                    .object_fit(ObjectFit::Contain),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The buttons page paints the enclosure `ART_HEIGHT` tall on a display of
    /// any density up to the last bake's. Anything smaller is painted upscaled.
    #[test]
    fn every_bake_covers_the_tallest_artwork_the_pages_paint() {
        for model in [
            MouseModel::Superlight,
            MouseModel::Superlight2,
            MouseModel::G305,
        ] {
            for color in [DeviceColor::Black, DeviceColor::White] {
                for bake in model.photo(color).densities {
                    let wanted = ART_HEIGHT.as_f32() * bake.scale_factor;
                    assert!(
                        bake.height as f32 >= wanted - 1.0,
                        "{model:?} {color:?}: {} baked pixels for {wanted}",
                        bake.height
                    );
                }
            }
        }
    }

    #[test]
    fn every_advertised_finish_has_distinct_baked_artwork() {
        for (device, model) in [
            (
                gflick_protocol::DeviceModel::Superlight,
                MouseModel::Superlight,
            ),
            (
                gflick_protocol::DeviceModel::Superlight2,
                MouseModel::Superlight2,
            ),
            (gflick_protocol::DeviceModel::G305, MouseModel::G305),
        ] {
            let mut photos = Vec::new();
            for color in device.colors() {
                let photo = model.photo(*color);
                for bake in photo.densities {
                    assert_eq!(bake.bgra.len(), (bake.width * bake.height * 4) as usize);
                }
                assert!(!photos.contains(&photo.densities[0].bgra));
                photos.push(photo.densities[0].bgra);
            }
        }
    }

    #[test]
    fn every_supported_model_maps_its_reported_buttons() {
        assert_eq!(MouseModel::Superlight.button_hotspots().len(), 5);
        assert_eq!(MouseModel::Superlight2.button_hotspots().len(), 5);
        assert_eq!(MouseModel::G305.button_hotspots().len(), 6);
        // Every anchor has to land inside the column the page draws lines in.
        for model in [
            MouseModel::Superlight,
            MouseModel::Superlight2,
            MouseModel::G305,
        ] {
            assert_eq!(model.button_anchors().len(), model.button_hotspots().len());
            for anchor in model.button_anchors() {
                assert!(anchor.x > px(0.0) && anchor.x < ART_WIDTH, "{model:?}");
                assert!(anchor.y > px(0.0) && anchor.y < ART_HEIGHT, "{model:?}");
            }
        }
        for hotspot in [
            MouseModel::Superlight.button_hotspots(),
            MouseModel::Superlight2.button_hotspots(),
            MouseModel::G305.button_hotspots(),
        ]
        .into_iter()
        .flatten()
        {
            assert!((0.0..=1.0).contains(&hotspot.x));
            assert!((0.0..=1.0).contains(&hotspot.y));
        }
    }
}
