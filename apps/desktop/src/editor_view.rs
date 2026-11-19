use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{
    AnyView, AppContext, Context, ExternalPaths, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement, PathPromptOptions,
    Render, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, Window, div, img, prelude::FluentBuilder, px, rgb,
};
use opencut_core::{
    AspectRatio, EditorCore, RecentProjectEntry, TrackKind, format_timecode, load_recent_projects,
    record_recent_project,
};

use crate::media_engine::{
    AudioStreamPlayer, ExportHandle, VideoStreamPlayer, decode_single_frame, probe_duration_secs,
    reveal_in_explorer, start_export_timeline,
};
use crate::theme::Theme;

const TRACK_HEADER_W: f32 = 124.0;

struct TooltipView {
    text: SharedString,
}

impl TooltipView {
    fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2p5()
            .py_1()
            .rounded_md()
            .bg(Theme::bg_surface())
            .border_1()
            .border_color(Theme::border())
            .shadow_md()
            .text_xs()
            .text_color(Theme::text_primary())
            .child(self.text.clone())
    }
}

fn build_tooltip(text: &'static str) -> impl Fn(&mut Window, &mut gpui::App) -> AnyView + 'static {
    move |_window, cx| cx.new(|_| TooltipView::new(text)).into()
}

enum ExportStatus {
    Idle,
    Exporting { handle: ExportHandle, progress: f32 },
    Done { output_path: PathBuf },
    Error { message: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipDragKind {
    Move,
    TrimLeft,
    TrimRight,
}

#[derive(Clone, Copy)]
struct ClipDragState {
    clip_id: usize,
    kind: ClipDragKind,
    start_mouse_x: f32,
    initial_start_secs: f32,
    initial_duration_secs: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppScreen {
    ProjectHub,
    Editor,
}

pub struct EditorView {
    pub(crate) state: EditorCore,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) screen: AppScreen,
    pub(crate) recent_projects: Vec<RecentProjectEntry>,
    pub(crate) create_modal_open: bool,
    pub(crate) draft_ratio: AspectRatio,
    pub(crate) draft_base_height: u32,
    pub(crate) hub_error: Option<String>,
    playback_started: bool,
    current_frame: Option<Arc<RenderImage>>,
    player: VideoStreamPlayer,
    audio_player: AudioStreamPlayer,
    decode_seq: u64,
    timeline_scroll_x: f32,
    scrollbar_dragging: bool,
    scrollbar_drag_start_x: f32,
    scrollbar_drag_start_scroll: f32,
    playhead_dragging: bool,
    clip_drag: Option<ClipDragState>,
    export_status: ExportStatus,
}

impl EditorView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            state: EditorCore::new(),
            focus_handle: cx.focus_handle(),
            screen: AppScreen::ProjectHub,
            recent_projects: load_recent_projects(),
            create_modal_open: false,
            draft_ratio: AspectRatio::Ratio16x9,
            draft_base_height: 1080,
            hub_error: None,
            playback_started: false,
            current_frame: None,
            player: VideoStreamPlayer::new(),
            audio_player: AudioStreamPlayer::new(),
            decode_seq: 0,
            timeline_scroll_x: 0.0,
            scrollbar_dragging: false,
            scrollbar_drag_start_x: 0.0,
            scrollbar_drag_start_scroll: 0.0,
            playhead_dragging: false,
            clip_drag: None,
            export_status: ExportStatus::Idle,
        };
        view.ensure_playback_loop(cx);
        view
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn seek_from_window_x(&mut self, window_x: f32, window: &Window, cx: &mut Context<Self>) {
        let vp_w = (f32::from(window.viewport_size().width) - TRACK_HEADER_W).max(200.0);
        let timeline_w = (self.state.total_duration_secs * self.state.px_per_sec).max(vp_w);
        let max_scroll = (timeline_w - vp_w).max(0.0);
        let scroll_x = self.timeline_scroll_x.clamp(0.0, max_scroll);

        let x_in_tracks = (window_x - TRACK_HEADER_W).max(0.0);
        let secs = (scroll_x + x_in_tracks) / self.state.px_per_sec;
        self.state.seek_to(secs);
        self.sync_preview_after_seek(cx);
        cx.notify();
    }

    fn set_current_frame(&mut self, next: Option<Arc<RenderImage>>, cx: &mut Context<Self>) {
        if let Some(old) = self.current_frame.take() {
            cx.drop_image(old, None);
        }
        self.current_frame = next;
    }

    fn ensure_playback_loop(&mut self, cx: &mut Context<Self>) {
        if self.playback_started {
            return;
        }
        self.playback_started = true;

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;

                let should_continue = this.update(cx, |view, cx| {
                    let mut changed = false;

                    if view.state.is_playing {
                        view.state.tick(0.04);
                        view.poll_playback_frame(cx);
                        changed = true;
                    } else {
                        view.audio_player.stop();
                    }

                    if view.poll_export_progress() {
                        changed = true;
                    }

                    if changed {
                        cx.notify();
                    }
                });

                if should_continue.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll_playback_frame(&mut self, cx: &mut Context<Self>) {
        let audio_clips = self.state.active_audio_clips_at(self.state.playhead_secs);
        self.audio_player
            .sync_playing_clips(&audio_clips, self.state.playhead_secs);

        let Some(active) = self.state.active_video_clip().cloned() else {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        };

        if active.track != TrackKind::Video {
            self.player.stop();
            return;
        }

        let Some(path) = active.source_path else {
            return;
        };

        if self.player.active_clip_id() != Some(active.id)
            || self.player.active_ratio() != Some(self.state.aspect_ratio)
        {
            let offset =
                active.source_in_secs + (self.state.playhead_secs - active.start_secs).max(0.0);
            self.player
                .start(active.id, path, offset, self.state.aspect_ratio);
        }

        if let Some(frame) = self.player.take_latest_frame() {
            self.set_current_frame(Some(frame), cx);
        }
    }

    fn sync_preview_after_seek(&mut self, cx: &mut Context<Self>) {
        self.audio_player.stop();
        if self.state.is_playing {
            let audio_clips = self.state.active_audio_clips_at(self.state.playhead_secs);
            self.audio_player
                .sync_playing_clips(&audio_clips, self.state.playhead_secs);
        }

        let Some(active) = self.state.active_video_clip().cloned() else {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        };

        if active.track != TrackKind::Video {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        }

        let Some(path) = active.source_path else {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        };

        let offset =
            active.source_in_secs + (self.state.playhead_secs - active.start_secs).max(0.0);
        if self.state.is_playing {
            self.player
                .start(active.id, path, offset, self.state.aspect_ratio);
        } else {
            self.player.stop();
            self.request_single_frame(path, offset, cx);
        }
    }

    fn request_single_frame(&mut self, path: PathBuf, offset_secs: f32, cx: &mut Context<Self>) {
        self.decode_seq = self.decode_seq.wrapping_add(1);
        let seq = self.decode_seq;
        let ratio = self.state.aspect_ratio;

        let task = cx
            .background_executor()
            .spawn(async move { decode_single_frame(&path, offset_secs, ratio) });

        cx.spawn(async move |this, cx| {
            let frame = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.decode_seq == seq && !view.state.is_playing {
                    view.set_current_frame(frame, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn prompt_import_media(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(SharedString::from("Import Video, Image, or Audio")),
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |view, cx| {
                    view.state.import_paths_with_probe(paths, probe_duration_secs);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn create_new_project(&mut self, cx: &mut Context<Self>) {
        self.player.stop();
        self.audio_player.stop();
        self.current_frame = None;
        self.timeline_scroll_x = 0.0;
        self.export_status = ExportStatus::Idle;

        let mut core = EditorCore::new();
        core.aspect_ratio = self.draft_ratio;
        core.base_height = self.draft_base_height;
        let (w, h) = core.aspect_ratio.dimensions(core.base_height);
        core.status_message = format!("New project ({} • {}x{})", core.aspect_ratio.label(), w, h);

        self.state = core;
        self.create_modal_open = false;
        self.hub_error = None;
        self.screen = AppScreen::Editor;
        cx.notify();
    }

    pub(crate) fn open_project_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.player.stop();
        self.audio_player.stop();
        self.current_frame = None;
        self.timeline_scroll_x = 0.0;
        self.export_status = ExportStatus::Idle;

        match self.state.load_from_file(&path) {
            Ok(()) => {
                self.recent_projects = record_recent_project(RecentProjectEntry {
                    name: self.state.project_name.clone(),
                    path,
                    aspect_ratio: self.state.aspect_ratio,
                    base_height: self.state.base_height,
                });
                self.hub_error = None;
                self.screen = AppScreen::Editor;
                self.sync_preview_after_seek(cx);
            }
            Err(err) => {
                self.hub_error = Some(err.clone());
                self.state.status_message = err;
            }
        }
        cx.notify();
    }

    pub(crate) fn prompt_open_project(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("Open .opencut Project")),
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |view, cx| {
                        view.open_project_path(path, cx);
                    });
                }
            }
        })
        .detach();
    }

    fn save_project(&mut self, cx: &mut Context<Self>) {
        if let Some(existing) = self.state.project_path.clone() {
            match self.state.save_to_file(&existing) {
                Ok(()) => {
                    self.recent_projects = record_recent_project(RecentProjectEntry {
                        name: self.state.project_name.clone(),
                        path: existing,
                        aspect_ratio: self.state.aspect_ratio,
                        base_height: self.state.base_height,
                    });
                }
                Err(err) => self.state.status_message = err,
            }
            cx.notify();
            return;
        }

        let dir = PathBuf::from(".");
        let suggested = format!("{}.opencut", self.state.project_name.replace(' ', "_"));
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = rx.await {
                if path.extension().is_none() {
                    path.set_extension("opencut");
                }
                let _ = this.update(cx, |view, cx| {
                    match view.state.save_to_file(&path) {
                        Ok(()) => {
                            view.recent_projects = record_recent_project(RecentProjectEntry {
                                name: view.state.project_name.clone(),
                                path,
                                aspect_ratio: view.state.aspect_ratio,
                                base_height: view.state.base_height,
                            });
                        }
                        Err(err) => view.state.status_message = err,
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let ctrl = event.keystroke.modifiers.control;
        let shift = event.keystroke.modifiers.shift;

        match (ctrl, shift, key) {
            (false, false, "space") => {
                self.state.toggle_play();
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (true, false, "z") => {
                if self.state.undo() {
                    self.sync_preview_after_seek(cx);
                }
                cx.notify();
            }
            (true, false, "y") | (true, true, "z") => {
                if self.state.redo() {
                    self.sync_preview_after_seek(cx);
                }
                cx.notify();
            }
            (true, false, "b") => {
                self.state.split_selected_or_at_playhead();
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (true, false, "s") => {
                self.save_project(cx);
            }
            (false, true, "delete") | (false, true, "backspace") => {
                self.state.ripple_delete_selected();
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (false, false, "delete") | (false, false, "backspace") => {
                self.state.delete_selected_clip();
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (false, false, "left") => {
                self.state.step_by(-0.5);
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (false, false, "right") => {
                self.state.step_by(0.5);
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            _ => {}
        }
    }

    fn poll_export_progress(&mut self) -> bool {
        let ExportStatus::Exporting { handle, progress } = &mut self.export_status else {
            return false;
        };

        let mut updated = false;
        if let Ok(pct) = handle.progress_pct.lock() {
            if (*pct - *progress).abs() >= 0.5 || *pct >= 100.0 {
                *progress = *pct;
                self.state.status_message = format!("Exporting... {:.0}%", *progress);
                updated = true;
            }
        }

        if handle.finished.load(Ordering::Relaxed) {
            let err_msg = handle.error.lock().ok().and_then(|e| e.clone());
            let out_path = handle.output_path.clone();

            if let Some(msg) = err_msg {
                self.state.status_message = "Export failed".into();
                self.export_status = ExportStatus::Error { message: msg };
            } else {
                self.state.status_message = format!("Exported to {}", out_path.display());
                self.export_status = ExportStatus::Done {
                    output_path: out_path,
                };
            }
            return true;
        }

        updated
    }

    fn prompt_export_video(&mut self, cx: &mut Context<Self>) {
        if matches!(self.export_status, ExportStatus::Exporting { .. }) {
            return;
        }

        if self.state.clips.is_empty() {
            self.export_status = ExportStatus::Error {
                message: "Add at least one clip to the timeline before exporting.".into(),
            };
            self.state.status_message = "Timeline is empty".into();
            cx.notify();
            return;
        }

        self.state.is_playing = false;
        self.player.stop();
        self.audio_player.stop();

        let default_dir = self
            .state
            .clips
            .iter()
            .filter_map(|c| c.source_path.as_ref()?.parent().map(|p| p.to_path_buf()))
            .next()
            .unwrap_or_else(|| PathBuf::from("."));

        let rx = cx.prompt_for_new_path(&default_dir, Some("opencut_export.mp4"));

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = rx.await {
                if path.extension().is_none() {
                    path.set_extension("mp4");
                }

                let _ = this.update(cx, |view, cx| {
                    let clips = view.state.clips.clone();
                    let flags = view.state.track_flags;
                    let ratio = view.state.aspect_ratio;
                    let base_height = view.state.base_height;
                    let handle =
                        start_export_timeline(clips, flags, ratio, base_height, path.clone());
                    view.state.status_message = "Exporting... 0%".into();
                    view.export_status = ExportStatus::Exporting {
                        handle,
                        progress: 0.0,
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn render_export_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        match &self.export_status {
            ExportStatus::Idle => None,
            ExportStatus::Exporting { handle, progress } => {
                let pct = progress.clamp(0.0, 100.0);
                let path_str = handle.output_path.display().to_string();

                Some(
                    div()
                        .w_full()
                        .px_4()
                        .py_2()
                        .bg(Theme::bg_surface())
                        .border_b_1()
                        .border_color(Theme::accent())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(Theme::text_primary())
                                                .child(format!("Exporting MP4... {:.0}%", pct)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(Theme::text_secondary())
                                                .child(format!("Saving to: {}", path_str)),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(6.))
                                        .rounded_full()
                                        .bg(Theme::bg_panel())
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .h_full()
                                                .w(gpui::relative(pct / 100.0))
                                                .bg(Theme::accent()),
                                        ),
                                ),
                        ),
                )
            }
            ExportStatus::Done { output_path } => {
                let path_str = output_path.display().to_string();
                let open_path = output_path.clone();

                Some(
                    div()
                        .w_full()
                        .px_4()
                        .py_2()
                        .bg(Theme::bg_surface())
                        .border_b_1()
                        .border_color(rgb(Theme::SUCCESS))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .px_2()
                                        .py_0p5()
                                        .rounded_sm()
                                        .bg(rgb(Theme::SUCCESS))
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(Theme::text_primary())
                                        .child("100% Done"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(Theme::text_primary())
                                        .child(format!("Saved to: {}", path_str)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    self.header_button("export-open-folder", "Open Folder", true)
                                        .on_click(cx.listener(move |_, _, _, _| {
                                            reveal_in_explorer(&open_path);
                                        })),
                                )
                                .child(
                                    self.header_button("export-dismiss", "Close", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.export_status = ExportStatus::Idle;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            }
            ExportStatus::Error { message } => Some(
                div()
                    .w_full()
                    .px_4()
                    .py_2()
                    .bg(Theme::bg_surface())
                    .border_b_1()
                    .border_color(rgb(Theme::PLAYHEAD))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(Theme::PLAYHEAD))
                            .child(format!("Export error: {}", message)),
                    )
                    .child(
                        self.header_button("export-err-dismiss", "Close", false)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_status = ExportStatus::Idle;
                                cx.notify();
                            })),
                    ),
            ),
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.state.status_message.clone();
        let project_name = self.state.project_name.clone();

        div()
            .h(px(44.))
            .w_full()
            .px_4()
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
                    .gap_2p5()
                    .child(
                        self.icon_button("btn-home-hub", "🏠", "Back to Projects", false, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.is_playing = false;
                                this.player.stop();
                                this.audio_player.stop();
                                this.recent_projects = load_recent_projects();
                                this.screen = AppScreen::ProjectHub;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(Theme::accent())
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(Theme::text_primary())
                            .child("OpenCut"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Theme::text_primary())
                            .child(project_name),
                    )
                    .child(
                        self.icon_button("btn-open-proj", "📂", "Open Project (.opencut)", false, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prompt_open_project(cx);
                            })),
                    )
                    .child(
                        self.icon_button("btn-save-proj", "💾", "Save Project (Ctrl+S)", false, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_project(cx);
                            })),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(Theme::bg_surface())
                            .text_xs()
                            .text_color(Theme::text_secondary())
                            .child(status),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        self.header_button(
                            "btn-ratio",
                            SharedString::from(format!("Ratio: {}", self.state.aspect_ratio.label())),
                            false,
                        )
                        .tooltip(build_tooltip("Cycle Aspect Ratio (16:9, 9:16, 1:1...)"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let next = this.state.aspect_ratio.next();
                            this.state.set_aspect_ratio(next);
                            this.player.stop();
                            this.sync_preview_after_seek(cx);
                            cx.notify();
                        })),
                    )
                    .child(
                        self.header_button(
                            "btn-export",
                            if matches!(self.export_status, ExportStatus::Exporting { .. }) {
                                "Exporting..."
                            } else {
                                "⬆ Export"
                            },
                            true,
                        )
                        .tooltip(build_tooltip("Export Video (MP4)"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.prompt_export_video(cx);
                        })),
                    ),
            )
    }

    fn icon_button(
        &self,
        id: &'static str,
        icon: &'static str,
        tooltip: &'static str,
        active: bool,
        enabled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let bg = if active {
            Theme::accent()
        } else {
            Theme::bg_surface()
        };
        let hover_bg = if active {
            rgb(Theme::ACCENT_HOVER)
        } else {
            Theme::bg_hover()
        };

        div()
            .id(id)
            .w(px(28.))
            .h(px(26.))
            .rounded_md()
            .bg(bg)
            .border_1()
            .border_color(if active {
                Theme::accent()
            } else {
                Theme::border()
            })
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(if enabled {
                Theme::text_primary()
            } else {
                Theme::text_muted()
            })
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(build_tooltip(tooltip))
            .child(icon)
    }

    pub(crate) fn header_button(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        primary: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let bg = if primary {
            Theme::accent()
        } else {
            Theme::bg_surface()
        };
        let hover_bg = if primary {
            rgb(Theme::ACCENT_HOVER)
        } else {
            Theme::bg_hover()
        };

        div()
            .id(id)
            .px_2p5()
            .py_1()
            .rounded_md()
            .bg(bg)
            .border_1()
            .border_color(if primary {
                Theme::accent()
            } else {
                Theme::border()
            })
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(Theme::text_primary())
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .child(label.into())
    }

    fn render_media_pool(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let assets = self.state.assets.clone();
        let selected_asset_id = self.state.selected_asset_id;
        let is_empty = assets.is_empty();

        div()
            .id("media-pool-dropzone")
            .w(px(280.))
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::bg_panel())
            .border_r_1()
            .border_color(Theme::border())
            .drag_over::<ExternalPaths>(|style, _, _, _| {
                style
                    .border_2()
                    .border_color(Theme::accent())
                    .bg(Theme::bg_hover())
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.state
                    .import_paths_with_probe(paths.paths().to_vec(), probe_duration_secs);
                cx.notify();
            }))
            .child(
                div()
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(Theme::border())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Theme::text_secondary())
                            .child("MEDIA POOL"),
                    )
                    .child(
                        div()
                            .id("media-add-btn")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(Theme::accent())
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(Theme::text_primary())
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(Theme::ACCENT_HOVER)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prompt_import_media(cx);
                            }))
                            .child("+ Import"),
                    ),
            )
            .child(
                div().p_2().child(
                    div()
                        .id("media-drop-box")
                        .p_3()
                        .rounded_md()
                        .bg(Theme::bg_surface())
                        .border_1()
                        .border_dashed()
                        .border_color(Theme::border())
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .cursor_pointer()
                        .hover(|s| s.border_color(Theme::accent()).bg(Theme::bg_hover()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.prompt_import_media(cx);
                        }))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Theme::text_primary())
                                .child("Drop files here or click to browse"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(Theme::text_muted())
                                .child("Video (MP4, MOV) • Image (PNG, JPG) • Audio (MP3, WAV)"),
                        ),
                ),
            )
            .child(if is_empty {
                div()
                    .flex_1()
                    .px_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::text_secondary())
                            .child("No media imported yet"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::text_muted())
                            .child("Drag from Explorer or click above"),
                    )
            } else {
                div()
                    .flex_1()
                    .px_2()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .overflow_hidden()
                    .children(assets.into_iter().map(|asset| {
                        let is_selected = selected_asset_id == Some(asset.id);
                        let asset_id = asset.id;
                        let badge_color = rgb(asset.kind.color_hex());

                        div()
                            .id(SharedString::from(format!("asset-{}", asset.id)))
                            .p_2()
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
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::bg_hover()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.selected_asset_id = Some(asset_id);
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded_sm()
                                            .bg(badge_color)
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(Theme::text_primary())
                                            .child(asset.kind.display_name()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .overflow_hidden()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(Theme::text_primary())
                                                    .child(asset.name),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(Theme::text_muted())
                                                    .child(format!(
                                                        "{} • {:.1}s",
                                                        asset.size_label, asset.duration_secs
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("asset-insert-{}", asset_id)))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(Theme::bg_panel())
                                    .border_1()
                                    .border_color(Theme::border())
                                    .text_xs()
                                    .text_color(Theme::text_primary())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(Theme::accent()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.add_asset_to_timeline(asset_id);
                                        this.sync_preview_after_seek(cx);
                                        cx.notify();
                                    }))
                                    .child("+"),
                            )
                    }))
            })
    }

    fn render_preview_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_video = self.state.active_video_clip().cloned();
        let active_count = self.state.active_clips_at_playhead().len();
        let timecode = format_timecode(self.state.playhead_secs);
        let total_timecode = format_timecode(self.state.total_duration_secs);
        let is_playing = self.state.is_playing;

        let win_w = f32::from(window.viewport_size().width);
        let win_h = f32::from(window.viewport_size().height);
        let avail_w = (win_w - 280.0 - 260.0 - 40.0).max(200.0);
        let avail_h = (win_h - 44.0 - 234.0 - 48.0 - 32.0).max(180.0);
        let target_ratio = self.state.aspect_ratio.ratio_f32();
        let (preview_w, preview_h) = if avail_w / avail_h > target_ratio {
            (avail_h * target_ratio, avail_h)
        } else {
            (avail_w, avail_w / target_ratio)
        };

        let preview_content = if let Some(clip) = active_video.clone() {
            if clip.track == TrackKind::Overlay {
                if let Some(path) = clip.source_path.clone() {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(img(path).size_full().object_fit(ObjectFit::Contain))
                } else {
                    self.render_empty_preview(
                        "No image path",
                        &timecode,
                        &total_timecode,
                        active_count,
                    )
                }
            } else if let Some(frame) = self.current_frame.clone() {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(img(frame).size_full().object_fit(ObjectFit::Contain))
            } else {
                self.render_empty_preview(
                    "Loading video frame...",
                    &timecode,
                    &total_timecode,
                    active_count,
                )
            }
        } else {
            self.render_empty_preview(
                "No video frame at playhead",
                &timecode,
                &total_timecode,
                active_count,
            )
        };

        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::bg_app())
            .child(
                div()
                    .flex_1()
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(preview_w))
                            .h(px(preview_h))
                            .rounded_lg()
                            .bg(Theme::bg_preview())
                            .border_1()
                            .border_color(Theme::border())
                            .overflow_hidden()
                            .relative()
                            .child(preview_content)
                            .when_some(active_video, |el, clip| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top_2()
                                        .left_2()
                                        .px_2()
                                        .py_0p5()
                                        .rounded_sm()
                                        .bg(Theme::bg_panel())
                                        .border_1()
                                        .border_color(Theme::border())
                                        .text_xs()
                                        .text_color(if is_playing {
                                            rgb(Theme::SUCCESS)
                                        } else {
                                            Theme::text_secondary()
                                        })
                                        .child(format!(
                                            "{} • {} • {}",
                                            if is_playing { "PLAYING" } else { "PAUSED" },
                                            clip.name,
                                            timecode
                                        )),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .h(px(48.))
                    .w_full()
                    .px_4()
                    .bg(Theme::bg_panel())
                    .border_t_1()
                    .border_color(Theme::border())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(Theme::text_secondary())
                            .child(format!("{} / {}", timecode, total_timecode)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                self.icon_button("tr-start", "⏮", "Jump to Start", false, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.seek_to(0.0);
                                        this.sync_preview_after_seek(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                self.icon_button("tr-back", "⏪", "Step Back 1s", false, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.step_by(-1.0);
                                        this.sync_preview_after_seek(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                self.icon_button(
                                    "tr-play",
                                    if is_playing { "⏸" } else { "▶" },
                                    if is_playing {
                                        "Pause (Space)"
                                    } else {
                                        "Play (Space)"
                                    },
                                    true,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.toggle_play();
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button("tr-fwd", "⏩", "Step Forward 1s", false, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.step_by(1.0);
                                        this.sync_preview_after_seek(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                self.icon_button("tr-end", "⏭", "Jump to End", false, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let end = this.state.total_duration_secs;
                                        this.state.seek_to(end);
                                        this.sync_preview_after_seek(cx);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(Theme::text_muted())
                            .child("Space: Play • Ctrl+B: Split • Ctrl+Z: Undo"),
                    ),
            )
    }

    fn render_empty_preview(
        &self,
        title: &'static str,
        timecode: &str,
        total_timecode: &str,
        active_count: usize,
    ) -> gpui::Div {
        let (out_w, out_h) = self.state.aspect_ratio.dimensions(self.state.base_height);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .px_2p5()
                    .py_1()
                    .rounded_sm()
                    .bg(Theme::bg_surface())
                    .text_xs()
                    .text_color(Theme::text_secondary())
                    .child(format!(
                        "PREVIEW {} • {}×{}",
                        self.state.aspect_ratio.label(),
                        out_w,
                        out_h
                    )),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(Theme::text_muted())
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(Theme::text_muted())
                    .child(format!(
                        "{}  /  {}  ({} active layers)",
                        timecode, total_timecode, active_count
                    )),
            )
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.selected_clip().cloned();

        div()
            .w(px(260.))
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::bg_panel())
            .border_l_1()
            .border_color(Theme::border())
            .child(
                div()
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(Theme::border())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Theme::text_secondary())
                            .child("INSPECTOR"),
                    ),
            )
            .child(if let Some(clip) = selected {
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .p_2p5()
                            .rounded_md()
                            .bg(Theme::bg_surface())
                            .border_1()
                            .border_color(Theme::border())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_primary())
                                    .child(clip.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::text_secondary())
                                    .child(format!(
                                        "Track {} • Start {:.1}s • Dur {:.1}s",
                                        clip.track.label(),
                                        clip.start_secs,
                                        clip.duration_secs
                                    )),
                            ),
                    )
                    .child(self.inspector_stepper(
                        "Opacity",
                        format!("{}%", clip.opacity),
                        "op-dec",
                        "op-inc",
                        cx.listener(|this, _, _, cx| {
                            this.state.push_undo();
                            if let Some(c) = this.state.selected_clip_mut() {
                                c.opacity = c.opacity.saturating_sub(10);
                            }
                            cx.notify();
                        }),
                        cx.listener(|this, _, _, cx| {
                            this.state.push_undo();
                            if let Some(c) = this.state.selected_clip_mut() {
                                c.opacity = (c.opacity + 10).min(100);
                            }
                            cx.notify();
                        }),
                    ))
                    .child(self.inspector_stepper(
                        "Scale",
                        format!("{}%", clip.scale),
                        "sc-dec",
                        "sc-inc",
                        cx.listener(|this, _, _, cx| {
                            this.state.push_undo();
                            if let Some(c) = this.state.selected_clip_mut() {
                                c.scale = c.scale.saturating_sub(10).max(10);
                            }
                            cx.notify();
                        }),
                        cx.listener(|this, _, _, cx| {
                            this.state.push_undo();
                            if let Some(c) = this.state.selected_clip_mut() {
                                c.scale = (c.scale + 10).min(200);
                            }
                            cx.notify();
                        }),
                    ))
                    .child(self.inspector_stepper(
                        "Volume",
                        format!("{}%", clip.volume),
                        "vol-dec",
                        "vol-inc",
                        cx.listener(|this, _, _, cx| {
                            this.state.push_undo();
                            if let Some(c) = this.state.selected_clip_mut() {
                                c.volume = c.volume.saturating_sub(10);
                            }
                            cx.notify();
                        }),
                        cx.listener(|this, _, _, cx| {
                            this.state.push_undo();
                            if let Some(c) = this.state.selected_clip_mut() {
                                c.volume = (c.volume + 10).min(100);
                            }
                            cx.notify();
                        }),
                    ))
            } else {
                div()
                    .p_4()
                    .text_xs()
                    .text_color(Theme::text_muted())
                    .child("Select a clip on the timeline to inspect, drag, or trim its edges.")
            })
    }

    fn inspector_stepper(
        &self,
        label: &'static str,
        value: String,
        dec_id: &'static str,
        inc_id: &'static str,
        on_dec: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
        on_inc: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .text_color(Theme::text_secondary())
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .id(dec_id)
                            .w(px(24.))
                            .h(px(24.))
                            .rounded_sm()
                            .bg(Theme::bg_surface())
                            .border_1()
                            .border_color(Theme::border())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(Theme::text_primary())
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::bg_hover()))
                            .on_click(on_dec)
                            .child("-"),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .text_xs()
                            .text_center()
                            .text_color(Theme::text_primary())
                            .child(value),
                    )
                    .child(
                        div()
                            .id(inc_id)
                            .w(px(24.))
                            .h(px(24.))
                            .rounded_sm()
                            .bg(Theme::bg_surface())
                            .border_1()
                            .border_color(Theme::border())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(Theme::text_primary())
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::bg_hover()))
                            .on_click(on_inc)
                            .child("+"),
                    ),
            )
    }

    fn track_flag_btn(
        &self,
        id: SharedString,
        label: &'static str,
        tooltip: &'static str,
        active: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .w(px(18.))
            .h(px(18.))
            .rounded_sm()
            .bg(if active {
                rgb(Theme::PLAYHEAD)
            } else {
                Theme::bg_surface()
            })
            .border_1()
            .border_color(Theme::border())
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(Theme::text_primary())
            .cursor_pointer()
            .hover(|s| s.bg(Theme::bg_hover()))
            .tooltip(build_tooltip(tooltip))
            .child(label)
    }

    fn render_timeline(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let px_per_sec = self.state.px_per_sec;
        let total_secs = self.state.total_duration_secs;
        let playhead_px = self.state.playhead_secs * px_per_sec;

        let win_width = f32::from(window.viewport_size().width);
        let viewport_w = (win_width - TRACK_HEADER_W).max(200.0);
        let timeline_width = (total_secs * px_per_sec).max(viewport_w);
        let max_scroll = (timeline_width - viewport_w).max(0.0);
        let scroll_x = self.timeline_scroll_x.clamp(0.0, max_scroll);

        let thumb_w = ((viewport_w / timeline_width) * viewport_w).clamp(36.0, viewport_w);
        let thumb_track_space = (viewport_w - thumb_w).max(0.0);
        let thumb_left = if max_scroll > 0.0 {
            (scroll_x / max_scroll) * thumb_track_space
        } else {
            0.0
        };

        let selected_clip_id = self.state.selected_clip_id;
        let snap_enabled = self.state.snap_enabled;
        let magnet_enabled = self.state.main_track_magnet;

        let ruler_step: usize = if px_per_sec >= 24.0 {
            2
        } else if px_per_sec >= 12.0 {
            5
        } else if px_per_sec >= 6.0 {
            10
        } else {
            20
        };
        let ruler_marks: Vec<u32> = (0..=(total_secs as u32)).step_by(ruler_step).collect();
        let tracks = [TrackKind::Overlay, TrackKind::Video, TrackKind::Audio];

        div()
            .id("timeline-root")
            .h(px(234.))
            .w_full()
            .flex()
            .flex_col()
            .bg(Theme::bg_panel())
            .border_t_1()
            .border_color(Theme::border())
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                let (dx, dy) = match event.delta {
                    ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                    ScrollDelta::Lines(p) => (p.x * 24.0, p.y * 24.0),
                };

                let vp_w = (f32::from(window.viewport_size().width) - TRACK_HEADER_W).max(200.0);

                if event.modifiers.control {
                    let old_pps = this.state.px_per_sec;
                    let factor = if dy > 0.0 { 1.15 } else { 0.87 };
                    this.state.adjust_zoom(factor);
                    let new_pps = this.state.px_per_sec;

                    let cursor_in_tracks =
                        (f32::from(event.position.x) - TRACK_HEADER_W).clamp(0.0, vp_w);
                    let time_at_cursor = (this.timeline_scroll_x + cursor_in_tracks) / old_pps;
                    let new_scroll = time_at_cursor * new_pps - cursor_in_tracks;
                    let new_max = (this.state.total_duration_secs * new_pps - vp_w).max(0.0);
                    this.timeline_scroll_x = new_scroll.clamp(0.0, new_max);
                } else {
                    let delta = if dx.abs() > dy.abs() { dx } else { dy };
                    let new_max =
                        (this.state.total_duration_secs * this.state.px_per_sec - vp_w).max(0.0);
                    this.timeline_scroll_x =
                        (this.timeline_scroll_x - delta * 2.0).clamp(0.0, new_max);
                }
                cx.notify();
            }))
            .child(
                div()
                    .h(px(34.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(Theme::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_secondary())
                                    .child("TIMELINE"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .child(format!(
                                        "{} clips • Ctrl+Scroll to Zoom",
                                        self.state.clips.len()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                self.icon_button(
                                    "tl-undo",
                                    "↶",
                                    "Undo (Ctrl+Z)",
                                    false,
                                    self.state.can_undo(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.state.undo() {
                                        this.sync_preview_after_seek(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-redo",
                                    "↷",
                                    "Redo (Ctrl+Y)",
                                    false,
                                    self.state.can_redo(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.state.redo() {
                                        this.sync_preview_after_seek(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(Theme::border()))
                            .child(
                                self.icon_button(
                                    "tl-split",
                                    "✂",
                                    "Split at Playhead (Ctrl+B)",
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.split_selected_or_at_playhead();
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-delete",
                                    "🗑",
                                    "Delete Clip (Del)",
                                    false,
                                    selected_clip_id.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.delete_selected_clip();
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-ripple",
                                    "⇇",
                                    "Ripple Delete (Shift+Del)",
                                    false,
                                    selected_clip_id.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.ripple_delete_selected();
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                })),
                            )
                            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(Theme::border()))
                            .child(
                                self.icon_button(
                                    "tl-snap",
                                    "⌖",
                                    if snap_enabled {
                                        "Snap to Edges: ON"
                                    } else {
                                        "Snap to Edges: OFF"
                                    },
                                    snap_enabled,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.snap_enabled = !this.state.snap_enabled;
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-magnet",
                                    "🧲",
                                    if magnet_enabled {
                                        "Main Track Magnet: ON"
                                    } else {
                                        "Main Track Magnet: OFF"
                                    },
                                    magnet_enabled,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.toggle_main_track_magnet();
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-marker",
                                    "⚑",
                                    "Add Marker at Playhead",
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let t = this.state.playhead_secs;
                                    this.state.add_marker(t, "Marker");
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button("tl-zoom-out", "−", "Zoom Out", false, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.adjust_zoom(0.8);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                self.icon_button("tl-zoom-in", "+", "Zoom In", false, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.adjust_zoom(1.25);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    // Track headers column with Mute / Hide / Lock
                    .child(
                        div()
                            .w(px(TRACK_HEADER_W))
                            .h_full()
                            .flex()
                            .flex_col()
                            .bg(Theme::bg_panel())
                            .border_r_1()
                            .border_color(Theme::border())
                            .child(
                                div()
                                    .h(px(26.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .border_b_1()
                                    .border_color(Theme::border())
                                    .child("Tracks"),
                            )
                            .children(tracks.iter().map(|track| {
                                let t = *track;
                                let flags = self.state.track_flags(t);

                                div()
                                    .h(px(48.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .border_b_1()
                                    .border_color(Theme::border())
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .child(
                                                div()
                                                    .w(px(4.))
                                                    .h(px(16.))
                                                    .rounded_sm()
                                                    .bg(rgb(t.color_hex())),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(Theme::text_secondary())
                                                    .child(t.label()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .when(t != TrackKind::Overlay, |el| {
                                                el.child(
                                                    self.track_flag_btn(
                                                        SharedString::from(format!(
                                                            "mute-{}",
                                                            t.label()
                                                        )),
                                                        "M",
                                                        if flags.muted {
                                                            "Unmute Track"
                                                        } else {
                                                            "Mute Track"
                                                        },
                                                        flags.muted,
                                                    )
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.state.toggle_mute(t);
                                                        this.sync_preview_after_seek(cx);
                                                        cx.notify();
                                                    })),
                                                )
                                            })
                                            .when(t != TrackKind::Audio, |el| {
                                                el.child(
                                                    self.track_flag_btn(
                                                        SharedString::from(format!(
                                                            "hide-{}",
                                                            t.label()
                                                        )),
                                                        "H",
                                                        if flags.hidden {
                                                            "Show Track"
                                                        } else {
                                                            "Hide Track"
                                                        },
                                                        flags.hidden,
                                                    )
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.state.toggle_hide(t);
                                                        this.sync_preview_after_seek(cx);
                                                        cx.notify();
                                                    })),
                                                )
                                            })
                                            .child(
                                                self.track_flag_btn(
                                                    SharedString::from(format!(
                                                        "lock-{}",
                                                        t.label()
                                                    )),
                                                    "L",
                                                    if flags.locked {
                                                        "Unlock Track"
                                                    } else {
                                                        "Lock Track"
                                                    },
                                                    flags.locked,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.state.toggle_lock(t);
                                                    cx.notify();
                                                })),
                                            ),
                                    )
                            })),
                    )
                    // Tracks & Ruler canvas
                    .child(
                        div()
                            .id("timeline-tracks-canvas")
                            .flex_1()
                            .h_full()
                            .relative()
                            .overflow_hidden()
                            .bg(Theme::bg_app())
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                    this.playhead_dragging = true;
                                    this.seek_from_window_x(
                                        f32::from(event.position.x),
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .child(
                                div()
                                    .w(px(timeline_width))
                                    .h_full()
                                    .flex()
                                    .flex_col()
                                    .relative()
                                    .left(px(-scroll_x))
                                    // Time ruler
                                    .child(
                                        div()
                                            .h(px(26.))
                                            .w_full()
                                            .relative()
                                            .bg(Theme::bg_surface())
                                            .border_b_1()
                                            .border_color(Theme::border())
                                            .children(ruler_marks.into_iter().map(|sec| {
                                                let left = sec as f32 * px_per_sec;
                                                div()
                                                    .id(SharedString::from(format!("ruler-{}", sec)))
                                                    .absolute()
                                                    .left(px(left))
                                                    .top_0()
                                                    .h_full()
                                                    .px_1()
                                                    .border_l_1()
                                                    .border_color(Theme::border())
                                                    .flex()
                                                    .items_center()
                                                    .text_xs()
                                                    .text_color(Theme::text_muted())
                                                    .child(format!("{}s", sec))
                                            })),
                                    )
                                    // Track lanes
                                    .children(tracks.iter().map(|track_kind| {
                                        let track_clips: Vec<_> = self
                                            .state
                                            .clips
                                            .iter()
                                            .filter(|c| c.track == *track_kind)
                                            .cloned()
                                            .collect();

                                        div()
                                            .h(px(48.))
                                            .w_full()
                                            .relative()
                                            .border_b_1()
                                            .border_color(Theme::border())
                                            .children(track_clips.into_iter().map(|clip| {
                                                let clip_id = clip.id;
                                                let is_sel = selected_clip_id == Some(clip_id);
                                                let left = clip.start_secs * px_per_sec;
                                                let width =
                                                    (clip.duration_secs * px_per_sec).max(24.0);
                                                let init_start = clip.start_secs;
                                                let init_dur = clip.duration_secs;

                                                div()
                                                    .id(SharedString::from(format!(
                                                        "clip-{}",
                                                        clip_id
                                                    )))
                                                    .absolute()
                                                    .left(px(left))
                                                    .top(px(6.))
                                                    .w(px(width))
                                                    .h(px(36.))
                                                    .rounded_md()
                                                    .bg(rgb(clip.track.color_hex()))
                                                    .border_2()
                                                    .border_color(if is_sel {
                                                        Theme::text_primary()
                                                    } else {
                                                        rgb(clip.track.color_hex())
                                                    })
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .overflow_hidden()
                                                    .cursor_pointer()
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(
                                                            move |this,
                                                                  event: &MouseDownEvent,
                                                                  _,
                                                                  cx| {
                                                                cx.stop_propagation();
                                                                this.state.selected_clip_id =
                                                                    Some(clip_id);
                                                                this.state.push_undo();
                                                                this.clip_drag =
                                                                    Some(ClipDragState {
                                                                        clip_id,
                                                                        kind: ClipDragKind::Move,
                                                                        start_mouse_x: f32::from(
                                                                            event.position.x,
                                                                        ),
                                                                        initial_start_secs:
                                                                            init_start,
                                                                        initial_duration_secs:
                                                                            init_dur,
                                                                    });
                                                                cx.notify();
                                                            },
                                                        ),
                                                    )
                                                    // Left trim handle
                                                    .child(
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "trim-l-{}",
                                                                clip_id
                                                            )))
                                                            .w(px(7.))
                                                            .h_full()
                                                            .bg(if is_sel {
                                                                Theme::text_primary()
                                                            } else {
                                                                rgb(clip.track.color_hex())
                                                            })
                                                            .cursor_ew_resize()
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    move |this,
                                                                          event: &MouseDownEvent,
                                                                          _,
                                                                          cx| {
                                                                        cx.stop_propagation();
                                                                        this.state.selected_clip_id =
                                                                            Some(clip_id);
                                                                        this.state.push_undo();
                                                                        this.clip_drag = Some(
                                                                            ClipDragState {
                                                                                clip_id,
                                                                                kind: ClipDragKind::TrimLeft,
                                                                                start_mouse_x: f32::from(
                                                                                    event.position.x,
                                                                                ),
                                                                                initial_start_secs: init_start,
                                                                                initial_duration_secs: init_dur,
                                                                            },
                                                                        );
                                                                        cx.notify();
                                                                    },
                                                                ),
                                                            ),
                                                    )
                                                    // Clip title
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .px_1()
                                                            .overflow_hidden()
                                                            .text_xs()
                                                            .font_weight(gpui::FontWeight::MEDIUM)
                                                            .text_color(Theme::text_primary())
                                                            .child(clip.name),
                                                    )
                                                    // Right trim handle
                                                    .child(
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "trim-r-{}",
                                                                clip_id
                                                            )))
                                                            .w(px(7.))
                                                            .h_full()
                                                            .bg(if is_sel {
                                                                Theme::text_primary()
                                                            } else {
                                                                rgb(clip.track.color_hex())
                                                            })
                                                            .cursor_ew_resize()
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    move |this,
                                                                          event: &MouseDownEvent,
                                                                          _,
                                                                          cx| {
                                                                        cx.stop_propagation();
                                                                        this.state.selected_clip_id =
                                                                            Some(clip_id);
                                                                        this.state.push_undo();
                                                                        this.clip_drag = Some(
                                                                            ClipDragState {
                                                                                clip_id,
                                                                                kind: ClipDragKind::TrimRight,
                                                                                start_mouse_x: f32::from(
                                                                                    event.position.x,
                                                                                ),
                                                                                initial_start_secs: init_start,
                                                                                initial_duration_secs: init_dur,
                                                                            },
                                                                        );
                                                                        cx.notify();
                                                                    },
                                                                ),
                                                            ),
                                                    )
                                            }))
                                    }))
                                    // Playhead line
                                    .child(
                                        div()
                                            .absolute()
                                            .left(px(playhead_px))
                                            .top_0()
                                            .w(px(2.))
                                            .h_full()
                                            .bg(rgb(Theme::PLAYHEAD)),
                                    ),
                            ),
                    ),
            )
            // Bottom Horizontal Scrollbar
            .child(
                div()
                    .h(px(16.))
                    .w_full()
                    .flex()
                    .bg(Theme::bg_panel())
                    .border_t_1()
                    .border_color(Theme::border())
                    .child(
                        div()
                            .w(px(TRACK_HEADER_W))
                            .h_full()
                            .border_r_1()
                            .border_color(Theme::border()),
                    )
                    .child(
                        div()
                            .id("timeline-scrollbar-track")
                            .flex_1()
                            .h_full()
                            .relative()
                            .bg(Theme::bg_app())
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                    if max_scroll <= 0.0 || thumb_track_space <= 0.0 {
                                        return;
                                    }
                                    let click_x =
                                        (f32::from(event.position.x) - TRACK_HEADER_W).max(0.0);
                                    let target_thumb_left =
                                        (click_x - thumb_w * 0.5).clamp(0.0, thumb_track_space);
                                    this.timeline_scroll_x =
                                        (target_thumb_left / thumb_track_space) * max_scroll;
                                    this.scrollbar_dragging = true;
                                    this.scrollbar_drag_start_x = f32::from(event.position.x);
                                    this.scrollbar_drag_start_scroll = this.timeline_scroll_x;
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .id("timeline-scrollbar-thumb")
                                    .absolute()
                                    .left(px(thumb_left))
                                    .top(px(3.))
                                    .w(px(thumb_w))
                                    .h(px(10.))
                                    .rounded_full()
                                    .bg(if self.scrollbar_dragging {
                                        Theme::accent()
                                    } else {
                                        Theme::bg_hover()
                                    })
                                    .hover(|s| s.bg(Theme::text_muted()))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                                            cx.stop_propagation();
                                            this.scrollbar_dragging = true;
                                            this.scrollbar_drag_start_x = f32::from(event.position.x);
                                            this.scrollbar_drag_start_scroll =
                                                this.timeline_scroll_x;
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    ),
            )
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.screen == AppScreen::ProjectHub {
            return self.render_project_hub(cx).into_any_element();
        }

        div()
            .id("editor-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::bg_app())
            .text_color(Theme::text_primary())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.handle_key_down(event, cx);
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if let Some(drag) = this.clip_drag {
                    let dx = f32::from(event.position.x) - drag.start_mouse_x;
                    let dt = dx / this.state.px_per_sec;
                    match drag.kind {
                        ClipDragKind::Move => {
                            let snap_threshold = 10.0 / this.state.px_per_sec;
                            this.state.move_clip_with_snap(
                                drag.clip_id,
                                drag.initial_start_secs + dt,
                                snap_threshold,
                            );
                        }
                        ClipDragKind::TrimLeft => {
                            this.state
                                .trim_clip_left(drag.clip_id, drag.initial_start_secs + dt);
                        }
                        ClipDragKind::TrimRight => {
                            let new_end =
                                drag.initial_start_secs + drag.initial_duration_secs + dt;
                            this.state.trim_clip_right(drag.clip_id, new_end);
                        }
                    }
                    cx.notify();
                    return;
                }

                if this.playhead_dragging {
                    this.seek_from_window_x(f32::from(event.position.x), window, cx);
                    return;
                }
                if !this.scrollbar_dragging {
                    return;
                }
                let vp_w = (f32::from(window.viewport_size().width) - TRACK_HEADER_W).max(200.0);
                let timeline_w =
                    (this.state.total_duration_secs * this.state.px_per_sec).max(vp_w);
                let max_scroll = (timeline_w - vp_w).max(0.0);
                let thumb_w = ((vp_w / timeline_w) * vp_w).clamp(36.0, vp_w);
                let track_space = (vp_w - thumb_w).max(1.0);

                let dx = f32::from(event.position.x) - this.scrollbar_drag_start_x;
                let scroll_delta = (dx / track_space) * max_scroll;
                this.timeline_scroll_x =
                    (this.scrollbar_drag_start_scroll + scroll_delta).clamp(0.0, max_scroll);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    let mut changed = false;
                    if this.clip_drag.is_some() {
                        this.clip_drag = None;
                        this.sync_preview_after_seek(cx);
                        changed = true;
                    }
                    if this.scrollbar_dragging {
                        this.scrollbar_dragging = false;
                        changed = true;
                    }
                    if this.playhead_dragging {
                        this.playhead_dragging = false;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                }),
            )
            .child(self.render_header(cx))
            .children(self.render_export_banner(cx))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .overflow_hidden()
                    .child(self.render_media_pool(cx))
                    .child(self.render_preview_panel(window, cx))
                    .child(self.render_inspector(cx)),
            )
            .child(self.render_timeline(window, cx))
            .into_any_element()
    }
}
