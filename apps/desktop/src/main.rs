mod editor_view;
mod media_engine;
mod project_hub;
mod theme;

use std::borrow::Cow;
use std::path::PathBuf;

use editor_view::EditorView;
use gpui::{
    App, Application, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions,
    prelude::*, px, size,
};

fn register_bundled_fonts(cx: &mut App) {
    let candidates = [
        PathBuf::from("apps/desktop/assets/fonts"),
        PathBuf::from("assets/fonts"),
    ];
    let mut blobs: Vec<Cow<'static, [u8]>> = Vec::new();
    for dir in candidates {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let is_font = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf"))
                    .unwrap_or(false);
                if is_font {
                    if let Ok(bytes) = std::fs::read(&p) {
                        blobs.push(Cow::Owned(bytes));
                    }
                }
            }
        }
    }
    if !blobs.is_empty() {
        let _ = cx.text_system().add_fonts(blobs);
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        register_bundled_fonts(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("OpenCut")),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(EditorView::new);
                let focus = view.read(cx).focus_handle().clone();
                window.focus(&focus);
                view
            },
        )
        .expect("failed to open the main window");
    });
}
