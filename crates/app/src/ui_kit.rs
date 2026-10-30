//! Shared look for popups (update card, save prompt, file dialogs, build picker):
//! a neutral elevated card, crisp status badges and one set of buttons.

use eframe::egui::{self, Color32, CornerRadius, FontId, RichText, Stroke, Ui};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub fn color(self) -> Color32 {
        match self {
            Tone::Info => Color32::from_rgb(0x3b, 0x82, 0xf6),
            Tone::Success => Color32::from_rgb(0x22, 0xa3, 0x5a),
            Tone::Warning => Color32::from_rgb(0xd9, 0x77, 0x06),
            Tone::Danger => Color32::from_rgb(0xe5, 0x48, 0x4d),
        }
    }
}

/// Card surface: deep navy in dark themes, white in light ones (independent of the color scheme).
fn surface(v: &egui::Visuals) -> Color32 {
    if v.dark_mode { Color32::from_rgb(0x16, 0x1b, 0x26) } else { Color32::WHITE }
}

/// Text color on the card surface.
pub fn ink(v: &egui::Visuals) -> Color32 {
    if v.dark_mode { Color32::from_rgb(0xe8, 0xec, 0xf4) } else { Color32::from_rgb(0x1a, 0x1f, 0x2b) }
}

pub fn muted(v: &egui::Visuals) -> Color32 {
    ink(v).gamma_multiply(0.62)
}

fn hairline(v: &egui::Visuals) -> Color32 {
    if v.dark_mode { Color32::from_white_alpha(22) } else { Color32::from_black_alpha(28) }
}

pub fn card_frame(v: &egui::Visuals) -> egui::Frame {
    egui::Frame::new()
        .fill(surface(v))
        .stroke(Stroke::new(1.0_f32, hairline(v)))
        .corner_radius(16.0)
        .shadow(egui::Shadow { offset: [0, 18], blur: 56, spread: 0, color: Color32::from_black_alpha(if v.dark_mode { 150 } else { 60 }) })
        .inner_margin(egui::Margin::same(26))
}

/// Inset panel inside a card (release notes, details).
pub fn inset_frame(v: &egui::Visuals) -> egui::Frame {
    let fill = if v.dark_mode { Color32::from_black_alpha(55) } else { Color32::from_black_alpha(10) };
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0_f32, hairline(v))).corner_radius(10.0).inner_margin(egui::Margin::same(14))
}

/// A centred modal card over a dimmed backdrop. Returns true if the backdrop was clicked.
pub fn modal(ctx: &egui::Context, id: &str, width: f32, add: impl FnOnce(&mut Ui)) -> bool {
    let screen = ctx.screen_rect();
    let mut clicked = false;
    egui::Area::new(egui::Id::new((id, "backdrop"))).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let r = ui.allocate_rect(screen, egui::Sense::click());
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(150));
        clicked = r.clicked();
    });
    egui::Area::new(egui::Id::new(id)).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0]).show(ctx, |ui| {
        let v = ui.visuals().clone();
        card_frame(&v).show(ui, |ui| {
            ui.set_width(width);
            ui.visuals_mut().override_text_color = Some(ink(&v));
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
            add(ui);
        });
    });
    clicked
}

/// Header row: status badge, title and an optional muted subtitle.
pub fn header(ui: &mut Ui, tone: Tone, icon: &str, busy: bool, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(46.0, 46.0), egui::Sense::hover());
        let col = tone.color();
        let p = ui.painter();
        p.circle_filled(r.center(), 23.0, col.gamma_multiply(0.16));
        p.circle_filled(r.center(), 17.0, col);
        if busy {
            egui::Spinner::new().size(18.0).color(Color32::WHITE).paint_at(ui, egui::Rect::from_center_size(r.center(), egui::vec2(18.0, 18.0)));
        } else {
            p.text(r.center(), egui::Align2::CENTER_CENTER, icon, FontId::proportional(18.0), Color32::WHITE);
        }
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.add_space(2.0);
            ui.label(RichText::new(title).size(19.0).strong());
            if !subtitle.is_empty() {
                let m = muted(ui.visuals());
                ui.add(egui::Label::new(RichText::new(subtitle).size(14.0).color(m)).wrap());
            }
        });
    });
}

fn button_size() -> egui::Vec2 {
    egui::vec2(104.0, 36.0)
}

/// Solid button for the main action.
pub fn primary(text: &str, tone: Tone) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).size(14.5).strong().color(Color32::WHITE))
        .fill(tone.color())
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(9))
        .min_size(button_size())
}

/// Quiet outlined button for secondary actions.
pub fn secondary(ui: &Ui, text: &str) -> egui::Button<'static> {
    let v = ui.visuals();
    let fill = if v.dark_mode { Color32::from_rgb(0x22, 0x2a, 0x3a) } else { Color32::from_rgb(0xf1, 0xf3, 0xf7) };
    egui::Button::new(RichText::new(text.to_owned()).size(14.5))
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, hairline(v)))
        .corner_radius(CornerRadius::same(9))
        .min_size(button_size())
}

/// Frameless text button (e.g. a destructive "Don't Save" off to the side).
pub fn ghost(text: &str, tone: Tone) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).size(14.5).color(tone.color())).frame(false).min_size(egui::vec2(0.0, 36.0))
}

/// Text field styled to sit inside a card.
pub fn text_field(ui: &mut Ui, value: &mut String) -> egui::Response {
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY).font(FontId::proportional(15.0)).margin(egui::Margin::symmetric(10, 8)))
}
