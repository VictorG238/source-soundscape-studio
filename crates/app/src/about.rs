use crate::TITLE;
use eframe::egui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const PROFILE: &str = env!("CARGO_PKG_HOMEPAGE");
const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
const LICENSE: &str = env!("CARGO_PKG_LICENSE");

const WAVE: [u8; 9] = [3, 6, 11, 7, 3, 9, 13, 7, 3];

pub fn icon() -> egui::IconData {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0i32..32 {
        for x in 0..32 {
            let bar = (4..31).contains(&x)
                && (x - 4) % 3 < 2
                && (y - 16).abs() < i32::from(WAVE[(x - 4) / 3]);
            rgba.extend(if bar {
                [127, 219, 202, 255]
            } else {
                [26, 32, 39, 255]
            });
        }
    }
    egui::IconData {
        rgba,
        width: 32,
        height: 32,
    }
}

pub fn show(ctx: &egui::Context, open: &mut bool) -> bool {
    let mut check = false;
    egui::Window::new(format!("About {TITLE}"))
        .open(open)
        .resizable(false)
        .show(ctx, |ui| {
            let (response, painter) =
                ui.allocate_painter(egui::vec2(32., 32.), egui::Sense::hover());
            let center = response.rect.center();
            for (i, height) in WAVE.into_iter().enumerate() {
                let x = center.x + (i as f32 - 4.) * 3.;
                let h = f32::from(height);
                painter.line_segment(
                    [egui::pos2(x, center.y - h), egui::pos2(x, center.y + h)],
                    (2., egui::Color32::from_rgb(127, 219, 202)),
                );
            }
            ui.heading(TITLE);
            ui.label(format!("Version {VERSION}"));
            ui.label(format!(
                "{} · {}",
                env!("SSS_BUILD_TARGET"),
                env!("SSS_BUILD_PROFILE")
            ));
            if let Some(commit) = option_env!("SSS_BUILD_COMMIT") {
                ui.label(format!("Commit {commit}"));
            }
            for (label, value) in [("Author", AUTHOR), ("License", LICENSE)] {
                let value = if value.is_empty() { "Pending" } else { value };
                ui.label(format!("{label} · {value}"));
            }
            for (label, url) in [
                ("GitHub profile", PROFILE),
                ("Project repository", REPOSITORY),
            ] {
                if !url.is_empty() {
                    ui.hyperlink_to(label, url);
                }
            }
            ui.separator();
            ui.label("Libraries");
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("eframe / egui", "https://github.com/emilk/egui");
                ui.hyperlink_to("CPAL", "https://github.com/RustAudio/cpal");
                ui.hyperlink_to("Symphonia", "https://github.com/pdeljanov/Symphonia");
                ui.hyperlink_to("rfd", "https://github.com/PolyMeilex/rfd");
            });
            check = ui.button("Check for updates").clicked();
        });
    check
}
