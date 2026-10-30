//! Color emoji: egui only renders monochrome glyphs, so emoji are drawn as bitmaps taken
//! from the system's Noto Color Emoji font (CBDT PNG strikes) on top of an invisible glyph.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, Color32, Pos2, Rect, TextureHandle};

const FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
];

fn face() -> Option<&'static ttf_parser::Face<'static>> {
    static FACE: OnceLock<Option<ttf_parser::Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| {
        let data = FONT_PATHS.iter().find_map(|p| std::fs::read(p).ok())?;
        ttf_parser::Face::parse(Box::leak(data.into_boxed_slice()), 0).ok()
    })
    .as_ref()
}

/// Cheap pre-filter before asking the font: emoji live outside ASCII and the common text blocks.
fn maybe_emoji(c: char) -> bool {
    let u = c as u32;
    u >= 0x2190 && !(0x2500..=0x257f).contains(&u) && !(0x3000..=0xfeff).contains(&u)
}

/// True when the color emoji font has a bitmap for this character.
pub fn is_emoji(c: char) -> bool {
    maybe_emoji(c) && face().and_then(|f| f.glyph_index(c)).is_some_and(|g| face().unwrap().glyph_raster_image(g, 128).is_some())
}

fn texture(ctx: &egui::Context, c: char) -> Option<TextureHandle> {
    static CACHE: OnceLock<Mutex<HashMap<char, Option<TextureHandle>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    cache
        .entry(c)
        .or_insert_with(|| {
            let f = face()?;
            let img = f.glyph_raster_image(f.glyph_index(c)?, 128)?;
            if img.format != ttf_parser::RasterImageFormat::PNG {
                return None;
            }
            let rgba = image::load_from_memory_with_format(img.data, image::ImageFormat::Png).ok()?.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            Some(ctx.load_texture(format!("emoji-{:x}", c as u32), color, egui::TextureOptions::LINEAR))
        })
        .clone()
}

/// Paint every emoji of a laid-out galley (positioned at `origin`) as a color bitmap.
pub fn paint_galley(painter: &egui::Painter, origin: Pos2, galley: &egui::Galley) {
    for row in &galley.rows {
        for g in &row.glyphs {
            if !is_emoji(g.chr) {
                continue;
            }
            let Some(tex) = texture(painter.ctx(), g.chr) else { continue };
            let h = g.font_height.min(g.line_height) * 1.05;
            let w = (h * tex.aspect_ratio()).max(1.0);
            let center = origin + egui::vec2(g.pos.x + g.advance_width / 2.0, row.rect.min.y + g.line_height / 2.0);
            let rect = Rect::from_center_size(center, egui::vec2(w, h));
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}
