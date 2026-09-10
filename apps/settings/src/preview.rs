//! Device photo, prefiltered by `build.rs` for the display's pixel density.
use std::collections::HashMap;
use std::sync::Arc;

use gflick_protocol::DeviceColor;
use gpui_kit::{
    Context, IntoElement, ObjectFit, Render, RenderImage, Window, div, img, prelude::*, px,
};

/// A device photo, painted `logical_height` points tall, baked by `build.rs`
/// at each pixel density in `densities` (ascending).
struct Photo {
    logical_height: f32,
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
}

pub struct MousePreview {
    model: MouseModel,
    color: DeviceColor,
    /// Textures built from the baked pixels, keyed by model and bake height, so
    /// switching back to a device does not copy them again.
    textures: HashMap<(MouseModel, DeviceColor, u32), Arc<RenderImage>>,
}

impl MousePreview {
    pub fn new(model: MouseModel) -> Self {
        Self {
            model,
            color: DeviceColor::Black,
            textures: HashMap::new(),
        }
    }

    pub fn set_model(&mut self, model: MouseModel, color: DeviceColor, cx: &mut Context<Self>) {
        if self.model != model || self.color != color {
            self.color = color;
            self.model = model;
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
        let logical_height = self.model.photo(self.color).logical_height;
        let photo = self.texture(window.scale_factor());
        // Artwork only. The caption belongs to whoever shows the photo: the
        // details card names the model under it, while a page whose heading is
        // already the device's name would be saying it twice.
        div()
            .w_full()
            .h(px(280.0))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .child(
                img(photo)
                    .w(px(440.0))
                    .h(px(logical_height))
                    .flex_shrink_0()
                    .object_fit(ObjectFit::Contain),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
