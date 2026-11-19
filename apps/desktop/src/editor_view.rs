use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    AnyView, AppContext, Bounds, Context, ElementInputHandler, EntityInputHandler, ExternalPaths,
    FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ObjectFit, ParentElement, PathBuilder, PathPromptOptions, Pixels, Point,
    Render, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, UTF16Selection, Window, canvas, div, img, point, prelude::FluentBuilder,
    px, rgb,
};
use opencut_core::{
    AspectRatio, ClipCrop, EditorCore, RecentProjectEntry, TextAnimation, TextPreset, TrackKind,
    format_timecode, load_recent_projects, record_recent_project,
};

use crate::media_engine::{
    AudioStreamPlayer, ExportFormat, ExportHandle, ExportOptions, VideoStreamPlayer,
    decode_single_frame, detect_gpu_encoder, export_freeze_frame, probe_duration_secs,
    probe_media_dimensions, render_rotated_text_frame, reveal_in_explorer, start_export_timeline,
};
use crate::theme::Theme;

const TRACK_HEADER_W: f32 = 152.0;

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

#[derive(Clone, Copy)]
enum PreviewTransformKind {
    Move,
    ResizeCorner,
    Rotate,
}

#[derive(Clone, Copy)]
struct PreviewTextDragState {
    clip_id: usize,
    kind: PreviewTransformKind,
    start_mouse_x: f32,
    start_mouse_y: f32,
    initial_pos_x: f32,
    initial_pos_y: f32,
    initial_scale: u16,
    initial_font_size: f32,
    initial_rot: f32,
    center_x: f32,
    center_y: f32,
    preview_scale: f32,
    moved: bool,
}

#[derive(Clone, Copy, Default)]
struct PreviewSnapGuides {
    center_x: bool,
    center_y: bool,
    left_edge: bool,
    right_edge: bool,
    top_edge: bool,
    bottom_edge: bool,
    safe_left: bool,
    safe_right: bool,
    safe_top: bool,
    safe_bottom: bool,
}

fn snap_rotation_angle(deg: f32, shift: bool) -> (f32, Option<f32>) {
    let norm = deg.rem_euclid(360.0);
    if shift {
        let s = ((norm / 15.0).round() * 15.0).rem_euclid(360.0);
        return (s, Some(s));
    }
    for target in [0.0_f32, 90.0, 180.0, 270.0] {
        let diff = ((norm - target + 180.0).rem_euclid(360.0) - 180.0).abs();
        if diff <= 4.0 {
            return (target, Some(target));
        }
    }
    for target in [45.0_f32, 135.0, 225.0, 315.0] {
        let diff = ((norm - target + 180.0).rem_euclid(360.0) - 180.0).abs();
        if diff <= 2.5 {
            return (target, Some(target));
        }
    }
    (norm, None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewQuality {
    Full,
    Half,
    Quarter,
}

impl PreviewQuality {
    fn height(&self) -> u32 {
        match self {
            PreviewQuality::Full => 720,
            PreviewQuality::Half => 360,
            PreviewQuality::Quarter => 180,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            PreviewQuality::Full => "Full",
            PreviewQuality::Half => "1/2",
            PreviewQuality::Quarter => "1/4",
        }
    }

    fn next(&self) -> Self {
        match self {
            PreviewQuality::Full => PreviewQuality::Half,
            PreviewQuality::Half => PreviewQuality::Quarter,
            PreviewQuality::Quarter => PreviewQuality::Full,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SliderField {
    Opacity,
    Scale,
    Rotation,
    Speed,
    Volume,
    FontSize,
    FontWeight,
    ColorR,
    ColorG,
    ColorB,
    StrokeWidth,
    ShadowOffset,
    BgPadding,
    AnimDuration,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    FadeIn,
    FadeOut,
}

const INSPECTOR_W: f32 = 280.0;
const SLIDER_W: f32 = 136.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppScreen {
    ProjectHub,
    Editor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaPanelTab {
    Media,
    Text,
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
    media_panel_tab: MediaPanelTab,
    menu_open: bool,
    media_filter: Option<TrackKind>,
    media_search_query: String,
    media_search_focused: bool,
    editing_text_clip_id: Option<usize>,
    marked_text_range: Option<Range<usize>>,
    preview_quality: PreviewQuality,
    preview_zoom: f32,
    show_safe_zone: bool,
    preview_fullscreen: bool,
    playback_started: bool,
    current_frame: Option<Arc<RenderImage>>,
    player: VideoStreamPlayer,
    audio_player: AudioStreamPlayer,
    decode_seq: u64,
    decode_applied_seq: u64,
    decode_in_flight: bool,
    decode_dirty: bool,
    timeline_scroll_x: f32,
    scrollbar_dragging: bool,
    scrollbar_drag_start_x: f32,
    scrollbar_drag_start_scroll: f32,
    playhead_dragging: bool,
    clip_drag: Option<ClipDragState>,
    preview_text_drag: Option<PreviewTextDragState>,
    slider_drag: Option<SliderField>,
    media_dim_cache: HashMap<PathBuf, (u32, u32)>,
    last_rotate_sync: Option<Instant>,
    active_snap_guides: PreviewSnapGuides,
    rotated_text_cache: HashMap<usize, (u64, Arc<RenderImage>, u32, u32)>,
    rotated_text_seq: HashMap<usize, u64>,
    rotated_text_applied_seq: HashMap<usize, u64>,
    rot_text_in_flight: HashMap<usize, bool>,
    rot_text_dirty: HashMap<usize, bool>,
    export_status: ExportStatus,
    export_modal_open: bool,
    export_options: ExportOptions,
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
            media_panel_tab: MediaPanelTab::Media,
            menu_open: false,
            media_filter: None,
            media_search_query: String::new(),
            media_search_focused: false,
            editing_text_clip_id: None,
            marked_text_range: None,
            preview_quality: PreviewQuality::Half,
            preview_zoom: 1.0,
            show_safe_zone: false,
            preview_fullscreen: false,
            playback_started: false,
            current_frame: None,
            player: VideoStreamPlayer::new(),
            audio_player: AudioStreamPlayer::new(),
            decode_seq: 0,
            decode_applied_seq: 0,
            decode_in_flight: false,
            decode_dirty: false,
            timeline_scroll_x: 0.0,
            scrollbar_dragging: false,
            scrollbar_drag_start_x: 0.0,
            scrollbar_drag_start_scroll: 0.0,
            playhead_dragging: false,
            clip_drag: None,
            preview_text_drag: None,
            slider_drag: None,
            media_dim_cache: HashMap::new(),
            last_rotate_sync: None,
            active_snap_guides: PreviewSnapGuides::default(),
            rotated_text_cache: HashMap::new(),
            rotated_text_seq: HashMap::new(),
            rotated_text_applied_seq: HashMap::new(),
            rot_text_in_flight: HashMap::new(),
            rot_text_dirty: HashMap::new(),
            export_status: ExportStatus::Idle,
            export_modal_open: false,
            export_options: ExportOptions::default(),
        };
        view.ensure_playback_loop(cx);
        view
    }

    fn clip_canvas_half_extents(&self, clip_id: usize) -> (f32, f32) {
        let (out_w, out_h) = self.state.aspect_ratio.dimensions(self.state.base_height);
        let cw = out_w as f32;
        let ch = out_h as f32;
        let Some(c) = self.state.clips.iter().find(|c| c.id == clip_id) else {
            return (cw * 0.5, ch * 0.5);
        };
        if c.track == TrackKind::Text {
            let td = c.text.as_ref();
            let font_sz = td.map_or(48.0, |t| t.font_size);
            let char_count = td.map_or(4.0, |t| t.content.chars().count().max(2) as f32);
            let sc = c.scale as f32 / 100.0;
            let est_w = (char_count * font_sz * 0.56 * sc).clamp(40.0, cw);
            let est_h = (font_sz * 1.35 * sc).clamp(24.0, ch);
            return (est_w * 0.5, est_h * 0.5);
        }
        let sc = (c.scale as f32 / 100.0).clamp(0.1, 2.0);
        let (fit_w, fit_h) = self.fit_media_box(c.source_path.as_ref(), cw, ch);
        (fit_w * sc * 0.5, fit_h * sc * 0.5)
    }

    fn apply_slider_ratio(&mut self, field: SliderField, ratio: f32) {
        let r = ratio.clamp(0.0, 1.0);
        let playhead = self.state.playhead_secs;
        let Some(c) = self.state.selected_clip_mut() else {
            return;
        };
        let is_kf_field = matches!(
            field,
            SliderField::Opacity | SliderField::Scale | SliderField::Rotation
        );
        if is_kf_field && !c.keyframes.is_empty() {
            let (epx, epy, esc, erot, eop) = c.evaluate_at(playhead);
            c.pos_x = epx;
            c.pos_y = epy;
            c.scale = esc;
            c.rotation_deg = erot;
            c.opacity = eop;
        }
        match field {
            SliderField::Opacity => {
                c.opacity = (r * 100.0).round() as u8;
                c.sync_keyframe_if_animated(playhead);
            }
            SliderField::Scale => {
                c.scale = (10.0 + r * 190.0).round() as u16;
                c.sync_keyframe_if_animated(playhead);
            }
            SliderField::Rotation => {
                let (snapped, _) = snap_rotation_angle((r * 360.0).round(), false);
                c.rotation_deg = snapped;
                c.sync_keyframe_if_animated(playhead);
            }
            SliderField::Speed => {
                let spd = ((0.25 + r * 3.75) * 4.0).round() / 4.0;
                self.state.set_selected_speed(spd);
            }
            SliderField::Volume => c.volume = (r * 100.0).round() as u8,
            SliderField::FontSize => {
                if let Some(ref mut t) = c.text {
                    t.font_size = (12.0 + r * 128.0).round();
                }
            }
            SliderField::FontWeight => {
                if let Some(ref mut t) = c.text {
                    let weights = [300_u16, 400, 500, 700, 900];
                    let idx = ((r * (weights.len() - 1) as f32).round() as usize)
                        .min(weights.len() - 1);
                    t.font_weight = weights[idx];
                }
            }
            SliderField::ColorR => {
                if let Some(ref mut t) = c.text {
                    let red = ((r * 255.0).round() as u32) & 0xff;
                    t.color_hex = (t.color_hex & 0x00ffff) | (red << 16);
                }
            }
            SliderField::ColorG => {
                if let Some(ref mut t) = c.text {
                    let green = ((r * 255.0).round() as u32) & 0xff;
                    t.color_hex = (t.color_hex & 0xff00ff) | (green << 8);
                }
            }
            SliderField::ColorB => {
                if let Some(ref mut t) = c.text {
                    let blue = ((r * 255.0).round() as u32) & 0xff;
                    t.color_hex = (t.color_hex & 0xffff00) | blue;
                }
            }
            SliderField::StrokeWidth => {
                if let Some(ref mut t) = c.text {
                    t.stroke_width = (r * 8.0 * 2.0).round() / 2.0;
                    t.stroke_color_hex = if t.stroke_width > 0.0 {
                        Some(t.stroke_color_hex.unwrap_or(0x000000))
                    } else {
                        None
                    };
                }
            }
            SliderField::ShadowOffset => {
                if let Some(ref mut t) = c.text {
                    let off = (r * 12.0).round();
                    t.shadow_offset_x = off;
                    t.shadow_offset_y = off;
                    t.shadow_color_hex = if off > 0.0 {
                        Some(t.shadow_color_hex.unwrap_or(0x000000))
                    } else {
                        None
                    };
                }
            }
            SliderField::BgPadding => {
                if let Some(ref mut t) = c.text {
                    t.bg_padding = (2.0 + r * 22.0).round();
                }
            }
            SliderField::AnimDuration => {
                if let Some(ref mut t) = c.text {
                    t.anim_duration_secs = ((0.1 + r * 1.9) * 10.0).round() / 10.0;
                }
            }
            SliderField::CropLeft => c.crop.left = ((r * 0.45) * 100.0).round() / 100.0,
            SliderField::CropRight => c.crop.right = ((r * 0.45) * 100.0).round() / 100.0,
            SliderField::CropTop => c.crop.top = ((r * 0.45) * 100.0).round() / 100.0,
            SliderField::CropBottom => c.crop.bottom = ((r * 0.45) * 100.0).round() / 100.0,
            SliderField::FadeIn => {
                let max_fade = (c.duration_secs * 0.5).clamp(0.1, 5.0);
                c.fade_in_secs = ((r * max_fade) * 10.0).round() / 10.0;
            }
            SliderField::FadeOut => {
                let max_fade = (c.duration_secs * 0.5).clamp(0.1, 5.0);
                c.fade_out_secs = ((r * max_fade) * 10.0).round() / 10.0;
            }
        }
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

        let active_video = self
            .state
            .active_clips_at_playhead()
            .into_iter()
            .find(|c| c.track == TrackKind::Video)
            .cloned();

        let Some(active) = active_video else {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        };

        let (_, _, _, rot, _) = active.evaluate_at(self.state.playhead_secs);
        let Some(path) = active.source_path else {
            return;
        };

        let ph = self.preview_quality.height();
        let (cur_flip, cur_rot, cur_crop, cur_rev) = self.player.active_transform();
        if self.player.active_clip_id() != Some(active.id)
            || self.player.active_ratio() != Some(self.state.aspect_ratio)
            || self.player.active_height() != Some(ph)
            || cur_flip != active.flip_h
            || (cur_rot - rot).abs() > 1.0
            || cur_crop != active.crop
            || cur_rev != active.reversed
        {
            let dt = (self.state.playhead_secs - active.start_secs).max(0.0);
            let spd = active.speed.clamp(0.25, 4.0);
            let offset = active.source_in_secs + dt * spd;
            let rev_src_dur = ((active.duration_secs - dt) * spd).max(0.1);
            self.player.start(
                active.id,
                path,
                offset,
                self.state.aspect_ratio,
                ph,
                active.flip_h,
                rot,
                active.crop,
                active.reversed,
                active.source_in_secs,
                rev_src_dur,
            );
        }

        if let Some(frame) = self.player.take_latest_frame() {
            self.set_current_frame(Some(frame), cx);
        }
    }

    fn sync_preview_after_seek(&mut self, cx: &mut Context<Self>) {
        self.sync_rotated_texts(cx);
        self.audio_player.stop();
        if self.state.is_playing {
            let audio_clips = self.state.active_audio_clips_at(self.state.playhead_secs);
            self.audio_player
                .sync_playing_clips(&audio_clips, self.state.playhead_secs);
        }

        let active_video = self
            .state
            .active_clips_at_playhead()
            .into_iter()
            .find(|c| c.track == TrackKind::Video)
            .cloned();

        let Some(active) = active_video else {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        };

        let (_, _, _, rot, _) = active.evaluate_at(self.state.playhead_secs);
        let Some(path) = active.source_path else {
            self.player.stop();
            self.set_current_frame(None, cx);
            return;
        };

        let dt = (self.state.playhead_secs - active.start_secs).clamp(0.0, active.duration_secs);
        let spd = active.speed.clamp(0.25, 4.0);
        let offset = if active.reversed {
            active.source_in_secs + ((active.duration_secs - dt) * spd).max(0.0)
        } else {
            active.source_in_secs + dt * spd
        };
        let rev_src_dur = ((active.duration_secs - dt) * spd).max(0.1);
        let ph = self.preview_quality.height();
        let paths_to_probe: Vec<PathBuf> = self
            .state
            .active_clips_at_playhead()
            .into_iter()
            .filter_map(|c| c.source_path.clone())
            .filter(|p| !self.media_dim_cache.contains_key(p))
            .collect();
        for p in paths_to_probe {
            self.ensure_media_dims_cached(p, cx);
        }

        if self.state.is_playing {
            self.player.start(
                active.id,
                path,
                offset,
                self.state.aspect_ratio,
                ph,
                active.flip_h,
                rot,
                active.crop,
                active.reversed,
                active.source_in_secs,
                rev_src_dur,
            );
        } else {
            self.player.stop();
            self.request_single_frame(path, offset, active.flip_h, rot, active.crop, cx);
        }
    }

    fn sync_rotated_texts(&mut self, cx: &mut Context<Self>) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let texts: Vec<_> = self
            .state
            .active_clips_at_playhead()
            .into_iter()
            .filter(|c| c.track == TrackKind::Text && c.text.is_some())
            .cloned()
            .collect();

        for tclip in texts {
            let Some(td) = tclip.text.clone() else {
                continue;
            };
            let (_, _, esc, erot, _) = tclip.evaluate_at(self.state.playhead_secs);
            let norm_rot = erot.rem_euclid(360.0);
            let needs_rot = tclip.flip_h || (norm_rot > 0.5 && norm_rot < 359.5);
            if !needs_rot {
                self.rotated_text_cache.remove(&tclip.id);
                continue;
            }
            let clip_scale = (esc as f32 / 100.0).clamp(0.1, 2.0);
            let font_sz = (td.font_size * clip_scale).clamp(8.0, 220.0);
            let mut max_cols = 1usize;
            let mut lines = 0usize;
            for line in td.content.lines() {
                lines += 1;
                max_cols = max_cols.max(line.chars().count());
            }
            let lines = lines.max(1) as f32;
            let pad = td.bg_padding.clamp(2.0, 24.0);
            let box_w = ((max_cols as f32 * font_sz * 0.56) + pad * 2.0 + 10.0)
                .ceil()
                .max(40.0) as u32;
            let box_h = ((lines * font_sz * 1.30) + pad * 1.2 + 8.0)
                .ceil()
                .max(26.0) as u32;

            let mut hasher = DefaultHasher::new();
            td.content.hash(&mut hasher);
            td.font_family.hash(&mut hasher);
            td.font_weight.hash(&mut hasher);
            (font_sz.round() as i32).hash(&mut hasher);
            td.color_hex.hash(&mut hasher);
            td.bg_color_hex.hash(&mut hasher);
            td.stroke_color_hex.hash(&mut hasher);
            ((td.stroke_width * 10.0).round() as i32).hash(&mut hasher);
            td.shadow_color_hex.hash(&mut hasher);
            tclip.flip_h.hash(&mut hasher);
            ((norm_rot * 2.0).round() as i32).hash(&mut hasher);
            let sig = hasher.finish();

            if self
                .rotated_text_cache
                .get(&tclip.id)
                .is_some_and(|(s, _, _, _)| *s == sig)
            {
                continue;
            }

            if self.rot_text_in_flight.get(&tclip.id).copied().unwrap_or(false) {
                self.rot_text_dirty.insert(tclip.id, true);
                continue;
            }
            self.rot_text_in_flight.insert(tclip.id, true);
            self.rot_text_dirty.insert(tclip.id, false);

            let seq = self
                .rotated_text_seq
                .entry(tclip.id)
                .and_modify(|v| *v = v.wrapping_add(1))
                .or_insert(1);
            let seq_val = *seq;
            let cid = tclip.id;
            let flip_h = tclip.flip_h;

            let task = cx.background_executor().spawn(async move {
                render_rotated_text_frame(&td, font_sz, box_w, box_h, flip_h, norm_rot)
            });

            cx.spawn(async move |this, cx| {
                let res = task.await;
                let _ = this.update(cx, |view, cx| {
                    view.rot_text_in_flight.insert(cid, false);
                    if let Some((r_img, rw, rh)) = res {
                        let applied = view.rotated_text_applied_seq.get(&cid).copied().unwrap_or(0);
                        if seq_val > applied {
                            view.rotated_text_applied_seq.insert(cid, seq_val);
                            view.rotated_text_cache.insert(cid, (sig, r_img, rw, rh));
                            cx.notify();
                        }
                    }
                    if view.rot_text_dirty.get(&cid).copied().unwrap_or(false) {
                        view.rot_text_dirty.insert(cid, false);
                        view.sync_rotated_texts(cx);
                    }
                });
            })
            .detach();
        }
    }

    fn ensure_media_dims_cached(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.media_dim_cache.contains_key(&path) {
            return;
        }
        if let Ok((w, h)) = image::image_dimensions(&path) {
            if w > 0 && h > 0 {
                self.media_dim_cache.insert(path, (w, h));
                return;
            }
        }
        let path_clone = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { probe_media_dimensions(&path_clone) });
        cx.spawn(async move |this, cx| {
            if let Some((w, h)) = task.await {
                let _ = this.update(cx, |view, cx| {
                    view.media_dim_cache.insert(path, (w, h));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn fit_media_box(&self, path: Option<&PathBuf>, box_w: f32, box_h: f32) -> (f32, f32) {
        if let Some(p) = path {
            if let Some(&(sw, sh)) = self.media_dim_cache.get(p) {
                if sw > 0 && sh > 0 {
                    let src_ratio = sw as f32 / sh as f32;
                    let box_ratio = box_w / box_h.max(1.0);
                    return if src_ratio > box_ratio {
                        (box_w, box_w / src_ratio)
                    } else {
                        (box_h * src_ratio, box_h)
                    };
                }
            }
        }
        (box_w, box_h)
    }

    fn request_single_frame(
        &mut self,
        path: PathBuf,
        offset_secs: f32,
        flip_h: bool,
        rotation_deg: f32,
        crop: ClipCrop,
        cx: &mut Context<Self>,
    ) {
        if self.decode_in_flight {
            self.decode_dirty = true;
            return;
        }
        self.decode_in_flight = true;
        self.decode_dirty = false;
        self.decode_seq = self.decode_seq.wrapping_add(1);
        let seq = self.decode_seq;
        let ratio = self.state.aspect_ratio;
        let ph = self.preview_quality.height();

        let task = cx.background_executor().spawn(async move {
            decode_single_frame(&path, offset_secs, ratio, ph, flip_h, rotation_deg, crop)
        });

        cx.spawn(async move |this, cx| {
            let frame = task.await;
            let _ = this.update(cx, |view, cx| {
                view.decode_in_flight = false;
                if seq > view.decode_applied_seq && !view.state.is_playing {
                    view.decode_applied_seq = seq;
                    view.set_current_frame(frame, cx);
                    cx.notify();
                }
                if view.decode_dirty && !view.state.is_playing {
                    view.decode_dirty = false;
                    view.sync_preview_after_seek(cx);
                }
            });
        })
        .detach();
    }

    fn prompt_import_font(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("Import Font (.ttf, .otf)")),
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    if let Ok(bytes) = std::fs::read(&path) {
                        let _ = this.update(cx, |view, cx| {
                            let _ = cx
                                .text_system()
                                .add_fonts(vec![std::borrow::Cow::Owned(bytes)]);
                            let family = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Custom Font")
                                .replace(['-', '_'], " ");
                            view.state.push_undo();
                            if let Some(c) = view.state.selected_clip_mut() {
                                if let Some(ref mut t) = c.text {
                                    t.font_family = family.clone();
                                    t.font_path = Some(path.clone());
                                }
                            }
                            view.state.status_message = format!("Loaded font {}", family);
                            cx.notify();
                        });
                    }
                }
            }
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

        if let Some(edit_id) = self.editing_text_clip_id {
            if self.state.selected_clip_id == Some(edit_id) {
                match key {
                    "escape" | "enter" => {
                        self.editing_text_clip_id = None;
                        self.marked_text_range = None;
                        cx.notify();
                        return;
                    }
                    "backspace" => {
                        if let Some(c) = self.state.clips.iter_mut().find(|c| c.id == edit_id) {
                            if let Some(ref mut td) = c.text {
                                td.content.pop();
                                c.name = if td.content.is_empty() {
                                    "Text".into()
                                } else {
                                    td.content.clone()
                                };
                            }
                        }
                        cx.notify();
                        return;
                    }
                    _ if !ctrl => {
                        // Printable characters and Vietnamese IME input are handled by EntityInputHandler
                        return;
                    }
                    _ => {}
                }
            } else {
                self.editing_text_clip_id = None;
                self.marked_text_range = None;
            }
        }

        if self.media_search_focused {
            match key {
                "escape" | "enter" => {
                    self.media_search_focused = false;
                    cx.notify();
                    return;
                }
                "backspace" => {
                    self.media_search_query.pop();
                    cx.notify();
                    return;
                }
                _ if !ctrl => {
                    if let Some(ref kc) = event.keystroke.key_char {
                        self.media_search_query.push_str(kc);
                        cx.notify();
                    } else if key.len() == 1 {
                        self.media_search_query.push_str(key);
                        cx.notify();
                    } else if key == "space" {
                        self.media_search_query.push(' ');
                        cx.notify();
                    }
                    return;
                }
                _ => {}
            }
        }

        match (ctrl, shift, key) {
            (false, false, "space") => {
                self.state.toggle_play();
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (true, false, "c") => {
                self.state.copy_selected_clip();
                cx.notify();
            }
            (true, false, "v") => {
                if self.state.paste_clip_at_playhead().is_some() {
                    self.sync_preview_after_seek(cx);
                }
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
            (false, false, ",") => {
                self.state.step_frames(-1);
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (false, false, ".") => {
                self.state.step_frames(1);
                self.sync_preview_after_seek(cx);
                cx.notify();
            }
            (false, false, "escape") if self.preview_fullscreen => {
                self.preview_fullscreen = false;
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

        let ext = self.export_options.format.extension();
        let default_name = format!("opencut_export.{}", ext);
        let rx = cx.prompt_for_new_path(&default_dir, Some(&default_name));

        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = rx.await {
                if path.extension().is_none() {
                    path.set_extension(ext);
                }

                let _ = this.update(cx, |view, cx| {
                    let clips = view.state.clips.clone();
                    let tracks = view.state.tracks.clone();
                    let ratio = view.state.aspect_ratio;
                    let base_height = view.state.base_height;
                    let options = view.export_options;
                    let handle = start_export_timeline(
                        clips,
                        tracks,
                        ratio,
                        base_height,
                        options,
                        path.clone(),
                    );
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

    fn render_export_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let opts = self.export_options;
        let audio_only = opts.format.is_audio_only();

        div()
            .id("export-modal-backdrop")
            .absolute()
            .inset_0()
            .bg(gpui::rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, _| {}))
            .child(
                div()
                    .w(px(420.))
                    .p_5()
                    .rounded_lg()
                    .bg(Theme::bg_panel())
                    .border_1()
                    .border_color(Theme::border())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_3p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(Theme::text_primary())
                                    .child("Export Settings"),
                            )
                            .child(
                                div()
                                    .id("exp-close")
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(Theme::bg_hover()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.export_modal_open = false;
                                        cx.notify();
                                    }))
                                    .child("✕"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(Theme::text_secondary())
                                    .child("Format"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1p5()
                                    .children(ExportFormat::all().iter().copied().map(|fmt| {
                                        let active = opts.format == fmt;
                                        div()
                                            .id(SharedString::from(format!("exp-fmt-{:?}", fmt)))
                                            .px_2p5()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(if active { Theme::accent() } else { Theme::bg_surface() })
                                            .border_1()
                                            .border_color(if active { Theme::accent() } else { Theme::border() })
                                            .text_xs()
                                            .text_color(Theme::text_primary())
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.export_options.format = fmt;
                                                cx.notify();
                                            }))
                                            .child(fmt.label())
                                    })),
                            ),
                    )
                    .when(!audio_only, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(Theme::text_secondary())
                                        .child("Resolution"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_1p5()
                                        .children(
                                            [
                                                (480_u32, "480p"),
                                                (720, "720p"),
                                                (1080, "1080p"),
                                                (1440, "2K"),
                                                (2160, "4K"),
                                            ]
                                            .into_iter()
                                            .map(|(h, lbl)| {
                                                let active = opts.height == h;
                                                div()
                                                    .id(SharedString::from(format!("exp-res-{}", h)))
                                                    .px_2p5()
                                                    .py_1()
                                                    .rounded_sm()
                                                    .bg(if active { Theme::accent() } else { Theme::bg_surface() })
                                                    .border_1()
                                                    .border_color(if active { Theme::accent() } else { Theme::border() })
                                                    .text_xs()
                                                    .text_color(Theme::text_primary())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.export_options.height = h;
                                                        cx.notify();
                                                    }))
                                                    .child(lbl)
                                            }),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(Theme::text_secondary())
                                        .child("Frame Rate (FPS)"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_1p5()
                                        .children([24_u32, 25, 30, 50, 60].into_iter().map(|fps| {
                                            let active = opts.fps == fps;
                                            div()
                                                .id(SharedString::from(format!("exp-fps-{}", fps)))
                                                .px_2p5()
                                                .py_1()
                                                .rounded_sm()
                                                .bg(if active { Theme::accent() } else { Theme::bg_surface() })
                                                .border_1()
                                                .border_color(if active { Theme::accent() } else { Theme::border() })
                                                .text_xs()
                                                .text_color(Theme::text_primary())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.export_options.fps = fps;
                                                    cx.notify();
                                                }))
                                                .child(format!("{} fps", fps))
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(Theme::text_secondary())
                                        .child("Quality"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_1p5()
                                        .children(
                                            [
                                                (18_u8, "High (CRF 18)"),
                                                (22, "Standard (CRF 22)"),
                                                (26, "Compact (CRF 26)"),
                                            ]
                                            .into_iter()
                                            .map(|(crf, lbl)| {
                                                let active = opts.crf == crf;
                                                div()
                                                    .id(SharedString::from(format!("exp-crf-{}", crf)))
                                                    .px_2p5()
                                                    .py_1()
                                                    .rounded_sm()
                                                    .bg(if active { Theme::accent() } else { Theme::bg_surface() })
                                                    .border_1()
                                                    .border_color(if active { Theme::accent() } else { Theme::border() })
                                                    .text_xs()
                                                    .text_color(Theme::text_primary())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.export_options.crf = crf;
                                                        cx.notify();
                                                    }))
                                                    .child(lbl)
                                            }),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(Theme::text_secondary())
                                        .child("Hardware Acceleration"),
                                )
                                .child({
                                    let is_hevc = opts.format == ExportFormat::Mp4Hevc;
                                    let detected = detect_gpu_encoder(is_hevc);
                                    let gpu_label = match detected {
                                        Some(enc) => format!("GPU ({})", enc),
                                        None => "GPU (Auto / Fallback CPU)".to_string(),
                                    };
                                    div()
                                        .flex()
                                        .gap_1p5()
                                        .children(
                                            [
                                                (true, gpu_label),
                                                (false, "CPU (x264 / x265)".to_string()),
                                            ]
                                            .into_iter()
                                            .map(|(use_gpu, lbl)| {
                                                let active = opts.use_gpu == use_gpu;
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "exp-gpu-{}",
                                                        use_gpu
                                                    )))
                                                    .px_2p5()
                                                    .py_1()
                                                    .rounded_sm()
                                                    .bg(if active {
                                                        Theme::accent()
                                                    } else {
                                                        Theme::bg_surface()
                                                    })
                                                    .border_1()
                                                    .border_color(if active {
                                                        Theme::accent()
                                                    } else {
                                                        Theme::border()
                                                    })
                                                    .text_xs()
                                                    .text_color(Theme::text_primary())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.export_options.use_gpu = use_gpu;
                                                        cx.notify();
                                                    }))
                                                    .child(lbl)
                                            }),
                                        )
                                }),
                        )
                    })
                    .child(
                        div()
                            .pt_2()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("exp-cancel")
                                    .px_3()
                                    .py_1p5()
                                    .rounded_md()
                                    .bg(Theme::bg_surface())
                                    .border_1()
                                    .border_color(Theme::border())
                                    .text_xs()
                                    .text_color(Theme::text_secondary())
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.export_modal_open = false;
                                        cx.notify();
                                    }))
                                    .child("Cancel"),
                            )
                            .child(
                                div()
                                    .id("exp-start")
                                    .px_4()
                                    .py_1p5()
                                    .rounded_md()
                                    .bg(Theme::accent())
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(Theme::text_primary())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(Theme::ACCENT_HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.export_modal_open = false;
                                        this.prompt_export_video(cx);
                                    }))
                                    .child("Choose File & Export"),
                            ),
                    ),
            )
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
            .h(px(36.))
            .w_full()
            .px_3()
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
                    .gap_2()
                    .child(
                        div()
                            .id("btn-top-menu")
                            .px_2p5()
                            .py_1()
                            .rounded_sm()
                            .bg(if self.menu_open {
                                Theme::bg_hover()
                            } else {
                                Theme::bg_surface()
                            })
                            .border_1()
                            .border_color(if self.menu_open {
                                Theme::accent()
                            } else {
                                Theme::border()
                            })
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(Theme::text_primary())
                            .cursor_pointer()
                            .hover(|s| s.bg(Theme::bg_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu_open = !this.menu_open;
                                cx.notify();
                            }))
                            .child("≡ Menu"),
                    )
                    .child(div().w(px(1.)).h(px(16.)).bg(Theme::border()))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Theme::text_primary())
                            .child(project_name),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(Theme::bg_surface())
                            .text_xs()
                            .text_color(Theme::text_muted())
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
                                "Export"
                            },
                            true,
                        )
                        .tooltip(build_tooltip("Export Video / Audio"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !matches!(this.export_status, ExportStatus::Exporting { .. }) {
                                this.export_options.height = this.state.base_height.max(480);
                                this.export_options.fps = this.state.fps.clamp(24, 60);
                                this.export_modal_open = true;
                                this.menu_open = false;
                                cx.notify();
                            }
                        })),
                    ),
            )
    }

    fn render_menu_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |id: &'static str, label: &'static str, shortcut: &'static str| {
            div()
                .id(id)
                .w_full()
                .px_3()
                .py_1p5()
                .rounded_sm()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .text_color(Theme::text_primary())
                .cursor_pointer()
                .hover(|s| s.bg(Theme::bg_hover()))
                .child(label)
                .child(div().text_xs().text_color(Theme::text_muted()).child(shortcut))
        };

        div()
            .id("top-menu-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.menu_open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("top-menu-dropdown")
                    .absolute()
                    .top(px(38.))
                    .left(px(12.))
                    .w(px(230.))
                    .p_1p5()
                    .rounded_md()
                    .bg(Theme::bg_panel())
                    .border_1()
                    .border_color(Theme::border())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, _| {}))
                    .child(item("menu-home", "Back to Projects", "").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.menu_open = false;
                            this.state.is_playing = false;
                            this.player.stop();
                            this.audio_player.stop();
                            this.recent_projects = load_recent_projects();
                            this.screen = AppScreen::ProjectHub;
                            cx.notify();
                        },
                    )))
                    .child(div().my_1().h(px(1.)).w_full().bg(Theme::border()))
                    .child(item("menu-open", "Open Project...", "").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.menu_open = false;
                            this.prompt_open_project(cx);
                        },
                    )))
                    .child(item("menu-save", "Save Project", "Ctrl+S").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.menu_open = false;
                            this.save_project(cx);
                        },
                    )))
                    .child(div().my_1().h(px(1.)).w_full().bg(Theme::border()))
                    .child(item("menu-import", "Import Media...", "").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.menu_open = false;
                            this.prompt_import_media(cx);
                        },
                    )))
                    .child(item("menu-import-font", "Import Font (.ttf/.otf)...", "").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.menu_open = false;
                            this.prompt_import_font(cx);
                        }),
                    )),
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
        let effective_filter = match self.media_panel_tab {
            MediaPanelTab::Media => self.media_filter,
            MediaPanelTab::Text => None,
        };
        let assets: Vec<_> = self
            .state
            .filtered_assets(&self.media_search_query, effective_filter)
            .into_iter()
            .cloned()
            .collect();
        let total_assets = self.state.assets.len();
        let selected_asset_id = self.state.selected_asset_id;
        let active_filter = self.media_filter;
        let active_tab = self.media_panel_tab;
        let is_empty = assets.is_empty();

        div()
            .id("media-pool-dropzone")
            .w(px(272.))
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
            // CapCut-style top tab strip: Media | Text
            .child(
                div()
                    .h(px(34.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(Theme::border())
                    .children(
                        [
                            (MediaPanelTab::Media, "Media"),
                            (MediaPanelTab::Text, "Text"),
                        ]
                        .into_iter()
                        .map(|(tab, label)| {
                            let is_active = active_tab == tab;
                            div()
                                .id(SharedString::from(format!("mp-tab-{}", label)))
                                .px_2p5()
                                .py_1()
                                .rounded_sm()
                                .bg(if is_active {
                                    Theme::bg_surface()
                                } else {
                                    Theme::bg_panel()
                                })
                                .border_b_2()
                                .border_color(if is_active {
                                    Theme::accent()
                                } else {
                                    Theme::bg_panel()
                                })
                                .text_xs()
                                .font_weight(if is_active {
                                    gpui::FontWeight::SEMIBOLD
                                } else {
                                    gpui::FontWeight::MEDIUM
                                })
                                .text_color(if is_active {
                                    Theme::text_primary()
                                } else {
                                    Theme::text_secondary()
                                })
                                .cursor_pointer()
                                .hover(|s| s.bg(Theme::bg_hover()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.media_panel_tab = tab;
                                    cx.notify();
                                }))
                                .child(label)
                        }),
                    ),
            )
            .when(active_tab == MediaPanelTab::Text, |el| {
                el.child(
                    div()
                        .flex_1()
                        .p_2p5()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .id("mp-add-default-text")
                                .w_full()
                                .py_2()
                                .rounded_md()
                                .bg(Theme::accent())
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_center()
                                .text_color(Theme::text_primary())
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(Theme::ACCENT_HOVER)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.add_text_clip("Text", 4.0);
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                }))
                                .child("+ Add Default Text"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Theme::text_secondary())
                                .child("Text Presets"),
                        )
                        .child(
                            div()
                                .grid()
                                .grid_cols(2)
                                .gap_1p5()
                                .children(TextPreset::all().iter().copied().map(|preset| {
                                    div()
                                        .id(SharedString::from(format!(
                                            "mp-preset-{:?}",
                                            preset
                                        )))
                                        .p_2()
                                        .rounded_md()
                                        .bg(Theme::bg_surface())
                                        .border_1()
                                        .border_color(Theme::border())
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_center()
                                        .text_color(Theme::text_primary())
                                        .cursor_pointer()
                                        .hover(|s| {
                                            s.border_color(Theme::accent()).bg(Theme::bg_hover())
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(cid) =
                                                this.state.add_text_clip(preset.label(), 4.0)
                                            {
                                                if let Some(c) =
                                                    this.state.clips.iter_mut().find(|c| c.id == cid)
                                                {
                                                    if let Some(ref mut td) = c.text {
                                                        td.apply_preset(preset);
                                                    }
                                                }
                                                this.sync_preview_after_seek(cx);
                                            }
                                            cx.notify();
                                        }))
                                        .child(preset.label())
                                })),
                        ),
                )
            })
            .when(active_tab != MediaPanelTab::Text, |el| {
                el.child(
                    div()
                        .p_2()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .id("media-search-box")
                                .flex_1()
                                .h(px(26.))
                                .px_2()
                                .rounded_sm()
                                .bg(Theme::bg_surface())
                                .border_1()
                                .border_color(if self.media_search_focused {
                                    Theme::accent()
                                } else {
                                    Theme::border()
                                })
                                .flex()
                                .items_center()
                                .justify_between()
                                .cursor_text()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.media_search_focused = true;
                                    this.editing_text_clip_id = None;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if self.media_search_query.is_empty() {
                                            Theme::text_muted()
                                        } else {
                                            Theme::text_primary()
                                        })
                                        .child(if self.media_search_query.is_empty() {
                                            if self.media_search_focused {
                                                "Type to search...".to_string()
                                            } else {
                                                "Search...".to_string()
                                            }
                                        } else {
                                            self.media_search_query.clone()
                                        }),
                                )
                                .when(!self.media_search_query.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .id("media-search-clear")
                                            .px_1()
                                            .text_xs()
                                            .text_color(Theme::text_muted())
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.media_search_query.clear();
                                                cx.notify();
                                            }))
                                            .child("✕"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id("media-add-btn")
                                .h(px(26.))
                                .px_2p5()
                                .rounded_sm()
                                .bg(Theme::accent())
                                .flex()
                                .items_center()
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
                .when(active_tab == MediaPanelTab::Media, |el| {
                    el.child(
                        div()
                            .px_2()
                            .pb_1p5()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(
                                [
                                    (None, "All"),
                                    (Some(TrackKind::Video), "Video"),
                                    (Some(TrackKind::Overlay), "Image"),
                                    (Some(TrackKind::Audio), "Audio"),
                                ]
                                .into_iter()
                                .map(|(filter_val, label)| {
                                    let is_active = active_filter == filter_val;
                                    div()
                                        .id(SharedString::from(format!("mf-{}", label)))
                                        .px_2()
                                        .py_0p5()
                                        .rounded_sm()
                                        .bg(if is_active {
                                            Theme::accent()
                                        } else {
                                            Theme::bg_surface()
                                        })
                                        .text_xs()
                                        .text_color(if is_active {
                                            Theme::text_primary()
                                        } else {
                                            Theme::text_secondary()
                                        })
                                        .cursor_pointer()
                                        .hover(|s| s.bg(Theme::bg_hover()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.media_filter = filter_val;
                                            cx.notify();
                                        }))
                                        .child(label)
                                }),
                            ),
                    )
                })
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
                            .child(if total_assets == 0 {
                                "No media imported yet"
                            } else {
                                "No matching media in filter"
                            }),
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
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "asset-insert-{}",
                                                asset_id
                                            )))
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
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "asset-remove-{}",
                                                asset_id
                                            )))
                                            .px_1p5()
                                            .py_1()
                                            .rounded_sm()
                                            .text_xs()
                                            .text_color(Theme::text_muted())
                                            .cursor_pointer()
                                            .hover(|s| {
                                                s.bg(Theme::bg_panel())
                                                    .text_color(Theme::text_primary())
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.state.remove_asset(asset_id);
                                                cx.notify();
                                            }))
                                            .child("✕"),
                                    ),
                            )
                    }))
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_rotated_transform_handles(
        &self,
        clip_id: usize,
        rel_cx: f32,
        rel_cy: f32,
        box_w: f32,
        box_h: f32,
        init_px: f32,
        init_py: f32,
        init_scale: u16,
        init_font: f32,
        init_rot: f32,
        center_x: f32,
        center_y: f32,
        preview_scale: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hw = (box_w * 0.5).max(8.0);
        let hh = (box_h * 0.5).max(8.0);
        let rad = init_rot.to_radians();
        let cos_a = rad.cos();
        let sin_a = rad.sin();
        let rot_pt = |lx: f32, ly: f32| -> (f32, f32) {
            (
                rel_cx + lx * cos_a - ly * sin_a,
                rel_cy + lx * sin_a + ly * cos_a,
            )
        };

        let tl = rot_pt(-hw, -hh);
        let tr = rot_pt(hw, -hh);
        let br = rot_pt(hw, hh);
        let bl = rot_pt(-hw, hh);
        let tc = rot_pt(0.0, -hh);
        let rh = rot_pt(0.0, -(hh + 24.0));

        let corners = [
            ("tl", tl.0, tl.1, gpui::CursorStyle::ResizeUpRightDownLeft),
            ("tr", tr.0, tr.1, gpui::CursorStyle::ResizeUpLeftDownRight),
            ("bl", bl.0, bl.1, gpui::CursorStyle::ResizeUpLeftDownRight),
            ("br", br.0, br.1, gpui::CursorStyle::ResizeUpRightDownLeft),
        ];
        let em_t = rot_pt(0.0, -hh);
        let em_b = rot_pt(0.0, hh);
        let em_l = rot_pt(-hw, 0.0);
        let em_r = rot_pt(hw, 0.0);
        let edges = [
            ("top", em_t.0, em_t.1, gpui::CursorStyle::ResizeUpDown),
            ("bottom", em_b.0, em_b.1, gpui::CursorStyle::ResizeUpDown),
            ("left", em_l.0, em_l.1, gpui::CursorStyle::ResizeLeftRight),
            ("right", em_r.0, em_r.1, gpui::CursorStyle::ResizeLeftRight),
        ];

        let is_rotating_now = self.preview_text_drag.as_ref().is_some_and(|d| {
            d.clip_id == clip_id && matches!(d.kind, PreviewTransformKind::Rotate)
        });
        let snapped_key_angle = [0.0_f32, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0]
            .iter()
            .any(|t| ((init_rot.rem_euclid(360.0) - t + 180.0).rem_euclid(360.0) - 180.0).abs() < 0.05);

        div()
            .absolute()
            .inset_0()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let ox = f32::from(bounds.origin.x);
                        let oy = f32::from(bounds.origin.y);
                        if is_rotating_now {
                            let axis_len = (hw.max(hh) + 36.0).max(64.0);
                            let mut gpb = PathBuilder::stroke(px(1.0));
                            gpb.move_to(point(px(ox + rel_cx - axis_len), px(oy + rel_cy)));
                            gpb.line_to(point(px(ox + rel_cx + axis_len), px(oy + rel_cy)));
                            gpb.move_to(point(px(ox + rel_cx), px(oy + rel_cy - axis_len)));
                            gpb.line_to(point(px(ox + rel_cx), px(oy + rel_cy + axis_len)));
                            if let Ok(gpath) = gpb.build() {
                                let gcolor = if snapped_key_angle {
                                    gpui::rgba(0xfacc15cc)
                                } else {
                                    gpui::rgba(0xffffff55)
                                };
                                window.paint_path(gpath, gcolor);
                            }

                            let mut rpb = PathBuilder::stroke(px(1.2));
                            rpb.move_to(point(px(ox + rel_cx), px(oy + rel_cy)));
                            rpb.line_to(point(px(ox + rh.0), px(oy + rh.1)));
                            if let Ok(rpath) = rpb.build() {
                                let rcolor = if snapped_key_angle {
                                    gpui::rgba(0xfacc15ee)
                                } else {
                                    gpui::rgba(0x38bdf8cc)
                                };
                                window.paint_path(rpath, rcolor);
                            }
                        }

                        let mut pb = PathBuilder::stroke(px(1.5));
                        pb.move_to(point(px(ox + tl.0), px(oy + tl.1)));
                        pb.line_to(point(px(ox + tr.0), px(oy + tr.1)));
                        pb.line_to(point(px(ox + br.0), px(oy + br.1)));
                        pb.line_to(point(px(ox + bl.0), px(oy + bl.1)));
                        pb.close();
                        pb.move_to(point(px(ox + tc.0), px(oy + tc.1)));
                        pb.line_to(point(px(ox + rh.0), px(oy + rh.1)));
                        if let Ok(path) = pb.build() {
                            window.paint_path(path, Theme::accent());
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .children(edges.into_iter().map(|(etag, ex, ey, cursor)| {
                div()
                    .id(SharedString::from(format!("rsz-e-{}-{}", etag, clip_id)))
                    .absolute()
                    .left(px(ex - 7.0))
                    .top(px(ey - 7.0))
                    .w(px(14.))
                    .h(px(14.))
                    .cursor(cursor)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.state.push_undo();
                            this.preview_text_drag = Some(PreviewTextDragState {
                                clip_id,
                                kind: PreviewTransformKind::ResizeCorner,
                                start_mouse_x: f32::from(event.position.x),
                                start_mouse_y: f32::from(event.position.y),
                                initial_pos_x: init_px,
                                initial_pos_y: init_py,
                                initial_scale: init_scale,
                                initial_font_size: init_font,
                                initial_rot: init_rot,
                                center_x,
                                center_y,
                                preview_scale,
                                moved: false,
                            });
                            cx.notify();
                        }),
                    )
            }))
            .child(
                div()
                    .id(SharedString::from(format!("rot-hdl-{}", clip_id)))
                    .absolute()
                    .left(px(rh.0 - 11.0))
                    .top(px(rh.1 - 11.0))
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .bg(Theme::accent())
                    .border_1()
                    .border_color(Theme::text_primary())
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(Theme::text_primary())
                    .cursor_pointer()
                    .tooltip(build_tooltip("Drag to Rotate (Shift: snap 15°)"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.state.push_undo();
                            let mx = f32::from(event.position.x);
                            let my = f32::from(event.position.y);
                            let exact_cx = mx - (rh.0 - rel_cx);
                            let exact_cy = my - (rh.1 - rel_cy);
                            this.preview_text_drag = Some(PreviewTextDragState {
                                clip_id,
                                kind: PreviewTransformKind::Rotate,
                                start_mouse_x: mx,
                                start_mouse_y: my,
                                initial_pos_x: init_px,
                                initial_pos_y: init_py,
                                initial_scale: init_scale,
                                initial_font_size: init_font,
                                initial_rot: init_rot,
                                center_x: exact_cx,
                                center_y: exact_cy,
                                preview_scale,
                                moved: false,
                            });
                            cx.notify();
                        }),
                    )
                    .child("↻"),
            )
            .when(is_rotating_now, |el| {
                el.child(
                    div()
                        .absolute()
                        .left(px(rh.0 + 15.0))
                        .top(px(rh.1 - 10.0))
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .bg(Theme::bg_panel())
                        .border_1()
                        .border_color(if snapped_key_angle {
                            gpui::rgba(0xfacc15ff)
                        } else {
                            gpui::rgba(0x3b82f6ff)
                        })
                        .text_xs()
                        .text_color(if snapped_key_angle {
                            gpui::rgba(0xfacc15ff)
                        } else {
                            gpui::rgba(0xf4f4f5ff)
                        })
                        .child(format!("{:.0}°", init_rot)),
                )
            })
            .children(corners.into_iter().map(|(tag, cx_pt, cy_pt, cursor)| {
                div()
                    .id(SharedString::from(format!("rsz-{}-{}", tag, clip_id)))
                    .absolute()
                    .left(px(cx_pt - 4.0))
                    .top(px(cy_pt - 4.0))
                    .w(px(8.))
                    .h(px(8.))
                    .rounded_sm()
                    .bg(Theme::text_primary())
                    .border_1()
                    .border_color(Theme::accent())
                    .cursor(cursor)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.state.push_undo();
                            this.preview_text_drag = Some(PreviewTextDragState {
                                clip_id,
                                kind: PreviewTransformKind::ResizeCorner,
                                start_mouse_x: f32::from(event.position.x),
                                start_mouse_y: f32::from(event.position.y),
                                initial_pos_x: init_px,
                                initial_pos_y: init_py,
                                initial_scale: init_scale,
                                initial_font_size: init_font,
                                initial_rot: init_rot,
                                center_x,
                                center_y,
                                preview_scale,
                                moved: false,
                            });
                            cx.notify();
                        }),
                    )
            }))
    }

    fn render_preview_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_clips = self.state.active_clips_at_playhead();
        let active_main_video = active_clips
            .iter()
            .find(|c| c.track == TrackKind::Video)
            .cloned()
            .cloned();
        let active_overlays: Vec<_> = active_clips
            .iter()
            .filter(|c| c.track == TrackKind::Overlay)
            .cloned()
            .cloned()
            .collect();
        let active_texts: Vec<_> = active_clips
            .iter()
            .filter(|c| c.track == TrackKind::Text && c.text.is_some())
            .cloned()
            .cloned()
            .collect();
        let active_count = active_clips.len();
        let timecode = format_timecode(self.state.playhead_secs);
        let total_timecode = format_timecode(self.state.total_duration_secs);
        let is_playing = self.state.is_playing;

        let win_w = f32::from(window.viewport_size().width);
        let win_h = f32::from(window.viewport_size().height);
        let (avail_w, avail_h) = if self.preview_fullscreen {
            ((win_w - 48.0).max(240.0), (win_h - 48.0 - 48.0).max(180.0))
        } else {
            (
                (win_w - 272.0 - INSPECTOR_W - 64.0).max(200.0),
                (win_h - 36.0 - 234.0 - 48.0 - 56.0).max(180.0),
            )
        };
        let target_ratio = self.state.aspect_ratio.ratio_f32();
        let (base_pw, base_ph) = if avail_w / avail_h > target_ratio {
            (avail_h * target_ratio, avail_h)
        } else {
            (avail_w, avail_w / target_ratio)
        };
        let preview_w = base_pw * self.preview_zoom;
        let preview_h = base_ph * self.preview_zoom;
        let (_, out_h) = self.state.aspect_ratio.dimensions(self.state.base_height);
        let preview_scale = preview_h / (out_h.max(1) as f32);
        let pv_center_x = if self.preview_fullscreen {
            win_w * 0.5
        } else {
            272.0 + (win_w - 272.0 - INSPECTOR_W) * 0.5
        };
        let pv_center_y = if self.preview_fullscreen {
            (win_h - 48.0) * 0.5
        } else {
            36.0 + (win_h - 36.0 - 234.0 - 48.0) * 0.5
        };

        let mut selected_handle_overlay: Option<gpui::AnyElement> = None;

        let base_layer = if let Some(vclip) = active_main_video {
            if let Some(frame) = self.current_frame.clone() {
                let vid = vclip.id;
                let (epx, epy, esc, erot, eop) = vclip.evaluate_at(self.state.playhead_secs);
                let vscale = (esc as f32 / 100.0).clamp(0.1, 2.0);
                let valpha = (eop as f32 / 100.0).clamp(0.0, 1.0);
                let (fit_w, fit_h) =
                    self.fit_media_box(vclip.source_path.as_ref(), preview_w, preview_h);
                let box_w = fit_w * vscale;
                let box_h = fit_h * vscale;
                let vox = epx * preview_scale;
                let voy = epy * preview_scale;
                let is_sel = self.state.selected_clip_id == Some(vid);
                let obj_cx = pv_center_x + vox;
                let obj_cy = pv_center_y + voy;
                if is_sel {
                    selected_handle_overlay = Some(
                        self.render_rotated_transform_handles(
                            vid,
                            preview_w * 0.5 + vox,
                            preview_h * 0.5 + voy,
                            box_w,
                            box_h,
                            epx,
                            epy,
                            esc,
                            48.0,
                            erot,
                            obj_cx,
                            obj_cy,
                            preview_scale,
                            cx,
                        )
                        .into_any_element(),
                    );
                }

                div()
                    .id(SharedString::from(format!("pv-vid-{}", vid)))
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor(gpui::CursorStyle::OpenHand)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.state.selected_clip_id = Some(vid);
                            this.editing_text_clip_id = None;
                            this.state.push_undo();
                            this.preview_text_drag = Some(PreviewTextDragState {
                                clip_id: vid,
                                kind: PreviewTransformKind::Move,
                                start_mouse_x: f32::from(event.position.x),
                                start_mouse_y: f32::from(event.position.y),
                                initial_pos_x: epx,
                                initial_pos_y: epy,
                                initial_scale: esc,
                                initial_font_size: 48.0,
                                initial_rot: erot,
                                center_x: obj_cx,
                                center_y: obj_cy,
                                preview_scale,
                                moved: false,
                            });
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .left(px(vox))
                            .top(px(voy))
                            .w(px(box_w))
                            .h(px(box_h))
                            .opacity(valpha)
                            .child(img(frame).size_full().object_fit(ObjectFit::Contain)),
                    )
                    .into_any_element()
            } else {
                self.render_empty_preview(
                    "Loading video frame...",
                    &timecode,
                    &total_timecode,
                    active_count,
                )
                .into_any_element()
            }
        } else if active_overlays.is_empty() && active_texts.is_empty() {
            self.render_empty_preview(
                "No video frame at playhead",
                &timecode,
                &total_timecode,
                active_count,
            )
            .into_any_element()
        } else {
            div().size_full().into_any_element()
        };

        let overlay_elements: Vec<_> = active_overlays
            .into_iter()
            .filter_map(|oclip| {
                let path = oclip.source_path.clone()?;
                let oid = oclip.id;
                let (epx, epy, esc, erot, eop) = oclip.evaluate_at(self.state.playhead_secs);
                let oscale = (esc as f32 / 100.0).clamp(0.1, 2.0);
                let oalpha = (eop as f32 / 100.0).clamp(0.0, 1.0);
                let (fit_w, fit_h) = self.fit_media_box(Some(&path), preview_w, preview_h);
                let box_w = fit_w * oscale;
                let box_h = fit_h * oscale;
                let ox = epx * preview_scale;
                let oy = epy * preview_scale;
                let is_sel = self.state.selected_clip_id == Some(oid);
                let obj_cx = pv_center_x + ox;
                let obj_cy = pv_center_y + oy;

                if is_sel {
                    selected_handle_overlay = Some(
                        self.render_rotated_transform_handles(
                            oid,
                            preview_w * 0.5 + ox,
                            preview_h * 0.5 + oy,
                            box_w,
                            box_h,
                            epx,
                            epy,
                            esc,
                            48.0,
                            erot,
                            obj_cx,
                            obj_cy,
                            preview_scale,
                            cx,
                        )
                        .into_any_element(),
                    );
                }

                Some(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .id(SharedString::from(format!("pv-ovl-{}", oid)))
                                .relative()
                                .left(px(ox))
                                .top(px(oy))
                                .w(px(box_w))
                                .h(px(box_h))
                                .opacity(oalpha)
                                .cursor(gpui::CursorStyle::OpenHand)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.state.selected_clip_id = Some(oid);
                                        this.editing_text_clip_id = None;
                                        this.state.push_undo();
                                        this.preview_text_drag = Some(PreviewTextDragState {
                                            clip_id: oid,
                                            kind: PreviewTransformKind::Move,
                                            start_mouse_x: f32::from(event.position.x),
                                            start_mouse_y: f32::from(event.position.y),
                                            initial_pos_x: epx,
                                            initial_pos_y: epy,
                                            initial_scale: esc,
                                            initial_font_size: 48.0,
                                            initial_rot: erot,
                                            center_x: obj_cx,
                                            center_y: obj_cy,
                                            preview_scale,
                                            moved: false,
                                        });
                                        cx.notify();
                                    }),
                                )
                                .child(img(path).size_full().object_fit(ObjectFit::Contain)),
                        ),
                )
            })
            .collect();

        let text_elements: Vec<_> = active_texts
            .into_iter()
            .map(|tclip| {
                let tclip_id = tclip.id;
                let is_sel = self.state.selected_clip_id == Some(tclip_id);
                let is_editing = self.editing_text_clip_id == Some(tclip_id);
                let td = tclip.text.clone().unwrap_or_default();

                let elapsed = (self.state.playhead_secs - tclip.start_secs).max(0.0);
                let remaining = (tclip.end_secs() - self.state.playhead_secs).max(0.0);
                let ad = td
                    .anim_duration_secs
                    .clamp(0.1, (tclip.duration_secs * 0.5).max(0.1));
                let (anim_alpha, anim_scale, anim_dy) = match td.animation {
                    TextAnimation::None => (1.0, 1.0, 0.0),
                    TextAnimation::FadeIn => ((elapsed / ad).clamp(0.0, 1.0), 1.0, 0.0),
                    TextAnimation::FadeOut => ((remaining / ad).clamp(0.0, 1.0), 1.0, 0.0),
                    TextAnimation::FadeInOut => {
                        let fin = (elapsed / ad).clamp(0.0, 1.0);
                        let fout = (remaining / ad).clamp(0.0, 1.0);
                        (fin.min(fout), 1.0, 0.0)
                    }
                    TextAnimation::PopIn => {
                        let p = (elapsed / ad).clamp(0.0, 1.0);
                        (p, 0.55 + 0.45 * p, 0.0)
                    }
                    TextAnimation::SlideUp => {
                        let p = (elapsed / ad).clamp(0.0, 1.0);
                        (p, 1.0, (1.0 - p) * 60.0)
                    }
                };

                let (epx, epy, esc, erot, eop) = tclip.evaluate_at(self.state.playhead_secs);
                let clip_scale = (esc as f32 / 100.0).clamp(0.1, 2.0);
                let scaled_font =
                    (td.font_size * clip_scale * preview_scale * anim_scale).clamp(8.0, 220.0);
                let ox = epx * preview_scale;
                let oy = (epy + anim_dy) * preview_scale;
                let alpha = ((eop as f32 / 100.0) * anim_alpha).clamp(0.0, 1.0);
                let obj_cx = pv_center_x + ox;
                let obj_cy = pv_center_y + oy;
                let display_text = if is_editing {
                    format!("{}|", td.content)
                } else if td.content.is_empty() {
                    "Double-click to edit".to_string()
                } else {
                    td.content.clone()
                };
                let shadow_ox = td.shadow_offset_x * preview_scale;
                let shadow_oy = td.shadow_offset_y * preview_scale;
                let pad_px = (td.bg_padding * preview_scale).clamp(2.0, 24.0);

                if is_sel && !is_editing {
                    let mut max_w = 0.0_f32;
                    let mut line_count = 0usize;
                    let mut font_def = gpui::font(td.font_family.clone());
                    font_def.weight = gpui::FontWeight(td.font_weight as f32);
                    for line in display_text.lines() {
                        line_count += 1;
                        let run = gpui::TextRun {
                            len: line.len(),
                            font: font_def.clone(),
                            color: gpui::hsla(0.0, 0.0, 1.0, 1.0),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let shaped = window.text_system().shape_line(
                            SharedString::from(line.to_string()),
                            px(scaled_font),
                            &[run],
                            None,
                        );
                        max_w = max_w.max(f32::from(shaped.width));
                    }
                    let lines = line_count.max(1) as f32;
                    let box_w = (max_w + pad_px * 2.0 + 4.0).max(40.0);
                    let box_h = (lines * scaled_font * 1.3 + pad_px * 1.2 + 4.0).max(26.0);
                    selected_handle_overlay = Some(
                        self.render_rotated_transform_handles(
                            tclip_id,
                            preview_w * 0.5 + ox,
                            preview_h * 0.5 + oy,
                            box_w,
                            box_h,
                            epx,
                            epy,
                            esc,
                            td.font_size,
                            erot,
                            obj_cx,
                            obj_cy,
                            preview_scale,
                            cx,
                        )
                        .into_any_element(),
                    );
                }

                let norm_rot = erot.rem_euclid(360.0);
                let use_rot_img =
                    !is_editing && (tclip.flip_h || (norm_rot > 0.5 && norm_rot < 359.5));
                let cached_rot_img = if use_rot_img {
                    self.rotated_text_cache
                        .get(&tclip_id)
                        .map(|(_, im, w, h)| (im.clone(), *w, *h))
                } else {
                    None
                };

                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some((r_img, rw, rh)) = cached_rot_img {
                        div()
                            .id(SharedString::from(format!("pv-text-{}", tclip_id)))
                            .relative()
                            .left(px(ox))
                            .top(px(oy))
                            .w(px(rw as f32 * preview_scale))
                            .h(px(rh as f32 * preview_scale))
                            .opacity(alpha)
                            .cursor(gpui::CursorStyle::OpenHand)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.state.selected_clip_id = Some(tclip_id);
                                    if event.click_count >= 2 {
                                        if this.editing_text_clip_id != Some(tclip_id) {
                                            this.state.push_undo();
                                            this.editing_text_clip_id = Some(tclip_id);
                                        }
                                        this.preview_text_drag = None;
                                    } else {
                                        this.state.push_undo();
                                        this.preview_text_drag = Some(PreviewTextDragState {
                                            clip_id: tclip_id,
                                            kind: PreviewTransformKind::Move,
                                            start_mouse_x: f32::from(event.position.x),
                                            start_mouse_y: f32::from(event.position.y),
                                            initial_pos_x: epx,
                                            initial_pos_y: epy,
                                            initial_scale: esc,
                                            initial_font_size: td.font_size,
                                            initial_rot: erot,
                                            center_x: obj_cx,
                                            center_y: obj_cy,
                                            preview_scale,
                                            moved: false,
                                        });
                                    }
                                    cx.notify();
                                }),
                            )
                            .child(img(r_img).size_full().object_fit(ObjectFit::Contain))
                            .into_any_element()
                    } else {
                        div()
                            .id(SharedString::from(format!("pv-text-{}", tclip_id)))
                            .relative()
                            .left(px(ox))
                            .top(px(oy))
                            .px(px(pad_px))
                            .py(px(pad_px * 0.6))
                            .rounded_sm()
                            .when_some(td.bg_color_hex, |el, bg_hex| el.bg(rgb(bg_hex)))
                            .border_1()
                            .border_color(if is_editing {
                                Theme::accent()
                            } else {
                                gpui::rgba(0x00000000)
                            })
                            .opacity(alpha)
                            .font_family(td.font_family.clone())
                            .text_size(px(scaled_font))
                            .font_weight(gpui::FontWeight(td.font_weight as f32))
                            .cursor(if is_editing {
                                gpui::CursorStyle::IBeam
                            } else {
                                gpui::CursorStyle::OpenHand
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.state.selected_clip_id = Some(tclip_id);
                                    if event.click_count >= 2 {
                                        if this.editing_text_clip_id != Some(tclip_id) {
                                            this.state.push_undo();
                                            this.editing_text_clip_id = Some(tclip_id);
                                        }
                                        this.preview_text_drag = None;
                                    } else {
                                        this.state.push_undo();
                                        this.preview_text_drag = Some(PreviewTextDragState {
                                            clip_id: tclip_id,
                                            kind: PreviewTransformKind::Move,
                                            start_mouse_x: f32::from(event.position.x),
                                            start_mouse_y: f32::from(event.position.y),
                                            initial_pos_x: epx,
                                            initial_pos_y: epy,
                                            initial_scale: esc,
                                            initial_font_size: td.font_size,
                                            initial_rot: erot,
                                            center_x: obj_cx,
                                            center_y: obj_cy,
                                            preview_scale,
                                            moved: false,
                                        });
                                    }
                                    cx.notify();
                                }),
                            )
                            .when_some(td.shadow_color_hex, |el, sh_hex| {
                                el.child(
                                    div()
                                        .absolute()
                                        .left(px(pad_px + shadow_ox))
                                        .top(px(pad_px * 0.6 + shadow_oy))
                                        .opacity(0.75)
                                        .text_color(rgb(sh_hex))
                                        .child(display_text.clone()),
                                )
                            })
                            .when(td.stroke_width > 0.1, |el| {
                                let sw =
                                    (td.stroke_width * preview_scale * 2.2).clamp(1.2, 10.0);
                                let sc = td.stroke_color_hex.unwrap_or(0x000000);
                                let d = sw * 0.7;
                                let offsets = [
                                    (-sw, 0.0),
                                    (sw, 0.0),
                                    (0.0, -sw),
                                    (0.0, sw),
                                    (-d, -d),
                                    (d, -d),
                                    (-d, d),
                                    (d, d),
                                ];
                                el.children(offsets.into_iter().map(|(dx, dy)| {
                                    div()
                                        .absolute()
                                        .left(px(pad_px + dx))
                                        .top(px(pad_px * 0.6 + dy))
                                        .text_color(rgb(sc))
                                        .child(display_text.clone())
                                }))
                            })
                            .child(
                                div()
                                    .relative()
                                    .text_color(rgb(td.color_hex))
                                    .child(display_text),
                            )
                            .into_any_element()
                    })
            })
            .collect();

        let is_editing_any_text = self.editing_text_clip_id.is_some();
        let focus_for_ime = self.focus_handle.clone();
        let view_entity = cx.entity().clone();

        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::bg_app())
            .child(
                div()
                    .id("preview-stage")
                    .flex_1()
                    .pt(px(36.))
                    .pb(px(20.))
                    .px(px(24.))
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        if !event.modifiers.control {
                            return;
                        }
                        let dy = match event.delta {
                            ScrollDelta::Pixels(p) => f32::from(p.y),
                            ScrollDelta::Lines(p) => p.y * 24.0,
                        };
                        let factor = if dy > 0.0 { 1.1 } else { 0.9 };
                        this.preview_zoom = (this.preview_zoom * factor).clamp(0.35, 2.5);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(preview_w))
                            .h(px(preview_h))
                            .relative()
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded_lg()
                                    .bg(Theme::bg_preview())
                                    .border_1()
                                    .border_color(Theme::border())
                                    .overflow_hidden()
                                    .when(is_editing_any_text, |el| {
                                        let fh = focus_for_ime.clone();
                                        let ent = view_entity.clone();
                                        el.child(
                                            canvas(
                                                |_, _, _| (),
                                                move |bounds, _, window, cx| {
                                                    window.handle_input(
                                                        &fh,
                                                        ElementInputHandler::new(bounds, ent),
                                                        cx,
                                                    );
                                                },
                                            )
                                            .absolute()
                                            .inset_0(),
                                        )
                                    })
                                    .child(base_layer)
                                    .children(overlay_elements)
                                    .children(text_elements)
                            .when(self.show_safe_zone, |el| {
                                let gx1 = preview_w / 3.0;
                                let gx2 = preview_w * 2.0 / 3.0;
                                let gy1 = preview_h / 3.0;
                                let gy2 = preview_h * 2.0 / 3.0;
                                let sx = preview_w * 0.08;
                                let sy = preview_h * 0.08;
                                el.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px(gx1))
                                                .top_0()
                                                .w(px(1.))
                                                .h_full()
                                                .bg(gpui::rgba(0xffffff33)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px(gx2))
                                                .top_0()
                                                .w(px(1.))
                                                .h_full()
                                                .bg(gpui::rgba(0xffffff33)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .top(px(gy1))
                                                .left_0()
                                                .h(px(1.))
                                                .w_full()
                                                .bg(gpui::rgba(0xffffff33)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .top(px(gy2))
                                                .left_0()
                                                .h(px(1.))
                                                .w_full()
                                                .bg(gpui::rgba(0xffffff33)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px(sx))
                                                .top(px(sy))
                                                .w(px(preview_w - sx * 2.0))
                                                .h(px(preview_h - sy * 2.0))
                                                .border_1()
                                                .border_color(gpui::rgba(0xfacc1566)),
                                        ),
                                )
                            })
                            .child({
                                let g = self.active_snap_guides;
                                let sx = preview_w * 0.08;
                                let sy = preview_h * 0.08;
                                div()
                                    .absolute()
                                    .inset_0()
                                    .when(g.center_x, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .left(px(preview_w * 0.5))
                                                .top_0()
                                                .w(px(1.5))
                                                .h_full()
                                                .bg(gpui::rgba(0x38bdf8ee)),
                                        )
                                    })
                                    .when(g.center_y, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .top(px(preview_h * 0.5))
                                                .left_0()
                                                .h(px(1.5))
                                                .w_full()
                                                .bg(gpui::rgba(0x38bdf8ee)),
                                        )
                                    })
                                    .when(g.left_edge, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .left_0()
                                                .top_0()
                                                .w(px(2.0))
                                                .h_full()
                                                .bg(gpui::rgba(0xf43f5eee)),
                                        )
                                    })
                                    .when(g.right_edge, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .right_0()
                                                .top_0()
                                                .w(px(2.0))
                                                .h_full()
                                                .bg(gpui::rgba(0xf43f5eee)),
                                        )
                                    })
                                    .when(g.top_edge, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .left_0()
                                                .h(px(2.0))
                                                .w_full()
                                                .bg(gpui::rgba(0xf43f5eee)),
                                        )
                                    })
                                    .when(g.bottom_edge, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .bottom_0()
                                                .left_0()
                                                .h(px(2.0))
                                                .w_full()
                                                .bg(gpui::rgba(0xf43f5eee)),
                                        )
                                    })
                                    .when(g.safe_left, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .left(px(sx))
                                                .top_0()
                                                .w(px(1.0))
                                                .h_full()
                                                .bg(gpui::rgba(0xfacc15dd)),
                                        )
                                    })
                                    .when(g.safe_right, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .left(px(preview_w - sx))
                                                .top_0()
                                                .w(px(1.0))
                                                .h_full()
                                                .bg(gpui::rgba(0xfacc15dd)),
                                        )
                                    })
                                    .when(g.safe_top, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .top(px(sy))
                                                .left_0()
                                                .h(px(1.0))
                                                .w_full()
                                                .bg(gpui::rgba(0xfacc15dd)),
                                        )
                                    })
                                    .when(g.safe_bottom, |el| {
                                        el.child(
                                            div()
                                                .absolute()
                                                .top(px(preview_h - sy))
                                                .left_0()
                                                .h(px(1.0))
                                                .w_full()
                                                .bg(gpui::rgba(0xfacc15dd)),
                                        )
                                    })
                            }),
                            )
                            .when_some(selected_handle_overlay, |el, hdl| el.child(hdl)),
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
                            .gap_1p5()
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
                                    "tr-fback",
                                    "‹|",
                                    "Previous Frame (,)",
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.step_frames(-1);
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
                                self.icon_button(
                                    "tr-ffwd",
                                    "|›",
                                    "Next Frame (.)",
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.step_frames(1);
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
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child({
                                let zoomed = (self.preview_zoom - 1.0).abs() > 0.02;
                                div()
                                    .id("pv-zoom-reset")
                                    .h(px(26.))
                                    .px_2()
                                    .rounded_md()
                                    .bg(if zoomed {
                                        Theme::accent()
                                    } else {
                                        Theme::bg_surface()
                                    })
                                    .border_1()
                                    .border_color(if zoomed {
                                        Theme::accent()
                                    } else {
                                        Theme::border()
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_xs()
                                    .text_color(if zoomed {
                                        rgb(0xFFFFFF)
                                    } else {
                                        Theme::text_primary()
                                    })
                                    .cursor_pointer()
                                    .hover(|s| {
                                        s.bg(if zoomed {
                                            rgb(Theme::ACCENT_HOVER)
                                        } else {
                                            Theme::bg_hover()
                                        })
                                    })
                                    .child(format!("{:.0}%", self.preview_zoom * 100.0))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.preview_zoom = 1.0;
                                        cx.notify();
                                    }))
                            })
                            .child(
                                self.icon_button(
                                    "pv-quality",
                                    self.preview_quality.label(),
                                    "Cycle Preview Quality (Full / 1/2 / 1/4)",
                                    self.preview_quality != PreviewQuality::Half,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.preview_quality = this.preview_quality.next();
                                    this.player.stop();
                                    this.sync_preview_after_seek(cx);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "pv-safezone",
                                    "⊞",
                                    if self.show_safe_zone {
                                        "Hide Grid & Safe Zone"
                                    } else {
                                        "Show Grid & Safe Zone"
                                    },
                                    self.show_safe_zone,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_safe_zone = !this.show_safe_zone;
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "pv-fullscreen",
                                    "⛶",
                                    if self.preview_fullscreen {
                                        "Exit Fullscreen (Esc)"
                                    } else {
                                        "Fullscreen Preview"
                                    },
                                    self.preview_fullscreen,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.preview_fullscreen = !this.preview_fullscreen;
                                    cx.notify();
                                })),
                            ),
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
            .w(px(INSPECTOR_W))
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
                    .id("inspector-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2p5()
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
                    .child({
                        let (_, _, _, _, eop) = clip.evaluate_at(self.state.playhead_secs);
                        self.inspector_slider(
                            "Opacity",
                            format!("{}%", eop),
                            eop as f32 / 100.0,
                            Theme::accent(),
                            SliderField::Opacity,
                            cx,
                        )
                    })
                    .when(clip.track != TrackKind::Text, |el| {
                        let (_, _, esc, _, _) = clip.evaluate_at(self.state.playhead_secs);
                        el.child(self.inspector_slider(
                            "Scale",
                            format!("{}%", esc),
                            ((esc as f32 - 10.0) / 190.0).clamp(0.0, 1.0),
                            Theme::accent(),
                            SliderField::Scale,
                            cx,
                        ))
                    })
                    .child({
                        let (_, _, _, erot, _) = clip.evaluate_at(self.state.playhead_secs);
                        self.inspector_slider(
                            "Rotate",
                            format!("{:.0}°", erot),
                            (erot.rem_euclid(360.0) / 360.0).clamp(0.0, 1.0),
                            Theme::accent(),
                            SliderField::Rotation,
                            cx,
                        )
                    })
                    .when(clip.track.is_visual(), |el| {
                        let (_, _, _, erot, _) = clip.evaluate_at(self.state.playhead_secs);
                        let cur_rot = erot.rem_euclid(360.0);
                        let presets: [(&str, f32, bool); 5] = [
                            ("-90°", -90.0, true),
                            ("0°", 0.0, false),
                            ("90°", 90.0, false),
                            ("180°", 180.0, false),
                            ("+90°", 90.0, true),
                        ];
                        let aligns: [(&str, &str, u8); 7] = [
                            ("⇤", "Căn lề trái", 0),
                            ("⇹", "Căn giữa ngang", 1),
                            ("⇥", "Căn lề phải", 2),
                            ("⤒", "Căn lề trên", 3),
                            ("↕", "Căn giữa dọc", 4),
                            ("⤓", "Căn lề dưới", 5),
                            ("⌖", "Căn giữa tâm", 6),
                        ];
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .children(presets.into_iter().map(|(lbl, val, is_rel)| {
                                    let active = !is_rel && (cur_rot - val).abs() < 0.5;
                                    div()
                                        .id(SharedString::from(format!("ang-btn-{}", lbl)))
                                        .flex_1()
                                        .py_0p5()
                                        .rounded_sm()
                                        .bg(if active {
                                            Theme::accent()
                                        } else {
                                            Theme::bg_surface()
                                        })
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
                                        .text_color(Theme::text_primary())
                                        .cursor_pointer()
                                        .hover(|s| s.bg(Theme::bg_hover()))
                                        .child(lbl)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.state.push_undo();
                                            let playhead = this.state.playhead_secs;
                                            if let Some(c) = this.state.selected_clip_mut() {
                                                if !c.keyframes.is_empty() {
                                                    let (epx, epy, esc, erot, eop) =
                                                        c.evaluate_at(playhead);
                                                    c.pos_x = epx;
                                                    c.pos_y = epy;
                                                    c.scale = esc;
                                                    c.rotation_deg = erot;
                                                    c.opacity = eop;
                                                }
                                                c.rotation_deg = if is_rel {
                                                    (c.rotation_deg + val).rem_euclid(360.0)
                                                } else {
                                                    val
                                                };
                                                let deg = c.rotation_deg;
                                                c.sync_keyframe_if_animated(playhead);
                                                this.state.status_message =
                                                    format!("Angle: {:.0}°", deg);
                                            }
                                            this.sync_preview_after_seek(cx);
                                            cx.notify();
                                        }))
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(Theme::text_secondary())
                                        .child("Align & Center"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .children(aligns.into_iter().map(|(icon, tip, mode)| {
                                            div()
                                                .id(SharedString::from(format!(
                                                    "align-btn-{}",
                                                    mode
                                                )))
                                                .flex_1()
                                                .py_1()
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
                                                .tooltip(build_tooltip(tip))
                                                .hover(|s| s.bg(Theme::bg_hover()))
                                                .child(icon)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    let Some(sel_id) = this.state.selected_clip_id
                                                    else {
                                                        return;
                                                    };
                                                    let (out_w, out_h) = this
                                                        .state
                                                        .aspect_ratio
                                                        .dimensions(this.state.base_height);
                                                    let cw = out_w as f32;
                                                    let ch = out_h as f32;
                                                    let (hw, hh) =
                                                        this.clip_canvas_half_extents(sel_id);
                                                    this.state.push_undo();
                                                    if let Some(c) = this
                                                        .state
                                                        .clips
                                                        .iter_mut()
                                                        .find(|c| c.id == sel_id)
                                                    {
                                                        match mode {
                                                            0 => c.pos_x = -(cw * 0.5 - hw),
                                                            1 => c.pos_x = 0.0,
                                                            2 => c.pos_x = cw * 0.5 - hw,
                                                            3 => c.pos_y = -(ch * 0.5 - hh),
                                                            4 => c.pos_y = 0.0,
                                                            5 => c.pos_y = ch * 0.5 - hh,
                                                            _ => {
                                                                c.pos_x = 0.0;
                                                                c.pos_y = 0.0;
                                                            }
                                                        }
                                                    }
                                                    this.state.status_message = tip.to_string();
                                                    cx.notify();
                                                }))
                                        })),
                                ),
                        )
                    })
                    .when(clip.track == TrackKind::Video || clip.track == TrackKind::Audio, |el| {
                        el.child(self.inspector_slider(
                            "Speed",
                            format!("{:.2}x", clip.speed),
                            ((clip.speed - 0.25) / 3.75).clamp(0.0, 1.0),
                            Theme::accent(),
                            SliderField::Speed,
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .when(clip.track.is_visual(), |el| {
                                el.child(
                                    div()
                                        .id("ins-mirror-btn")
                                        .flex_1()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(if clip.flip_h {
                                            Theme::accent()
                                        } else {
                                            Theme::bg_surface()
                                        })
                                        .border_1()
                                        .border_color(Theme::border())
                                        .text_xs()
                                        .text_center()
                                        .text_color(Theme::text_primary())
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if this.state.toggle_mirror_selected() {
                                                this.sync_preview_after_seek(cx);
                                            }
                                            cx.notify();
                                        }))
                                        .child("⇄ Mirror"),
                                )
                            })
                            .when(clip.track == TrackKind::Video, |el| {
                                el.child(
                                    div()
                                        .id("ins-rev-btn")
                                        .flex_1()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(if clip.reversed {
                                            Theme::accent()
                                        } else {
                                            Theme::bg_surface()
                                        })
                                        .border_1()
                                        .border_color(Theme::border())
                                        .text_xs()
                                        .text_center()
                                        .text_color(Theme::text_primary())
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if this.state.toggle_reverse_selected() {
                                                this.sync_preview_after_seek(cx);
                                            }
                                            cx.notify();
                                        }))
                                        .child("⟲ Reverse"),
                                )
                            })
                            .child(
                                div()
                                    .id("ins-kf-btn")
                                    .flex_1()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(Theme::bg_surface())
                                    .border_1()
                                    .border_color(rgb(0xf59e0b))
                                    .text_xs()
                                    .text_center()
                                    .text_color(rgb(0xf59e0b))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.toggle_keyframe_at_playhead();
                                        cx.notify();
                                    }))
                                    .child(format!("◆ KF ({})", clip.keyframes.len())),
                            ),
                    )
                    .when(clip.track.is_audible(), |el| {
                        let max_fade = (clip.duration_secs * 0.5).clamp(0.1, 5.0);
                        el.child(self.inspector_slider(
                            "Volume",
                            format!("{}%", clip.volume),
                            clip.volume as f32 / 100.0,
                            Theme::accent(),
                            SliderField::Volume,
                            cx,
                        ))
                        .child(self.inspector_slider(
                            "Fade In",
                            format!("{:.1}s", clip.fade_in_secs),
                            (clip.fade_in_secs / max_fade).clamp(0.0, 1.0),
                            Theme::accent(),
                            SliderField::FadeIn,
                            cx,
                        ))
                        .child(self.inspector_slider(
                            "Fade Out",
                            format!("{:.1}s", clip.fade_out_secs),
                            (clip.fade_out_secs / max_fade).clamp(0.0, 1.0),
                            Theme::accent(),
                            SliderField::FadeOut,
                            cx,
                        ))
                    })
                    .when(
                        clip.track == TrackKind::Video || clip.track == TrackKind::Overlay,
                        |el| {
                            let has_crop = clip.crop.left > 0.001
                                || clip.crop.right > 0.001
                                || clip.crop.top > 0.001
                                || clip.crop.bottom > 0.001;
                            el.child(div().h(px(1.)).w_full().bg(Theme::border()))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(Theme::text_secondary())
                                                .child("CROP"),
                                        )
                                        .when(has_crop, |el| {
                                            el.child(
                                                div()
                                                    .id("ins-reset-crop")
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .bg(Theme::bg_surface())
                                                    .text_xs()
                                                    .text_color(Theme::text_muted())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        if this.state.reset_crop_selected() {
                                                            this.sync_preview_after_seek(cx);
                                                        }
                                                        cx.notify();
                                                    }))
                                                    .child("Reset"),
                                            )
                                        }),
                                )
                                .child(self.inspector_slider(
                                    "Left",
                                    format!("{}%", (clip.crop.left * 100.0).round() as u32),
                                    (clip.crop.left / 0.45).clamp(0.0, 1.0),
                                    Theme::accent(),
                                    SliderField::CropLeft,
                                    cx,
                                ))
                                .child(self.inspector_slider(
                                    "Right",
                                    format!("{}%", (clip.crop.right * 100.0).round() as u32),
                                    (clip.crop.right / 0.45).clamp(0.0, 1.0),
                                    Theme::accent(),
                                    SliderField::CropRight,
                                    cx,
                                ))
                                .child(self.inspector_slider(
                                    "Top",
                                    format!("{}%", (clip.crop.top * 100.0).round() as u32),
                                    (clip.crop.top / 0.45).clamp(0.0, 1.0),
                                    Theme::accent(),
                                    SliderField::CropTop,
                                    cx,
                                ))
                                .child(self.inspector_slider(
                                    "Bottom",
                                    format!("{}%", (clip.crop.bottom * 100.0).round() as u32),
                                    (clip.crop.bottom / 0.45).clamp(0.0, 1.0),
                                    Theme::accent(),
                                    SliderField::CropBottom,
                                    cx,
                                ))
                        },
                    )
                    .when(clip.track == TrackKind::Text, |el| {
                        let td = clip.text.clone().unwrap_or_default();
                        let col_r = ((td.color_hex >> 16) & 0xff) as u8;
                        let col_g = ((td.color_hex >> 8) & 0xff) as u8;
                        let col_b = (td.color_hex & 0xff) as u8;

                        el.child(div().h(px(1.)).w_full().bg(Theme::border()))
                            .child(
                                div()
                                    .p_2()
                                    .rounded_md()
                                    .bg(Theme::bg_surface())
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .child("Double-click text in Preview to type • Drag to move"),
                            )
                            // Style Presets
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(Theme::text_secondary())
                                            .child("Style Presets"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_1()
                                            .children(TextPreset::all().iter().map(|&preset| {
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "tp-{}",
                                                        preset.label()
                                                    )))
                                                    .px_2()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .bg(Theme::bg_surface())
                                                    .border_1()
                                                    .border_color(Theme::border())
                                                    .text_xs()
                                                    .text_color(Theme::text_primary())
                                                    .cursor_pointer()
                                                    .hover(|s| {
                                                        s.border_color(Theme::accent())
                                                            .bg(Theme::bg_hover())
                                                    })
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.state.push_undo();
                                                        if let Some(c) =
                                                            this.state.selected_clip_mut()
                                                        {
                                                            if let Some(ref mut t) = c.text {
                                                                t.apply_preset(preset);
                                                            }
                                                        }
                                                        cx.notify();
                                                    }))
                                                    .child(preset.label())
                                            })),
                                    ),
                            )
                            // 10 Vietnamese-ready Fonts + Import
                            .child(
                                div()
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
                                                    .text_color(Theme::text_secondary())
                                                    .child("Font Family"),
                                            )
                                            .child(
                                                div()
                                                    .id("btn-import-font")
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .bg(Theme::bg_surface())
                                                    .border_1()
                                                    .border_color(Theme::border())
                                                    .text_xs()
                                                    .text_color(Theme::accent())
                                                    .cursor_pointer()
                                                    .hover(|s| s.bg(Theme::bg_hover()))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.prompt_import_font(cx);
                                                    }))
                                                    .child("+ Font"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_1()
                                            .children(
                                                [
                                                    "Be Vietnam Pro",
                                                    "Roboto",
                                                    "Montserrat",
                                                    "Oswald",
                                                    "Playfair Display",
                                                    "Quicksand",
                                                    "Pacifico",
                                                    "Lobster",
                                                    "Space Grotesk",
                                                    "Merriweather",
                                                ]
                                                .into_iter()
                                                .map(|fname| {
                                                    let active = td.font_family == fname;
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "ff-{}",
                                                            fname
                                                        )))
                                                        .px_1p5()
                                                        .py_0p5()
                                                        .rounded_sm()
                                                        .bg(if active {
                                                            Theme::accent()
                                                        } else {
                                                            Theme::bg_surface()
                                                        })
                                                        .border_1()
                                                        .border_color(Theme::border())
                                                        .text_xs()
                                                        .text_color(Theme::text_primary())
                                                        .cursor_pointer()
                                                        .hover(|s| s.bg(Theme::bg_hover()))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.state.push_undo();
                                                                if let Some(c) =
                                                                    this.state.selected_clip_mut()
                                                                {
                                                                    if let Some(ref mut t) = c.text
                                                                    {
                                                                        t.font_family =
                                                                            fname.into();
                                                                        t.font_path = None;
                                                                    }
                                                                }
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child(fname)
                                                }),
                                            ),
                                    ),
                            )
                            // Font Size & Weight sliders
                            .child(self.inspector_slider(
                                "Size",
                                format!("{:.0}px", td.font_size),
                                ((td.font_size - 12.0) / 128.0).clamp(0.0, 1.0),
                                Theme::accent(),
                                SliderField::FontSize,
                                cx,
                            ))
                            .child(self.inspector_slider(
                                "Weight",
                                match td.font_weight {
                                    0..=350 => "Light",
                                    351..=450 => "Regular",
                                    451..=600 => "Medium",
                                    601..=800 => "Bold",
                                    _ => "Black",
                                }
                                .into(),
                                ((td.font_weight as f32 - 300.0) / 600.0).clamp(0.0, 1.0),
                                Theme::accent(),
                                SliderField::FontWeight,
                                cx,
                            ))
                            // RGB Color Picker + Quick swatches
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1p5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1p5()
                                                    .child(
                                                        div()
                                                            .w(px(14.))
                                                            .h(px(14.))
                                                            .rounded_sm()
                                                            .bg(rgb(td.color_hex))
                                                            .border_1()
                                                            .border_color(Theme::border()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(Theme::text_secondary())
                                                            .child(format!(
                                                                "RGB #{:06X}",
                                                                td.color_hex
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .children(
                                                        [
                                                            0xffffff_u32,
                                                            0x000000,
                                                            0xfacc15,
                                                            0xef4444,
                                                            0x3b82f6,
                                                            0x22c55e,
                                                        ]
                                                        .into_iter()
                                                        .map(|hex| {
                                                            let active = td.color_hex == hex;
                                                            div()
                                                                .id(SharedString::from(format!(
                                                                    "tc-{:06x}",
                                                                    hex
                                                                )))
                                                                .w(px(14.))
                                                                .h(px(14.))
                                                                .rounded_sm()
                                                                .bg(rgb(hex))
                                                                .border_1()
                                                                .border_color(if active {
                                                                    Theme::accent()
                                                                } else {
                                                                    Theme::border()
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.state.push_undo();
                                                                        if let Some(c) = this
                                                                            .state
                                                                            .selected_clip_mut()
                                                                        {
                                                                            if let Some(ref mut t) =
                                                                                c.text
                                                                            {
                                                                                t.color_hex = hex;
                                                                            }
                                                                        }
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                        }),
                                                    ),
                                            ),
                                    )
                                    .child(self.inspector_slider(
                                        "Red",
                                        format!("{}", col_r),
                                        col_r as f32 / 255.0,
                                        rgb(0xef4444),
                                        SliderField::ColorR,
                                        cx,
                                    ))
                                    .child(self.inspector_slider(
                                        "Green",
                                        format!("{}", col_g),
                                        col_g as f32 / 255.0,
                                        rgb(0x22c55e),
                                        SliderField::ColorG,
                                        cx,
                                    ))
                                    .child(self.inspector_slider(
                                        "Blue",
                                        format!("{}", col_b),
                                        col_b as f32 / 255.0,
                                        rgb(0x3b82f6),
                                        SliderField::ColorB,
                                        cx,
                                    )),
                            )
                            // Effects: Stroke, Shadow, Background
                            .child(div().h(px(1.)).w_full().bg(Theme::border()))
                            .child(self.inspector_slider(
                                "Stroke",
                                format!("{:.1}px", td.stroke_width),
                                (td.stroke_width / 8.0).clamp(0.0, 1.0),
                                Theme::accent(),
                                SliderField::StrokeWidth,
                                cx,
                            ))
                            .when(td.stroke_width > 0.0, |el| {
                                let cur_sc = td.stroke_color_hex.unwrap_or(0x000000);
                                el.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(Theme::text_secondary())
                                                .child("Stroke Color"),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .children(
                                                    [
                                                        0x000000_u32,
                                                        0xffffff,
                                                        0xfacc15,
                                                        0xef4444,
                                                        0x3b82f6,
                                                        0xec4899,
                                                    ]
                                                    .into_iter()
                                                    .map(|hex| {
                                                        let active = cur_sc == hex;
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "sc-{:06x}",
                                                                hex
                                                            )))
                                                            .w(px(14.))
                                                            .h(px(14.))
                                                            .rounded_sm()
                                                            .bg(rgb(hex))
                                                            .border_1()
                                                            .border_color(if active {
                                                                Theme::accent()
                                                            } else {
                                                                Theme::border()
                                                            })
                                                            .cursor_pointer()
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.state.push_undo();
                                                                    if let Some(c) = this
                                                                        .state
                                                                        .selected_clip_mut()
                                                                    {
                                                                        if let Some(ref mut t) =
                                                                            c.text
                                                                        {
                                                                            t.stroke_color_hex =
                                                                                Some(hex);
                                                                        }
                                                                    }
                                                                    cx.notify();
                                                                },
                                                            ))
                                                    }),
                                                ),
                                        ),
                                )
                            })
                            .child(self.inspector_slider(
                                "Shadow",
                                if td.shadow_color_hex.is_some() {
                                    format!("{:.0}px", td.shadow_offset_x)
                                } else {
                                    "Off".into()
                                },
                                if td.shadow_color_hex.is_some() {
                                    (td.shadow_offset_x / 12.0).clamp(0.0, 1.0)
                                } else {
                                    0.0
                                },
                                Theme::accent(),
                                SliderField::ShadowOffset,
                                cx,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(Theme::text_secondary())
                                            .child("Background"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .children(
                                                [
                                                    (None, "Off"),
                                                    (Some(0x111115_u32), "Dark"),
                                                    (Some(0xef4444_u32), "Red"),
                                                    (Some(0x2563eb_u32), "Blue"),
                                                ]
                                                .into_iter()
                                                .map(|(bg_opt, label)| {
                                                    let active = td.bg_color_hex == bg_opt;
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "tbg-{}",
                                                            label
                                                        )))
                                                        .px_1p5()
                                                        .py_0p5()
                                                        .rounded_sm()
                                                        .bg(if active {
                                                            Theme::accent()
                                                        } else {
                                                            Theme::bg_surface()
                                                        })
                                                        .border_1()
                                                        .border_color(Theme::border())
                                                        .text_xs()
                                                        .text_color(Theme::text_primary())
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.state.push_undo();
                                                                if let Some(c) =
                                                                    this.state.selected_clip_mut()
                                                                {
                                                                    if let Some(ref mut t) = c.text
                                                                    {
                                                                        t.bg_color_hex = bg_opt;
                                                                    }
                                                                }
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child(label)
                                                }),
                                            ),
                                    ),
                            )
                            .when(td.bg_color_hex.is_some(), |el| {
                                el.child(self.inspector_slider(
                                    "Pad",
                                    format!("{:.0}px", td.bg_padding),
                                    ((td.bg_padding - 2.0) / 22.0).clamp(0.0, 1.0),
                                    Theme::accent(),
                                    SliderField::BgPadding,
                                    cx,
                                ))
                            })
                            // Text Animations
                            .child(div().h(px(1.)).w_full().bg(Theme::border()))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(Theme::text_secondary())
                                            .child("Animation"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_1()
                                            .children(TextAnimation::all().iter().map(|&anim| {
                                                let active = td.animation == anim;
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "ta-{}",
                                                        anim.label()
                                                    )))
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .bg(if active {
                                                        Theme::accent()
                                                    } else {
                                                        Theme::bg_surface()
                                                    })
                                                    .border_1()
                                                    .border_color(Theme::border())
                                                    .text_xs()
                                                    .text_color(Theme::text_primary())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.state.push_undo();
                                                        if let Some(c) =
                                                            this.state.selected_clip_mut()
                                                        {
                                                            if let Some(ref mut t) = c.text {
                                                                t.animation = anim;
                                                            }
                                                        }
                                                        cx.notify();
                                                    }))
                                                    .child(anim.label())
                                            })),
                                    ),
                            )
                            .when(td.animation != TextAnimation::None, |el| {
                                el.child(self.inspector_slider(
                                    "Anim Dur",
                                    format!("{:.1}s", td.anim_duration_secs),
                                    ((td.anim_duration_secs - 0.1) / 1.9).clamp(0.0, 1.0),
                                    Theme::accent(),
                                    SliderField::AnimDuration,
                                    cx,
                                ))
                            })
                    })
            } else {
                div()
                    .id("inspector-empty")
                    .p_4()
                    .text_xs()
                    .text_color(Theme::text_muted())
                    .child("Select a clip on the timeline to inspect, drag, or trim its edges.")
            })
    }

    fn inspector_slider(
        &self,
        label: &'static str,
        value: String,
        ratio: f32,
        fill_color: gpui::Rgba,
        field: SliderField,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let clamped = ratio.clamp(0.0, 1.0);
        let fill_w = SLIDER_W * clamped;
        let thumb_x = (fill_w - 5.0).clamp(0.0, SLIDER_W - 10.0);

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .w(px(56.))
                    .text_xs()
                    .text_color(Theme::text_secondary())
                    .child(label),
            )
            .child(
                div()
                    .id(SharedString::from(format!("sld-{}", label)))
                    .w(px(SLIDER_W))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .relative()
                    .cursor_ew_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.state.push_undo();
                            this.slider_drag = Some(field);
                            let slider_left =
                                f32::from(window.viewport_size().width) - 62.0 - SLIDER_W;
                            let r = (f32::from(event.position.x) - slider_left) / SLIDER_W;
                            this.apply_slider_ratio(field, r);
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(6.))
                            .rounded_full()
                            .bg(Theme::bg_surface())
                            .border_1()
                            .border_color(Theme::border())
                            .overflow_hidden()
                            .child(
                                div()
                                    .w(px(fill_w))
                                    .h_full()
                                    .bg(fill_color),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(thumb_x))
                            .w(px(10.))
                            .h(px(10.))
                            .rounded_full()
                            .bg(Theme::text_primary())
                            .border_1()
                            .border_color(Theme::bg_app()),
                    ),
            )
            .child(
                div()
                    .w(px(42.))
                    .text_xs()
                    .text_right()
                    .text_color(Theme::text_primary())
                    .child(value),
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
            .w(px(20.))
            .h(px(20.))
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
        let is_linked = self
            .state
            .selected_clip()
            .and_then(|c| c.linked_clip_id)
            .is_some();
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
        let tracks = self.state.tracks.clone();
        let markers = self.state.markers.clone();

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
                            .child(
                                self.icon_button(
                                    "tl-link",
                                    "🔗",
                                    if is_linked {
                                        "Unlink Clip"
                                    } else {
                                        "Link with Overlapping Clip"
                                    },
                                    is_linked,
                                    selected_clip_id.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let Some(sel_id) = this.state.selected_clip_id else {
                                        return;
                                    };
                                    if this
                                        .state
                                        .selected_clip()
                                        .and_then(|c| c.linked_clip_id)
                                        .is_some()
                                    {
                                        this.state.unlink_clip(sel_id);
                                    } else if let Some(sel) = this.state.selected_clip().cloned() {
                                        if let Some(partner_id) = this
                                            .state
                                            .clips
                                            .iter()
                                            .find(|c| {
                                                c.id != sel_id
                                                    && c.track != sel.track
                                                    && c.start_secs < sel.end_secs()
                                                    && c.end_secs() > sel.start_secs
                                            })
                                            .map(|c| c.id)
                                        {
                                            this.state.link_clips(sel_id, partner_id);
                                        } else {
                                            this.state.status_message =
                                                "No overlapping clip on another track to link"
                                                    .into();
                                        }
                                    }
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
                            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(Theme::border()))
                            .child(
                                self.icon_button(
                                    "tl-mirror",
                                    "⇄",
                                    "Mirror Horizontally",
                                    self.state.selected_clip().map(|c| c.flip_h).unwrap_or(false),
                                    selected_clip_id.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.state.toggle_mirror_selected() {
                                        this.sync_preview_after_seek(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-rot90",
                                    "↻",
                                    "Rotate 90°",
                                    false,
                                    selected_clip_id.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.state.rotate_selected_90() {
                                        this.sync_preview_after_seek(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-freeze",
                                    "❄",
                                    "Freeze Frame at Playhead",
                                    false,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let Some(vclip) = this
                                        .state
                                        .active_clips_at_playhead()
                                        .into_iter()
                                        .find(|c| c.track == TrackKind::Video)
                                        .cloned()
                                    else {
                                        this.state.status_message =
                                            "Place playhead over a video clip to freeze".into();
                                        cx.notify();
                                        return;
                                    };
                                    let Some(path) = vclip.source_path else {
                                        return;
                                    };
                                    let offset = vclip.source_in_secs
                                        + (this.state.playhead_secs - vclip.start_secs).max(0.0);
                                    if let Some(img_path) = export_freeze_frame(&path, offset) {
                                        this.state.insert_freeze_frame_at_playhead(img_path, 3.0);
                                        this.sync_preview_after_seek(cx);
                                    } else {
                                        this.state.status_message =
                                            "Failed to capture freeze frame".into();
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.icon_button(
                                    "tl-kf",
                                    "◆",
                                    "Add / Remove Keyframe at Playhead",
                                    false,
                                    selected_clip_id.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.toggle_keyframe_at_playhead();
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
                                    .justify_between()
                                    .text_xs()
                                    .text_color(Theme::text_muted())
                                    .border_b_1()
                                    .border_color(Theme::border())
                                    .child("Tracks")
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .children(
                                                [
                                                    (TrackKind::Video, "🎬", "Add Video Track"),
                                                    (
                                                        TrackKind::Overlay,
                                                        "🖼",
                                                        "Add Overlay/Image Track",
                                                    ),
                                                    (TrackKind::Audio, "🎵", "Add Audio Track"),
                                                ]
                                                .into_iter()
                                                .map(|(kind, icon, tip)| {
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "add-tr-{}",
                                                            kind.label()
                                                        )))
                                                        .w(px(20.))
                                                        .h(px(20.))
                                                        .rounded_sm()
                                                        .bg(Theme::bg_surface())
                                                        .border_1()
                                                        .border_color(Theme::border())
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .text_xs()
                                                        .text_color(Theme::text_secondary())
                                                        .cursor_pointer()
                                                        .hover(|s| {
                                                            s.bg(Theme::bg_hover())
                                                                .text_color(Theme::text_primary())
                                                        })
                                                        .tooltip(build_tooltip(tip))
                                                        .on_click(cx.listener(move |this, _, _, cx| {
                                                            this.state.add_track(kind);
                                                            cx.notify();
                                                        }))
                                                        .child(icon)
                                                }),
                                            ),
                                    ),
                            )
                            .children(tracks.iter().map(|track| {
                                let track_id = track.id;
                                let t = track.kind;
                                let flags = track.flags;
                                let track_name = track.name.clone();
                                let is_main = track.is_main;

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
                                                    .child(track_name),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .when(t.is_audible(), |el| {
                                                el.child(
                                                    self.track_flag_btn(
                                                        SharedString::from(format!(
                                                            "mute-{}",
                                                            track_id
                                                        )),
                                                        if flags.muted { "🔇" } else { "🔊" },
                                                        if flags.muted {
                                                            "Unmute Track (Muted)"
                                                        } else {
                                                            "Mute Track (Audio On)"
                                                        },
                                                        flags.muted,
                                                    )
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.state.toggle_mute_track(track_id);
                                                        this.sync_preview_after_seek(cx);
                                                        cx.notify();
                                                    })),
                                                )
                                            })
                                            .when(t.is_visual(), |el| {
                                                el.child(
                                                    self.track_flag_btn(
                                                        SharedString::from(format!(
                                                            "hide-{}",
                                                            track_id
                                                        )),
                                                        if flags.hidden { "🚫" } else { "👁" },
                                                        if flags.hidden {
                                                            "Show Track (Hidden)"
                                                        } else {
                                                            "Hide Track (Visible)"
                                                        },
                                                        flags.hidden,
                                                    )
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.state.toggle_hide_track(track_id);
                                                        this.sync_preview_after_seek(cx);
                                                        cx.notify();
                                                    })),
                                                )
                                            })
                                            .child(
                                                self.track_flag_btn(
                                                    SharedString::from(format!(
                                                        "lock-{}",
                                                        track_id
                                                    )),
                                                    if flags.locked { "🔒" } else { "🔓" },
                                                    if flags.locked {
                                                        "Unlock Track (Locked)"
                                                    } else {
                                                        "Lock Track (Unlocked)"
                                                    },
                                                    flags.locked,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.state.toggle_lock_track(track_id);
                                                    cx.notify();
                                                })),
                                            )
                                            .when(!is_main, |el| {
                                                el.child(
                                                    self.track_flag_btn(
                                                        SharedString::from(format!(
                                                            "rm-track-{}",
                                                            track_id
                                                        )),
                                                        "🗑",
                                                        "Remove Track",
                                                        false,
                                                    )
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        let _ = this.state.remove_track(track_id);
                                                        this.sync_preview_after_seek(cx);
                                                        cx.notify();
                                                    })),
                                                )
                                            }),
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
                                            }))
                                            .children(markers.into_iter().map(|m| {
                                                let m_id = m.id;
                                                let m_time = m.time_secs;
                                                let left = m_time * px_per_sec;
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "marker-{}",
                                                        m_id
                                                    )))
                                                    .absolute()
                                                    .left(px(left - 6.0))
                                                    .top(px(2.))
                                                    .px_1()
                                                    .rounded_sm()
                                                    .bg(rgb(m.color_hex))
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .text_color(Theme::bg_app())
                                                    .cursor_pointer()
                                                    .tooltip(build_tooltip(
                                                        "Click: Jump to Marker • Shift+Click: Remove",
                                                    ))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(
                                                            move |this,
                                                                  event: &MouseDownEvent,
                                                                  _,
                                                                  cx| {
                                                                cx.stop_propagation();
                                                                if event.modifiers.shift {
                                                                    this.state.remove_marker(m_id);
                                                                } else {
                                                                    this.state.seek_to(m_time);
                                                                    this.sync_preview_after_seek(cx);
                                                                }
                                                                cx.notify();
                                                            },
                                                        ),
                                                    )
                                                    .child("⚑")
                                            })),
                                    )
                                    // Track lanes
                                    .children(tracks.iter().map(|track| {
                                        let track_id = track.id;
                                        let track_kind = track.kind;
                                        let track_clips: Vec<_> = self
                                            .state
                                            .clips
                                            .iter()
                                            .filter(|c| {
                                                c.track_id == track_id
                                                    || (c.track_id == 0 && c.track == track_kind)
                                            })
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
                                                    // Clip title & keyframe diamonds
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .h_full()
                                                            .relative()
                                                            .flex()
                                                            .items_center()
                                                            .px_1()
                                                            .overflow_hidden()
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .font_weight(
                                                                        gpui::FontWeight::MEDIUM,
                                                                    )
                                                                    .text_color(
                                                                        Theme::text_primary(),
                                                                    )
                                                                    .child(clip.name),
                                                            )
                                                            .children(clip.keyframes.into_iter().map(
                                                                |kf| {
                                                                    let kx = (kf.time_offset_secs
                                                                        * px_per_sec
                                                                        - 10.0)
                                                                        .max(0.0);
                                                                    div()
                                                                        .absolute()
                                                                        .left(px(kx))
                                                                        .bottom(px(1.))
                                                                        .text_xs()
                                                                        .text_color(rgb(0xfacc15))
                                                                        .child("◆")
                                                                },
                                                            )),
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
                if let Some(field) = this.slider_drag {
                    let slider_left = f32::from(window.viewport_size().width) - 62.0 - SLIDER_W;
                    let r = (f32::from(event.position.x) - slider_left) / SLIDER_W;
                    this.apply_slider_ratio(field, r);
                    if matches!(
                        field,
                        SliderField::Rotation
                            | SliderField::FontSize
                            | SliderField::FontWeight
                            | SliderField::ColorR
                            | SliderField::ColorG
                            | SliderField::ColorB
                            | SliderField::StrokeWidth
                            | SliderField::ShadowOffset
                            | SliderField::BgPadding
                    ) {
                        let now = Instant::now();
                        let due = this
                            .last_rotate_sync
                            .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(35));
                        if due {
                            this.last_rotate_sync = Some(now);
                            if field == SliderField::Rotation {
                                this.sync_preview_after_seek(cx);
                            } else {
                                this.sync_rotated_texts(cx);
                            }
                        }
                    }
                    cx.notify();
                    return;
                }

                if let Some(mut pdrag) = this.preview_text_drag {
                    let cur_x = f32::from(event.position.x);
                    let cur_y = f32::from(event.position.y);
                    let dx = cur_x - pdrag.start_mouse_x;
                    let dy = cur_y - pdrag.start_mouse_y;
                    if dx.abs() > 2.0 || dy.abs() > 2.0 {
                        pdrag.moved = true;
                        this.preview_text_drag = Some(pdrag);
                    }
                    let inv = if pdrag.preview_scale > 0.001 {
                        1.0 / pdrag.preview_scale
                    } else {
                        1.0
                    };
                    let mut rotate_sync_needed = false;
                    let (out_w, out_h) = this
                        .state
                        .aspect_ratio
                        .dimensions(this.state.base_height);
                    let cw = out_w as f32;
                    let ch = out_h as f32;
                    let (half_w, half_h) = this.clip_canvas_half_extents(pdrag.clip_id);
                    let mut next_guides = PreviewSnapGuides::default();

                    let playhead = this.state.playhead_secs;
                    if let Some(c) = this.state.clips.iter_mut().find(|c| c.id == pdrag.clip_id) {
                        if !c.keyframes.is_empty() {
                            let (epx, epy, esc, erot, eop) = c.evaluate_at(playhead);
                            c.pos_x = epx;
                            c.pos_y = epy;
                            c.scale = esc;
                            c.rotation_deg = erot;
                            c.opacity = eop;
                        }
                        match pdrag.kind {
                            PreviewTransformKind::Move => {
                                let mut nx = pdrag.initial_pos_x + dx * inv;
                                let mut ny = pdrag.initial_pos_y + dy * inv;
                                if !event.modifiers.alt {
                                    let tol = 8.0 * inv;
                                    let left_pos = -(cw * 0.5 - half_w);
                                    let right_pos = cw * 0.5 - half_w;
                                    let sleft_pos = -(cw * 0.42 - half_w);
                                    let sright_pos = cw * 0.42 - half_w;
                                    if nx.abs() <= tol {
                                        nx = 0.0;
                                        next_guides.center_x = true;
                                    } else if (nx - left_pos).abs() <= tol {
                                        nx = left_pos;
                                        next_guides.left_edge = true;
                                    } else if (nx - right_pos).abs() <= tol {
                                        nx = right_pos;
                                        next_guides.right_edge = true;
                                    } else if (nx - sleft_pos).abs() <= tol {
                                        nx = sleft_pos;
                                        next_guides.safe_left = true;
                                    } else if (nx - sright_pos).abs() <= tol {
                                        nx = sright_pos;
                                        next_guides.safe_right = true;
                                    }

                                    let top_pos = -(ch * 0.5 - half_h);
                                    let bot_pos = ch * 0.5 - half_h;
                                    let stop_pos = -(ch * 0.42 - half_h);
                                    let sbot_pos = ch * 0.42 - half_h;
                                    if ny.abs() <= tol {
                                        ny = 0.0;
                                        next_guides.center_y = true;
                                    } else if (ny - top_pos).abs() <= tol {
                                        ny = top_pos;
                                        next_guides.top_edge = true;
                                    } else if (ny - bot_pos).abs() <= tol {
                                        ny = bot_pos;
                                        next_guides.bottom_edge = true;
                                    } else if (ny - stop_pos).abs() <= tol {
                                        ny = stop_pos;
                                        next_guides.safe_top = true;
                                    } else if (ny - sbot_pos).abs() <= tol {
                                        ny = sbot_pos;
                                        next_guides.safe_bottom = true;
                                    }
                                }
                                c.pos_x = nx;
                                c.pos_y = ny;
                            }
                            PreviewTransformKind::ResizeCorner => {
                                let d_init = ((pdrag.start_mouse_x - pdrag.center_x).powi(2)
                                    + (pdrag.start_mouse_y - pdrag.center_y).powi(2))
                                .sqrt()
                                .max(12.0);
                                let d_cur = ((cur_x - pdrag.center_x).powi(2)
                                    + (cur_y - pdrag.center_y).powi(2))
                                .sqrt()
                                .max(6.0);
                                let factor = (d_cur / d_init).clamp(0.2, 4.0);
                                if c.track == TrackKind::Text && c.keyframes.is_empty() {
                                    if let Some(ref mut t) = c.text {
                                        t.font_size =
                                            (pdrag.initial_font_size * factor).clamp(12.0, 140.0);
                                    }
                                    rotate_sync_needed = true;
                                } else {
                                    c.scale = ((pdrag.initial_scale as f32) * factor)
                                        .round()
                                        .clamp(10.0, 200.0)
                                        as u16;
                                    if c.track == TrackKind::Text {
                                        rotate_sync_needed = true;
                                    }
                                }
                            }
                            PreviewTransformKind::Rotate => {
                                let a0 = (pdrag.start_mouse_y - pdrag.center_y)
                                    .atan2(pdrag.start_mouse_x - pdrag.center_x);
                                let a1 = (cur_y - pdrag.center_y).atan2(cur_x - pdrag.center_x);
                                let delta_deg = (a1 - a0).to_degrees();
                                let (snapped_rot, snapped_to) = snap_rotation_angle(
                                    pdrag.initial_rot + delta_deg,
                                    event.modifiers.shift,
                                );
                                c.rotation_deg = snapped_rot;
                                this.state.status_message = if snapped_to.is_some() {
                                    format!("Rotate: {:.0}° (Snap)", c.rotation_deg)
                                } else {
                                    format!("Rotate: {:.0}°", c.rotation_deg)
                                };
                                rotate_sync_needed =
                                    c.track == TrackKind::Video || c.track == TrackKind::Text;
                            }
                        }
                        c.sync_keyframe_if_animated(playhead);
                    }
                    this.active_snap_guides = next_guides;
                    if rotate_sync_needed {
                        let now = Instant::now();
                        let due = this
                            .last_rotate_sync
                            .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(35));
                        if due {
                            this.last_rotate_sync = Some(now);
                            this.sync_preview_after_seek(cx);
                        }
                    }
                    cx.notify();
                    return;
                }

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
                    if let Some(sf) = this.slider_drag.take() {
                        if matches!(
                            sf,
                            SliderField::CropLeft
                                | SliderField::CropRight
                                | SliderField::CropTop
                                | SliderField::CropBottom
                                | SliderField::Rotation
                                | SliderField::FontSize
                                | SliderField::FontWeight
                                | SliderField::ColorR
                                | SliderField::ColorG
                                | SliderField::ColorB
                                | SliderField::StrokeWidth
                                | SliderField::ShadowOffset
                                | SliderField::BgPadding
                                | SliderField::Volume
                                | SliderField::FadeIn
                                | SliderField::FadeOut
                                | SliderField::Speed
                        ) {
                            this.sync_preview_after_seek(cx);
                        }
                        changed = true;
                    }
                    if let Some(pdrag) = this.preview_text_drag.take() {
                        this.active_snap_guides = PreviewSnapGuides::default();
                        if matches!(
                            pdrag.kind,
                            PreviewTransformKind::Rotate | PreviewTransformKind::ResizeCorner
                        ) {
                            this.sync_preview_after_seek(cx);
                        }
                        changed = true;
                    }
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
            .when(!self.preview_fullscreen, |el| {
                el.child(self.render_header(cx))
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
            })
            .when(self.preview_fullscreen, |el| {
                el.child(self.render_preview_panel(window, cx))
            })
            .when(self.export_modal_open, |el| {
                el.child(self.render_export_modal(cx))
            })
            .when(self.menu_open, |el| {
                el.child(self.render_menu_dropdown(cx))
            })
            .into_any_element()
    }
}

fn utf16_offset_to_utf8_idx(s: &str, utf16_off: usize) -> usize {
    let mut u16_count = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if u16_count >= utf16_off {
            return byte_idx;
        }
        u16_count += ch.len_utf16();
    }
    s.len()
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let edit_id = self.editing_text_clip_id?;
        let clip = self.state.clips.iter().find(|c| c.id == edit_id)?;
        let content = &clip.text.as_ref()?.content;
        let total_u16: usize = content.encode_utf16().count();
        let start = range_utf16.start.min(total_u16);
        let end = range_utf16.end.min(total_u16).max(start);
        *adjusted_range = Some(start..end);
        let b_start = utf16_offset_to_utf8_idx(content, start);
        let b_end = utf16_offset_to_utf8_idx(content, end);
        Some(content[b_start..b_end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let edit_id = self.editing_text_clip_id?;
        let clip = self.state.clips.iter().find(|c| c.id == edit_id)?;
        let len_u16: usize = clip.text.as_ref()?.content.encode_utf16().count();
        Some(UTF16Selection {
            range: len_u16..len_u16,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text_range.clone()
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_text_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit_id) = self.editing_text_clip_id else {
            return;
        };
        let Some(clip) = self.state.clips.iter_mut().find(|c| c.id == edit_id) else {
            return;
        };
        let Some(ref mut td) = clip.text else {
            return;
        };

        let replace_range = range_utf16
            .or_else(|| self.marked_text_range.take())
            .unwrap_or_else(|| {
                let len_u16: usize = td.content.encode_utf16().count();
                len_u16..len_u16
            });

        let b_start = utf16_offset_to_utf8_idx(&td.content, replace_range.start);
        let b_end = utf16_offset_to_utf8_idx(&td.content, replace_range.end).max(b_start);
        td.content.replace_range(b_start..b_end, text);
        clip.name = if td.content.is_empty() {
            "Text".into()
        } else {
            td.content.clone()
        };
        self.marked_text_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit_id) = self.editing_text_clip_id else {
            return;
        };
        let Some(clip) = self.state.clips.iter_mut().find(|c| c.id == edit_id) else {
            return;
        };
        let Some(ref mut td) = clip.text else {
            return;
        };

        let replace_range = range_utf16
            .or_else(|| self.marked_text_range.clone())
            .unwrap_or_else(|| {
                let len_u16: usize = td.content.encode_utf16().count();
                len_u16..len_u16
            });

        let b_start = utf16_offset_to_utf8_idx(&td.content, replace_range.start);
        let b_end = utf16_offset_to_utf8_idx(&td.content, replace_range.end).max(b_start);
        td.content.replace_range(b_start..b_end, new_text);
        clip.name = if td.content.is_empty() {
            "Text".into()
        } else {
            td.content.clone()
        };

        if !new_text.is_empty() {
            let new_u16_len: usize = new_text.encode_utf16().count();
            self.marked_text_range =
                Some(replace_range.start..(replace_range.start + new_u16_len));
        } else {
            self.marked_text_range = None;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
