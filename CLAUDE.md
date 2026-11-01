# PrzmTerm — notes for Claude

- Native terminal client in Rust (egui/eframe), clean-room alternative to SecureCRT.
  Read README.md, ROADMAP.md and CONTRIBUTING.md before larger changes.
- Versions go from 0.1.0 to 1.0.0, one roadmap version at a time (ROADMAP.md).
  The workspace version in `Cargo.toml` carries `-dev` between releases.
  Release only with `scripts/release.sh` and only when the version's gate is met
  (docs/VERSIONING.md). Never tag or bump versions unless asked.
- Every user-visible change adds a line under `[Unreleased]` in CHANGELOG.md
  and ticks the matching item in ROADMAP.md.
- Before committing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- Keep the crate layering: `core` ← `transport`/`term`/`session` ← `cli`/`app`.
- egui 0.37 API: `eframe::App::ui(&mut self, ui, frame)`, `egui::Panel::left/bottom`,
  `CentralPanel::no_frame()`, `ctx.fonts_mut(..)`.
- The user writes in Portuguese (pt-BR); code, comments and repo docs are in English.
