use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px, rgb,
};
use opencut_core::{AspectRatio, remove_recent_project};

use crate::editor_view::EditorView;
use crate::theme::Theme;

const RESOLUTION_PRESETS: &[(u32, &str, &str)] = &[
    (720, "720p", "HD"),
    (1080, "1080p", "Full HD"),
    (1440, "1440p", "2K QHD"),
    (2160, "2160p", "4K UHD"),
];

fn ratio_subtitle(ratio: AspectRatio) -> &'static str {
    match ratio {
        AspectRatio::Ratio16x9 => "YouTube / Desktop",
        AspectRatio::Ratio9x16 => "TikTok / Reels",
        AspectRatio::Ratio1x1 => "Instagram / Square",
        AspectRatio::Ratio4x3 => "Classic TV",
        AspectRatio::Ratio3x4 => "Portrait",
        AspectRatio::Ratio21x9 => "Cinema / Ultrawide",
    }
}

fn ratio_preview_box(ratio: AspectRatio) -> (f32, f32) {
    match ratio {
        AspectRatio::Ratio16x9 => (32.0, 18.0),
        AspectRatio::Ratio9x16 => (14.0, 24.0),
        AspectRatio::Ratio1x1 => (22.0, 22.0),
        AspectRatio::Ratio4x3 => (26.0, 20.0),
        AspectRatio::Ratio3x4 => (18.0, 24.0),
        AspectRatio::Ratio21x9 => (36.0, 15.0),
    }
}

impl EditorView {
    pub(crate) fn render_project_hub(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.recent_projects.clone();
        let hub_error = self.hub_error.clone();

        div()
            .id("project-hub-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(Theme::bg_app())
            .text_color(Theme::text_primary())
            // Top bar
            .child(
                div()
                    .h(px(56.))
                    .w_full()
                    .px_6()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(Theme::bg_panel())
                    .border_b_1()
                    .border_color(Theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .px_2p5()
                                    .py_1()
                                    .rounded_md()
                                    .bg(Theme::accent())
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(Theme::text_primary())
                                    .child("OpenCut"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_secondary())
                                    .child("Projects"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .child(
                                self.header_button("hub-open-btn", "📂 Open Project", false)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.prompt_open_project(cx);
                                    })),
                            )
                            .child(
                                self.header_button("hub-create-btn", "+ Create New", true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.draft_ratio = AspectRatio::Ratio16x9;
                                        this.draft_base_height = 1080;
                                        this.create_modal_open = true;
                                        this.hub_error = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            // Main body
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .p_8()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .overflow_hidden()
                    .when_some(hub_error, |el, err| {
                        el.child(
                            div()
                                .px_4()
                                .py_2p5()
                                .rounded_md()
                                .bg(Theme::bg_surface())
                                .border_1()
                                .border_color(rgb(Theme::PLAYHEAD))
                                .text_xs()
                                .text_color(Theme::text_primary())
                                .child(err),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_primary())
                                    .child("Recent Projects"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .child(format!("{} project(s)", recent.len())),
                            ),
                    )
                    .child(if recent.is_empty() {
                        div()
                            .flex_1()
                            .rounded_lg()
                            .bg(Theme::bg_panel())
                            .border_1()
                            .border_dashed()
                            .border_color(Theme::border())
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_primary())
                                    .child("No recent projects yet"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::text_secondary())
                                    .child("Create a new project with your target aspect ratio or open an existing .opencut file."),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        self.header_button(
                                            "empty-create-btn",
                                            "+ Create New Project",
                                            true,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.draft_ratio = AspectRatio::Ratio16x9;
                                            this.draft_base_height = 1080;
                                            this.create_modal_open = true;
                                            cx.notify();
                                        })),
                                    )
                                    .child(
                                        self.header_button(
                                            "empty-open-btn",
                                            "📂 Open .opencut",
                                            false,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.prompt_open_project(cx);
                                        })),
                                    ),
                            )
                    } else {
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_4()
                            .children(recent.into_iter().enumerate().map(|(idx, entry)| {
                                let (w, h) = entry.aspect_ratio.dimensions(entry.base_height);
                                let open_path = entry.path.clone();
                                let remove_path = entry.path.clone();
                                let path_display = entry.path.to_string_lossy().to_string();

                                div()
                                    .id(SharedString::from(format!("recent-card-{}", idx)))
                                    .w(px(280.))
                                    .p_4()
                                    .rounded_lg()
                                    .bg(Theme::bg_panel())
                                    .border_1()
                                    .border_color(Theme::border())
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .cursor_pointer()
                                    .hover(|s| s.border_color(Theme::accent()).bg(Theme::bg_surface()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_project_path(open_path.clone(), cx);
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(Theme::text_primary())
                                                    .truncate()
                                                    .child(entry.name),
                                            )
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("recent-rm-{}", idx)))
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .text_xs()
                                                    .text_color(Theme::text_muted())
                                                    .hover(|s| {
                                                        s.bg(Theme::bg_hover())
                                                            .text_color(Theme::text_primary())
                                                    })
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.recent_projects =
                                                            remove_recent_project(&remove_path);
                                                        cx.notify();
                                                    }))
                                                    .child("✕"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .bg(Theme::bg_surface())
                                                    .border_1()
                                                    .border_color(Theme::border())
                                                    .text_xs()
                                                    .text_color(Theme::accent())
                                                    .child(entry.aspect_ratio.label()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(Theme::text_secondary())
                                                    .child(format!("{}×{}", w, h)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(Theme::text_muted())
                                            .truncate()
                                            .child(path_display),
                                    )
                            }))
                    }),
            )
            // Create New Project Modal
            .when(self.create_modal_open, |el| {
                el.child(self.render_create_project_modal(cx))
            })
    }

    fn render_create_project_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_ratio = self.draft_ratio;
        let selected_height = self.draft_base_height;
        let (out_w, out_h) = selected_ratio.dimensions(selected_height);

        div()
            .id("create-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(gpui::rgba(0x000000aa))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("create-modal-card")
                    .w(px(560.))
                    .p_6()
                    .rounded_xl()
                    .bg(Theme::bg_panel())
                    .border_1()
                    .border_color(Theme::border())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(Theme::text_primary())
                                    .child("Create New Project"),
                            )
                            .child(
                                div()
                                    .id("modal-close-x")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .cursor_pointer()
                                    .hover(|s| {
                                        s.bg(Theme::bg_hover()).text_color(Theme::text_primary())
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.create_modal_open = false;
                                        cx.notify();
                                    }))
                                    .child("✕"),
                            ),
                    )
                    // Aspect Ratio selection
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_secondary())
                                    .child("ASPECT RATIO (DEFAULT 16:9)"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2p5()
                                    .children(AspectRatio::all().iter().map(|&ratio| {
                                        let is_selected = ratio == selected_ratio;
                                        let (pw, ph) = ratio_preview_box(ratio);

                                        div()
                                            .id(SharedString::from(format!(
                                                "ratio-opt-{}",
                                                ratio.label()
                                            )))
                                            .w(px(162.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(if is_selected {
                                                Theme::bg_hover()
                                            } else {
                                                Theme::bg_surface()
                                            })
                                            .border_1()
                                            .border_color(if is_selected {
                                                Theme::accent()
                                            } else {
                                                Theme::border()
                                            })
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .cursor_pointer()
                                            .hover(|s| s.border_color(Theme::accent()))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.draft_ratio = ratio;
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .w(px(38.))
                                                    .h(px(28.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(
                                                        div()
                                                            .w(px(pw))
                                                            .h(px(ph))
                                                            .rounded_sm()
                                                            .border_2()
                                                            .border_color(if is_selected {
                                                                Theme::accent()
                                                            } else {
                                                                Theme::text_muted()
                                                            }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(gpui::FontWeight::BOLD)
                                                            .text_color(Theme::text_primary())
                                                            .child(ratio.label()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(Theme::text_muted())
                                                            .child(ratio_subtitle(ratio)),
                                                    ),
                                            )
                                    })),
                            ),
                    )
                    // Resolution preset selection
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_secondary())
                                    .child("RESOLUTION PRESET"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .children(RESOLUTION_PRESETS.iter().map(
                                        |&(height, title, desc)| {
                                            let is_selected = height == selected_height;
                                            div()
                                                .id(SharedString::from(format!("res-opt-{}", height)))
                                                .flex_1()
                                                .py_2()
                                                .px_3()
                                                .rounded_md()
                                                .bg(if is_selected {
                                                    Theme::bg_hover()
                                                } else {
                                                    Theme::bg_surface()
                                                })
                                                .border_1()
                                                .border_color(if is_selected {
                                                    Theme::accent()
                                                } else {
                                                    Theme::border()
                                                })
                                                .flex()
                                                .flex_col()
                                                .items_center()
                                                .cursor_pointer()
                                                .hover(|s| s.border_color(Theme::accent()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.draft_base_height = height;
                                                    cx.notify();
                                                }))
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_weight(gpui::FontWeight::BOLD)
                                                        .text_color(Theme::text_primary())
                                                        .child(title),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(Theme::text_muted())
                                                        .child(desc),
                                                )
                                        },
                                    )),
                            ),
                    )
                    // Output summary & Actions
                    .child(
                        div()
                            .pt_3()
                            .border_t_1()
                            .border_color(Theme::border())
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::text_secondary())
                                    .child(format!(
                                        "Canvas: {} × {} px • 30 fps",
                                        out_w, out_h
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2p5()
                                    .child(
                                        self.header_button("modal-cancel-btn", "Cancel", false)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.create_modal_open = false;
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        self.header_button("modal-create-btn", "Create", true)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.create_new_project(cx);
                                            })),
                                    ),
                            ),
                    ),
            )
    }
}
