//! PWA install demo — parses a W3C Web App Manifest and registers it with the
//! W3C OS app registry, so a Progressive Web App becomes a first-class
//! installed application.
//!
//! Run with: `cargo run -p w3cos-runtime --example pwa_install`.

use w3cos_runtime::manifest::AppRegistry;
use w3cos_runtime::pwa::{DisplayMode, PwaManifest};

const SAMPLE_MANIFEST: &str = r##"{
    "name": "Notes",
    "short_name": "Notes",
    "description": "Take rich-text notes that sync across devices.",
    "id": "notes",
    "start_url": "/",
    "scope": "/",
    "display": "standalone",
    "display_override": ["window-controls-overlay", "standalone"],
    "background_color": "#ffffff",
    "theme_color": "#1f6feb",
    "lang": "en-US",
    "icons": [
        {"src": "/icon-48.png",  "sizes": "48x48",   "type": "image/png"},
        {"src": "/icon-96.png",  "sizes": "96x96",   "type": "image/png"},
        {"src": "/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any"},
        {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable"}
    ],
    "shortcuts": [
        {"name": "New Note",    "url": "/new"},
        {"name": "Search",      "url": "/search"}
    ],
    "categories": ["productivity", "utilities"]
}"##;

fn main() {
    println!("[pwa] parsing W3C Web App Manifest...");
    let pwa = PwaManifest::from_json(SAMPLE_MANIFEST).expect("manifest should parse");

    println!("  name:                {}", pwa.display_name());
    println!("  description:         {:?}", pwa.description);
    println!("  effective display:   {:?}", pwa.effective_display());
    println!("  theme color:         {:?}", pwa.theme_color);
    println!("  shortcuts:           {}", pwa.shortcuts.len());
    println!("  icons (declared):    {}", pwa.icons.len());

    if let Some(icon) = pwa.pick_icon(192) {
        println!(
            "  best icon @ 192px:   {} ({:?})",
            icon.src, icon.sizes
        );
    }

    let app = pwa.into_app_manifest("notes-fallback");
    println!();
    println!("[pwa] converted to W3C OS AppManifest:");
    println!("  id:        {}", app.id);
    println!("  name:      {}", app.name);
    println!("  version:   {}", app.version);
    println!("  entry:     {}", app.entry);
    println!("  icon:      {:?}", app.icon);
    println!("  frame:     {}", app.window.frame);
    println!("  resizable: {}", app.window.resizable);

    let mut registry = AppRegistry::new();
    registry.register_builtins();
    registry.register(app.clone());
    println!();
    println!(
        "[pwa] registry now has {} apps; '{}' is registered.",
        registry.list().len(),
        app.id
    );

    let demo_modes = [
        DisplayMode::Fullscreen,
        DisplayMode::Standalone,
        DisplayMode::MinimalUi,
        DisplayMode::Browser,
    ];
    println!();
    println!("[pwa] display mode mapping:");
    for mode in demo_modes {
        let frame = !matches!(mode, DisplayMode::Fullscreen);
        println!(
            "  {:>22?} → frame: {}",
            mode,
            if frame { "yes" } else { "no" }
        );
    }
}
