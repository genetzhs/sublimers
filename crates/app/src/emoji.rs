//! Color emoji: egui only renders monochrome glyphs, so emoji are drawn as bitmaps taken
//! from the system's Noto Color Emoji font (CBDT PNG strikes) on top of an invisible glyph.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, layers::ShapeIdx, text::{LayoutSection, TextFormat}, Color32, LayerId, Pos2, Rect, Shape, TextureHandle};

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

/// Recolor emoji in every text shape painted this frame (editor, tabs, menus, dialogs...):
/// the monochrome glyph is made transparent and a color bitmap is painted over it.
/// Call once at the end of the frame.
pub fn colorize_frame(ctx: &egui::Context) {
    let mut layers: Vec<LayerId> = ctx.memory(|m| m.areas().visible_layer_ids().into_iter().collect());
    layers.push(LayerId::background());
    layers.sort_by_key(|l| (l.order, l.id.value()));
    layers.dedup();

    // Collect under the graphics lock, lay out outside it (fonts take the same context lock).
    let mut found = Vec::new();
    ctx.graphics(|g| {
        for &layer in &layers {
            let Some(list) = g.get(layer) else { continue };
            for (i, cs) in list.all_entries().enumerate() {
                if let Shape::Text(t) = &cs.shape {
                    if t.galley.job.text.chars().any(is_emoji) {
                        found.push((layer, i, t.clone()));
                    }
                }
            }
        }
    });
    if found.is_empty() {
        return;
    }
    let mut out = Vec::new();
    for (layer, i, mut t) in found {
        let mut job = (*t.galley.job).clone();
        let over = t.override_text_color.take();
        let mut sections = Vec::new();
        for sec in &job.sections {
            let mut fmt = sec.format.clone();
            if let Some(c) = over {
                fmt.color = c;
            } else if fmt.color == Color32::PLACEHOLDER {
                fmt.color = t.fallback_color;
            }
            let text = &job.text[sec.byte_range.clone()];
            let mut start = sec.byte_range.start;
            let mut first = true;
            for (off, ch) in text.char_indices() {
                if is_emoji(ch) {
                    let at = sec.byte_range.start + off;
                    if at > start {
                        sections.push(LayoutSection { leading_space: if first { sec.leading_space } else { 0.0 }, byte_range: start..at, format: fmt.clone() });
                        first = false;
                    }
                    let end = at + ch.len_utf8();
                    sections.push(LayoutSection { leading_space: if first { sec.leading_space } else { 0.0 }, byte_range: at..end, format: TextFormat { color: Color32::TRANSPARENT, ..fmt.clone() } });
                    first = false;
                    start = end;
                }
            }
            if start < sec.byte_range.end || first {
                sections.push(LayoutSection { leading_space: if first { sec.leading_space } else { 0.0 }, byte_range: start..sec.byte_range.end, format: fmt });
            }
        }
        job.sections = sections;
        t.galley = ctx.fonts(|f| f.layout_job(job));
        let images = emoji_images(ctx, t.pos, &t.galley);
        out.push((layer, i, t, images));
    }
    ctx.graphics_mut(|g| {
        for (layer, i, t, images) in out {
            let Some(list) = g.get_mut(layer) else { continue };
            list.mutate_shape(ShapeIdx(i), |cs| cs.shape = Shape::Vec(std::iter::once(Shape::Text(t)).chain(images).collect()));
        }
    });
}

fn emoji_images(ctx: &egui::Context, origin: Pos2, galley: &egui::Galley) -> Vec<Shape> {
    let mut v = Vec::new();
    for row in &galley.rows {
        for g in &row.glyphs {
            if !is_emoji(g.chr) {
                continue;
            }
            let Some(tex) = texture(ctx, g.chr) else { continue };
            let h = g.font_height.min(g.line_height) * 1.05;
            let w = (h * tex.aspect_ratio()).max(1.0);
            let center = origin + egui::vec2(g.pos.x + g.advance_width / 2.0, row.rect.min.y + g.line_height / 2.0);
            let rect = Rect::from_center_size(center, egui::vec2(w, h));
            v.push(Shape::image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE));
        }
    }
    v
}

/// Calls [`colorize_frame`] when dropped, i.e. after the rest of the frame has been painted.
pub struct FrameEnd(pub egui::Context);

impl Drop for FrameEnd {
    fn drop(&mut self) {
        colorize_frame(&self.0);
    }
}
