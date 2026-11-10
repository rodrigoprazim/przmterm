//! Commands of the application itself (About, Settings, the vault…): in the
//! menu bar on macOS, and in the session panel's **+** menu everywhere.

use std::sync::Mutex;

use eframe::egui;

/// The project's page, shown in About and opened from the Help menu.
pub const WEBSITE: &str = "https://github.com/rodrigoprazim/przmterm";
/// The developer's page, shown in About.
pub const DEVELOPER_PAGE: &str = "https://github.com/rodrigoprazim";
pub const DEVELOPER: &str = "Rodrigo Prazim";

// Some commands are only in the macOS menu bar.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    CheckUpdates,
    Settings,
    Appearance,
    Shortcuts,
    Vault,
    Keywords,
    SettingsFolder,
    ScriptGuide,
    Website,
    /// Quit, asking first when sessions are open (unlike the system's).
    Quit,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl AppCommand {
    const ALL: [AppCommand; 11] = [
        Self::About,
        Self::CheckUpdates,
        Self::Settings,
        Self::Appearance,
        Self::Shortcuts,
        Self::Vault,
        Self::Keywords,
        Self::SettingsFolder,
        Self::ScriptGuide,
        Self::Website,
        Self::Quit,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::About => "about",
            Self::CheckUpdates => "check-updates",
            Self::Settings => "settings",
            Self::Appearance => "appearance",
            Self::Shortcuts => "shortcuts",
            Self::Vault => "vault",
            Self::Keywords => "keywords",
            Self::SettingsFolder => "settings-folder",
            Self::ScriptGuide => "script-guide",
            Self::Website => "website",
            Self::Quit => "quit",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
}

/// Commands chosen in the menu bar, waiting for the next frame.
static CHOSEN: Mutex<Vec<AppCommand>> = Mutex::new(Vec::new());

/// What was chosen in the menu bar since the last call.
pub fn take() -> Vec<AppCommand> {
    CHOSEN
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn choose(id: &str, ctx: &egui::Context) {
    if let Some(command) = AppCommand::from_id(id) {
        if let Ok(mut chosen) = CHOSEN.lock() {
            chosen.push(command);
        }
        ctx.request_repaint();
    }
}

/// The menu bar; it stays while this is kept.
pub struct MenuBar {
    #[cfg(target_os = "macos")]
    _menu: muda::Menu,
}

/// Puts PrzmTerm's menus in the macOS menu bar, replacing the one winit
/// makes (whose items say the executable's name, "przmterm"). Nothing
/// elsewhere: Windows and Linux windows have no menu bar.
#[cfg(target_os = "macos")]
pub fn install(ctx: &egui::Context) -> Option<MenuBar> {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    let item = |command: AppCommand, text: &str, key: Option<Code>| {
        MenuItem::with_id(
            command.id(),
            text,
            true,
            key.map(|k| Accelerator::new(Modifiers::META, k)),
        )
    };
    let app = Submenu::with_items(
        "PrzmTerm",
        true,
        &[
            &item(AppCommand::About, "About PrzmTerm", None),
            &item(AppCommand::CheckUpdates, "Check for Updates…", None),
            &PredefinedMenuItem::separator(),
            &item(AppCommand::Settings, "Settings…", Some(Code::Comma)),
            &item(AppCommand::Appearance, "Appearance…", None),
            &item(AppCommand::Shortcuts, "Keyboard Shortcuts…", None),
            &item(AppCommand::Vault, "Password Vault…", None),
            &item(AppCommand::Keywords, "Keyword Sets…", None),
            &item(AppCommand::SettingsFolder, "Settings Folder…", None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Hide PrzmTerm")),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &item(AppCommand::Quit, "Quit PrzmTerm", Some(Code::KeyQ)),
        ],
    )
    .ok()?;
    let help = Submenu::with_items(
        "Help",
        true,
        &[
            &item(AppCommand::ScriptGuide, "Script Guide", None),
            &item(AppCommand::Website, "PrzmTerm on GitHub", None),
        ],
    )
    .ok()?;
    let menu = Menu::with_items(&[&app, &help]).ok()?;
    menu.init_for_nsapp();
    let ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        choose(event.id().as_ref(), &ctx)
    }));
    Some(MenuBar { _menu: menu })
}

#[cfg(not(target_os = "macos"))]
pub fn install(_ctx: &egui::Context) -> Option<MenuBar> {
    None
}

/// The About window's contents.
pub fn about(ui: &mut egui::Ui, icon: Option<&egui::TextureHandle>) {
    ui.set_max_width(300.0);
    ui.vertical_centered(|ui| {
        if let Some(icon) = icon {
            ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(96.0, 96.0)));
        }
        ui.heading("PrzmTerm");
        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
        ui.add_space(6.0);
        ui.label("A terminal for network and systems engineers.");
        ui.add_space(6.0);
        ui.label(format!("Developed by {DEVELOPER}"));
        ui.hyperlink_to("github.com/rodrigoprazim", DEVELOPER_PAGE);
        ui.hyperlink_to("github.com/rodrigoprazim/przmterm", WEBSITE);
        ui.add_space(6.0);
        ui.weak("MIT OR Apache-2.0");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_come_back_from_their_ids() {
        for command in AppCommand::ALL {
            assert_eq!(AppCommand::from_id(command.id()), Some(command));
        }
        assert_eq!(AppCommand::from_id("nothing"), None);
        let ctx = egui::Context::default();
        choose("vault", &ctx);
        choose("nothing", &ctx);
        choose("about", &ctx);
        assert_eq!(take(), [AppCommand::Vault, AppCommand::About]);
        assert!(take().is_empty());
    }
}
