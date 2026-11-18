mod editor_view;
mod media_engine;
mod theme;

use editor_view::EditorView;
use gpui::{
    App, Application, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions,
    prelude::*, px, size,
};

fn main() {
    Application::new().run(|cx: &mut App| {
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
