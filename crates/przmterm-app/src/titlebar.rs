//! Windows: the title bar in the interface's own colours, as a part of the
//! window rather than the system's bar above it. Windows 11 draws it in the
//! colours given at once; Windows 10 cannot colour it and keeps following
//! the look's light or dark (egui's theme sync).

use eframe::egui;

/// The colours last given to the title bar.
#[derive(Default)]
pub struct TitleBar {
    shown: Option<bool>,
    /// Given once more on the next frame: egui switches the window's own
    /// dark mode only after this frame, and Windows redraws the bar's
    /// buttons from it.
    again: bool,
}

impl TitleBar {
    /// Called every frame: colours the title bar when the look changes.
    pub fn follow(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.shown != Some(dark) {
            self.shown = Some(dark);
            self.again = true;
            paint(frame, dark);
            ctx.request_repaint();
        } else if self.again {
            self.again = false;
            paint(frame, dark);
        }
    }
}

#[cfg(windows)]
fn paint(frame: &eframe::Frame, dark: bool) {
    use winit::platform::windows::{Color, WindowExtWindows as _};
    let Some(window) = frame.winit_window() else {
        return;
    };
    let pal = if dark {
        &crate::theme::DARK
    } else {
        &crate::theme::LIGHT
    };
    let color = |c: egui::Color32| Color::from_rgb(c.r(), c.g(), c.b());
    // The bar in the colour of the bar under it (Quick connect), its title
    // in the interface's text colour, the window's edge in its lines'.
    window.set_title_background_color(Some(color(pal.chrome)));
    window.set_title_text_color(color(pal.text));
    window.set_border_color(Some(color(pal.border)));
    tracing::debug!("title bar {}", if dark { "dark" } else { "light" });
}

#[cfg(not(windows))]
fn paint(_frame: &eframe::Frame, _dark: bool) {}
