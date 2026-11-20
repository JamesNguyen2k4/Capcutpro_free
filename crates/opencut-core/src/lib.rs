pub mod models;

use std::path::{Path, PathBuf};

pub use models::*;

pub struct EditorCore {
    pub project_name: String,
    pub project_path: Option<PathBuf>,
    pub aspect_ratio: AspectRatio,
    pub base_height: u32,
    pub fps: u32,
    pub main_track_magnet: bool,
    pub assets: Vec<MediaAsset>,
    pub clips: Vec<TimelineClip>,
    pub tracks: Vec<Track>,
    pub track_flags: [TrackFlags; 3],
    pub markers: Vec<TimelineMarker>,
    pub selected_asset_id: Option<usize>,
    pub selected_clip_id: Option<usize>,
    pub playhead_secs: f32,
    pub total_duration_secs: f32,
    pub is_playing: bool,
    pub px_per_sec: f32,
    pub status_message: String,
    pub snap_enabled: bool,
    pub next_id: usize,
    pub copied_clip: Option<TimelineClip>,
    undo_stack: Vec<ProjectSnapshot>,
    redo_stack: Vec<ProjectSnapshot>,
}

impl Default for EditorCore {
    fn default() -> Self {
        Self::new()
    }
}

fn default_tracks() -> Vec<Track> {
    vec![
        Track {
            id: 1,
            kind: TrackKind::Overlay,
            name: "V2".into(),
            flags: TrackFlags::default(),
            is_main: false,
        },
        Track {
            id: 2,
            kind: TrackKind::Video,
            name: "V1".into(),
            flags: TrackFlags::default(),
            is_main: true,
        },
        Track {
            id: 3,
            kind: TrackKind::Audio,
            name: "A1".into(),
            flags: TrackFlags::default(),
            is_main: false,
        },
    ]
}

impl EditorCore {
    pub fn new() -> Self {
        Self {
            project_name: "Untitled Project".into(),
            project_path: None,
            aspect_ratio: AspectRatio::Ratio16x9,
            base_height: 720,
            fps: 30,
            main_track_magnet: false,
            assets: Vec::new(),
            clips: Vec::new(),
            tracks: default_tracks(),
            track_flags: [TrackFlags::default(); 3],
            markers: Vec::new(),
            selected_asset_id: None,
            selected_clip_id: None,
            playhead_secs: 0.0,
            total_duration_secs: 30.0,
            is_playing: false,
            px_per_sec: 32.0,
            status_message: "Drop or import media to begin".into(),
            snap_enabled: true,
            next_id: 10,
            copied_clip: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot {
            project_name: self.project_name.clone(),
            aspect_ratio: self.aspect_ratio,
            base_height: self.base_height,
            fps: self.fps,
            main_track_magnet: self.main_track_magnet,
            assets: self.assets.clone(),
            clips: self.clips.clone(),
            tracks: self.tracks.clone(),
            track_flags: self.track_flags,
            markers: self.markers.clone(),
            total_duration_secs: self.total_duration_secs,
            next_id: self.next_id,
        }
    }

    fn sync_legacy_track_flags(&mut self) {
        for kind in [TrackKind::Overlay, TrackKind::Video, TrackKind::Audio] {
            if let Some(t) = self.tracks.iter().find(|t| t.kind == kind) {
                self.track_flags[kind.index()] = t.flags;
            }
        }
    }

    fn apply_snapshot(&mut self, mut snap: ProjectSnapshot) {
        if snap.tracks.is_empty() {
            snap.tracks = default_tracks();
            for t in &mut snap.tracks {
                if t.kind.index() < 3 {
                    t.flags = snap.track_flags[t.kind.index()];
                }
            }
        }

        // Backfill track_id for legacy snapshots
        for clip in &mut snap.clips {
            if clip.track_id == 0 {
                if let Some(t) = snap.tracks.iter().find(|t| t.kind == clip.track) {
                    clip.track_id = t.id;
                }
            }
        }

        self.project_name = snap.project_name;
        self.aspect_ratio = snap.aspect_ratio;
        self.base_height = if snap.base_height == 0 {
            720
        } else {
            snap.base_height
        };
        self.fps = if snap.fps == 0 { 30 } else { snap.fps };
        self.main_track_magnet = snap.main_track_magnet;
        self.assets = snap.assets;
        self.clips = snap.clips;
        self.tracks = snap.tracks;
        self.track_flags = snap.track_flags;
        self.markers = snap.markers;
        self.total_duration_secs = snap.total_duration_secs;
        self.next_id = snap.next_id.max(10);
        self.sync_legacy_track_flags();

        if let Some(sel) = self.selected_clip_id {
            if !self.clips.iter().any(|c| c.id == sel) {
                self.selected_clip_id = None;
            }
        }
    }

    pub fn push_undo(&mut self) {
        const MAX_UNDO: usize = 50;
        let snap = self.snapshot();
        self.undo_stack.push(snap);
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            self.status_message = "Nothing to undo".into();
            return false;
        };
        let current = self.snapshot();
        self.redo_stack.push(current);
        self.apply_snapshot(prev);
        self.status_message = "Undid last action".into();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            self.status_message = "Nothing to redo".into();
            return false;
        };
        let current = self.snapshot();
        self.undo_stack.push(current);
        self.apply_snapshot(next);
        self.status_message = "Redid action".into();
        true
    }

    pub fn save_to_file(&mut self, path: &Path) -> Result<(), String> {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            self.project_name = stem.to_string();
        }
        let json = serde_json::to_string_pretty(&self.snapshot())
            .map_err(|e| format!("Serialize error: {}", e))?;
        std::fs::write(path, json).map_err(|e| format!("Write error: {}", e))?;
        self.project_path = Some(path.to_path_buf());
        self.status_message = format!("Saved {}", path.display());
        Ok(())
    }

    pub fn load_from_file(&mut self, path: &Path) -> Result<(), String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;
        let snap: ProjectSnapshot =
            serde_json::from_str(&content).map_err(|e| format!("Invalid project file: {}", e))?;
        self.apply_snapshot(snap);
        self.project_path = Some(path.to_path_buf());
        self.playhead_secs = 0.0;
        self.is_playing = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.status_message = format!("Opened {}", self.project_name);
        Ok(())
    }

    pub fn set_project_name(&mut self, name: impl Into<String>) {
        let trimmed = name.into().trim().to_string();
        if !trimmed.is_empty() {
            self.push_undo();
            self.project_name = trimmed;
        }
    }

    pub fn set_aspect_ratio(&mut self, ratio: AspectRatio) {
        if self.aspect_ratio != ratio {
            self.push_undo();
            self.aspect_ratio = ratio;
            self.status_message = format!("Aspect ratio: {}", ratio.label());
        }
    }

    pub fn set_base_height(&mut self, height: u32) {
        let clamped = height.clamp(360, 2160);
        if self.base_height != clamped {
            self.push_undo();
            self.base_height = clamped;
            let (w, h) = self.aspect_ratio.dimensions(clamped);
            self.status_message = format!("Resolution: {}x{}", w, h);
        }
    }

    pub fn set_fps(&mut self, fps: u32) {
        let clamped = fps.clamp(15, 120);
        if self.fps != clamped {
            self.push_undo();
            self.fps = clamped;
            self.status_message = format!("Project FPS: {}", clamped);
        }
    }

    pub fn default_track_id(&self, kind: TrackKind) -> usize {
        self.tracks
            .iter()
            .find(|t| t.kind == kind)
            .map(|t| t.id)
            .unwrap_or(0)
    }

    pub fn track_by_id(&self, track_id: usize) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == track_id)
    }

    pub fn add_track(&mut self, kind: TrackKind) -> usize {
        self.push_undo();
        let id = self.next_id;
        self.next_id += 1;
        let count = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let name = format!("{}{}", kind.prefix(), count);
        let track = Track {
            id,
            kind,
            name: name.clone(),
            flags: TrackFlags::default(),
            is_main: false,
        };

        // Keep audio tracks at bottom, visual/overlay/text tracks above main video track
        if kind == TrackKind::Audio {
            self.tracks.push(track);
        } else {
            self.tracks.insert(0, track);
        }
        self.status_message = format!("Added track {}", name);
        id
    }

    pub fn remove_track(&mut self, track_id: usize) -> Result<(), String> {
        let Some(pos) = self.tracks.iter().position(|t| t.id == track_id) else {
            return Err("Track not found".into());
        };
        if self.tracks[pos].is_main {
            return Err("Cannot remove main video track".into());
        }

        self.push_undo();
        let removed = self.tracks.remove(pos);
        self.clips.retain(|c| c.track_id != track_id);
        if let Some(sel) = self.selected_clip_id {
            if !self.clips.iter().any(|c| c.id == sel) {
                self.selected_clip_id = None;
            }
        }
        self.sync_legacy_track_flags();
        self.status_message = format!("Removed track {}", removed.name);
        Ok(())
    }

    pub fn track_flags_by_id(&self, track_id: usize) -> TrackFlags {
        self.tracks
            .iter()
            .find(|t| t.id == track_id)
            .map(|t| t.flags)
            .unwrap_or_default()
    }

    pub fn track_flags(&self, track: TrackKind) -> TrackFlags {
        self.tracks
            .iter()
            .find(|t| t.kind == track)
            .map(|t| t.flags)
            .unwrap_or_else(|| {
                if track.index() < 3 {
                    self.track_flags[track.index()]
                } else {
                    TrackFlags::default()
                }
            })
    }

    fn is_clip_track_locked(&self, clip: &TimelineClip) -> bool {
        if clip.track_id != 0 {
            self.track_flags_by_id(clip.track_id).locked
        } else {
            self.track_flags(clip.track).locked
        }
    }

    fn is_clip_track_hidden(&self, clip: &TimelineClip) -> bool {
        if clip.track_id != 0 {
            self.track_flags_by_id(clip.track_id).hidden
        } else {
            self.track_flags(clip.track).hidden
        }
    }

    fn is_clip_track_muted(&self, clip: &TimelineClip) -> bool {
        if clip.track_id != 0 {
            self.track_flags_by_id(clip.track_id).muted
        } else {
            self.track_flags(clip.track).muted
        }
    }

    pub fn toggle_mute_track(&mut self, track_id: usize) {
        self.push_undo();
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.flags.muted = !t.flags.muted;
        }
        self.sync_legacy_track_flags();
    }

    pub fn toggle_hide_track(&mut self, track_id: usize) {
        self.push_undo();
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.flags.hidden = !t.flags.hidden;
        }
        self.sync_legacy_track_flags();
    }

    pub fn toggle_lock_track(&mut self, track_id: usize) {
        self.push_undo();
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.flags.locked = !t.flags.locked;
        }
        self.sync_legacy_track_flags();
    }

    pub fn toggle_mute(&mut self, track: TrackKind) {
        if let Some(id) = self.tracks.iter().find(|t| t.kind == track).map(|t| t.id) {
            self.toggle_mute_track(id);
        }
    }

    pub fn toggle_hide(&mut self, track: TrackKind) {
        if let Some(id) = self.tracks.iter().find(|t| t.kind == track).map(|t| t.id) {
            self.toggle_hide_track(id);
        }
    }

    pub fn toggle_lock(&mut self, track: TrackKind) {
        if let Some(id) = self.tracks.iter().find(|t| t.kind == track).map(|t| t.id) {
            self.toggle_lock_track(id);
        }
    }

    pub fn toggle_main_track_magnet(&mut self) {
        self.push_undo();
        self.main_track_magnet = !self.main_track_magnet;
        if self.main_track_magnet {
            self.compact_main_track();
        }
        self.status_message = if self.main_track_magnet {
            "Main track magnet enabled".into()
        } else {
            "Main track magnet disabled".into()
        };
    }

    pub fn compact_main_track(&mut self) {
        let Some(main_track_id) = self.tracks.iter().find(|t| t.is_main).map(|t| t.id) else {
            return;
        };
        let mut indices: Vec<usize> = self
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| c.track_id == main_track_id)
            .map(|(i, _)| i)
            .collect();
        indices.sort_by(|&a, &b| {
            self.clips[a]
                .start_secs
                .partial_cmp(&self.clips[b].start_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut cursor = 0.0_f32;
        for idx in indices {
            self.clips[idx].start_secs = cursor;
            cursor += self.clips[idx].duration_secs;
        }
    }

    pub fn selected_clip(&self) -> Option<&TimelineClip> {
        let id = self.selected_clip_id?;
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn selected_clip_mut(&mut self) -> Option<&mut TimelineClip> {
        let id = self.selected_clip_id?;
        self.clips.iter_mut().find(|c| c.id == id)
    }

    pub fn active_clips_at_playhead(&self) -> Vec<&TimelineClip> {
        self.clips
            .iter()
            .filter(|c| !self.is_clip_track_hidden(c) && c.contains(self.playhead_secs))
            .collect()
    }

    pub fn active_video_clip(&self) -> Option<&TimelineClip> {
        self.clips
            .iter()
            .filter(|c| {
                c.track != TrackKind::Audio
                    && c.track != TrackKind::Text
                    && c.track != TrackKind::Adjustment
                    && !self.is_clip_track_hidden(c)
                    && c.contains(self.playhead_secs)
            })
            .max_by_key(|c| match c.track {
                TrackKind::Overlay => 2,
                TrackKind::Video => 1,
                _ => 0,
            })
    }

    pub fn active_audio_clips_at(&self, time_secs: f32) -> Vec<&TimelineClip> {
        self.clips
            .iter()
            .filter(|c| {
                c.track.is_audible()
                    && !self.is_clip_track_muted(c)
                    && c.volume > 0
                    && c.contains(time_secs)
            })
            .collect()
    }

    pub fn seek_to(&mut self, secs: f32) {
        self.playhead_secs = secs.clamp(0.0, self.total_duration_secs);
    }

    pub fn step_by(&mut self, delta: f32) {
        self.seek_to(self.playhead_secs + delta);
    }

    pub fn step_frames(&mut self, frames: i32) {
        let frame_dur = 1.0 / (self.fps.max(1) as f32);
        self.seek_to(self.playhead_secs + (frames as f32) * frame_dur);
    }

    pub fn toggle_play(&mut self) {
        self.is_playing = !self.is_playing;
        if self.is_playing && self.playhead_secs >= self.total_duration_secs {
            self.playhead_secs = 0.0;
        }
        self.status_message = if self.is_playing {
            "Playing".into()
        } else {
            "Paused".into()
        };
    }

    pub fn tick(&mut self, dt_secs: f32) {
        if !self.is_playing {
            return;
        }
        self.playhead_secs += dt_secs;
        if self.playhead_secs >= self.total_duration_secs {
            self.playhead_secs = self.total_duration_secs;
            self.is_playing = false;
            self.status_message = "Reached end of timeline".into();
        }
    }

    pub fn rename_asset(&mut self, asset_id: usize, new_name: impl Into<String>) {
        let trimmed = new_name.into().trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        if self.assets.iter().any(|a| a.id == asset_id) {
            self.push_undo();
            if let Some(asset) = self.assets.iter_mut().find(|a| a.id == asset_id) {
                asset.name = trimmed.clone();
            }
            self.status_message = format!("Renamed asset to {}", trimmed);
        }
    }

    pub fn remove_asset(&mut self, asset_id: usize) {
        if let Some(pos) = self.assets.iter().position(|a| a.id == asset_id) {
            self.push_undo();
            let removed = self.assets.remove(pos);
            if self.selected_asset_id == Some(asset_id) {
                self.selected_asset_id = None;
            }
            self.status_message = format!("Removed {} from media pool", removed.name);
        }
    }

    pub fn filtered_assets(&self, query: &str, kind_filter: Option<TrackKind>) -> Vec<&MediaAsset> {
        let q = query.trim().to_ascii_lowercase();
        self.assets
            .iter()
            .filter(|a| {
                let matches_kind = kind_filter.map(|k| a.kind == k).unwrap_or(true);
                let matches_query = q.is_empty() || a.name.to_ascii_lowercase().contains(&q);
                matches_kind && matches_query
            })
            .collect()
    }

    pub fn import_paths_with_probe<F>(&mut self, paths: Vec<PathBuf>, mut probe_duration: F)
    where
        F: FnMut(&Path) -> Option<f32>,
    {
        let mut imported_count = 0;
        let mut unsupported_count = 0;
        let mut pushed_undo = false;

        for path in paths {
            let Some(kind) = classify_media_path(&path) else {
                unsupported_count += 1;
                continue;
            };

            if !pushed_undo {
                self.push_undo();
                pushed_undo = true;
            }

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".into());

            let size_label = std::fs::metadata(&path)
                .map(|m| format_file_size(m.len()))
                .unwrap_or_else(|_| "Local file".into());

            let duration_secs = match kind {
                TrackKind::Overlay => 4.0,
                TrackKind::Video | TrackKind::Audio => probe_duration(&path).unwrap_or(8.0),
                TrackKind::Text | TrackKind::Adjustment => 4.0,
            };

            let id = self.next_id;
            self.next_id += 1;

            self.assets.push(MediaAsset {
                id,
                name,
                path: Some(path),
                size_label,
                kind,
                duration_secs,
            });
            self.selected_asset_id = Some(id);
            imported_count += 1;
        }

        if imported_count > 0 && unsupported_count == 0 {
            self.status_message = format!("Imported {} file(s)", imported_count);
        } else if imported_count > 0 {
            self.status_message = format!(
                "Imported {} file(s), skipped {} unsupported",
                imported_count, unsupported_count
            );
        } else if unsupported_count > 0 {
            self.status_message = "Unsupported file type (use video, image, or audio)".into();
        }
    }

    fn build_clip_base(
        id: usize,
        asset_id: usize,
        name: String,
        source_path: Option<PathBuf>,
        track: TrackKind,
        track_id: usize,
        start_secs: f32,
        duration_secs: f32,
        max_source_duration_secs: f32,
    ) -> TimelineClip {
        TimelineClip {
            id,
            asset_id,
            name,
            source_path,
            track,
            track_id,
            start_secs,
            source_in_secs: 0.0,
            duration_secs,
            max_source_duration_secs,
            opacity: 100,
            volume: if track.is_audible() { 100 } else { 0 },
            scale: 100,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation_deg: 0.0,
            flip_h: false,
            flip_v: false,
            crop: ClipCrop::default(),
            speed: 1.0,
            reversed: false,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            color: ColorAdjustment::default(),
            filter_preset: ColorFilterPreset::None,
            transition_in: TransitionKind::None,
            transition_dur_secs: 0.5,
            text: None,
            linked_clip_id: None,
            keyframes: Vec::new(),
        }
    }

    pub fn add_asset_to_timeline(&mut self, asset_id: usize) {
        let Some(asset) = self.assets.iter().find(|a| a.id == asset_id).cloned() else {
            return;
        };

        let track_id = self.default_track_id(asset.kind);
        if self.track_flags_by_id(track_id).locked {
            self.status_message = format!("Track {} is locked", asset.kind.label());
            return;
        }

        self.push_undo();

        let last_end = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id || (c.track_id == 0 && c.track == asset.kind))
            .map(|c| c.end_secs())
            .fold(0.0_f32, f32::max);

        let start = last_end;
        let clip_id = self.next_id;
        self.next_id += 1;

        if start + asset.duration_secs > self.total_duration_secs {
            self.total_duration_secs = (start + asset.duration_secs + 5.0).ceil();
        }

        let max_src = if asset.kind == TrackKind::Overlay {
            3600.0
        } else {
            asset.duration_secs
        };

        self.clips.push(Self::build_clip_base(
            clip_id,
            asset.id,
            asset.name.clone(),
            asset.path.clone(),
            asset.kind,
            track_id,
            start,
            asset.duration_secs,
            max_src,
        ));
        self.selected_clip_id = Some(clip_id);
        self.status_message = format!("Added {} to {}", asset.name, asset.kind.label());
    }

    pub fn add_text_clip(&mut self, content: impl Into<String>, duration_secs: f32) -> Option<usize> {
        let text_str = content.into();
        let dur = duration_secs.max(0.5);
        let track_id = if let Some(t) = self.tracks.iter().find(|t| t.kind == TrackKind::Text) {
            t.id
        } else {
            self.add_track(TrackKind::Text)
        };

        if self.track_flags_by_id(track_id).locked {
            self.status_message = "Text track is locked".into();
            return None;
        }

        self.push_undo();
        let clip_id = self.next_id;
        self.next_id += 1;
        let start = self.playhead_secs.max(0.0);

        if start + dur > self.total_duration_secs {
            self.total_duration_secs = (start + dur + 5.0).ceil();
        }

        let mut clip = Self::build_clip_base(
            clip_id,
            0,
            if text_str.is_empty() {
                "Text".into()
            } else {
                text_str.clone()
            },
            None,
            TrackKind::Text,
            track_id,
            start,
            dur,
            3600.0,
        );
        clip.text = Some(TextClipData {
            content: text_str,
            ..Default::default()
        });

        self.clips.push(clip);
        self.selected_clip_id = Some(clip_id);
        self.status_message = "Added Text clip".into();
        Some(clip_id)
    }

    pub fn link_clips(&mut self, clip_a: usize, clip_b: usize) -> bool {
        if clip_a == clip_b {
            return false;
        }
        let has_a = self.clips.iter().any(|c| c.id == clip_a);
        let has_b = self.clips.iter().any(|c| c.id == clip_b);
        if !has_a || !has_b {
            return false;
        }

        self.push_undo();
        for c in &mut self.clips {
            if c.id == clip_a {
                c.linked_clip_id = Some(clip_b);
            } else if c.id == clip_b {
                c.linked_clip_id = Some(clip_a);
            }
        }
        self.status_message = "Linked clips".into();
        true
    }

    pub fn unlink_clip(&mut self, clip_id: usize) -> bool {
        let Some(other_id) = self
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .and_then(|c| c.linked_clip_id)
        else {
            return false;
        };

        self.push_undo();
        for c in &mut self.clips {
            if c.id == clip_id || c.id == other_id {
                c.linked_clip_id = None;
            }
        }
        self.status_message = "Unlinked clip".into();
        true
    }

    pub fn add_marker(&mut self, time_secs: f32, label: impl Into<String>) -> usize {
        self.push_undo();
        let id = self.next_id;
        self.next_id += 1;
        let t = time_secs.clamp(0.0, self.total_duration_secs);
        self.markers.push(TimelineMarker {
            id,
            time_secs: t,
            label: label.into(),
            color_hex: 0xf59e0b,
        });
        self.status_message = format!("Added marker at {:.1}s", t);
        id
    }

    pub fn remove_marker(&mut self, marker_id: usize) {
        if let Some(pos) = self.markers.iter().position(|m| m.id == marker_id) {
            self.push_undo();
            self.markers.remove(pos);
            self.status_message = "Removed marker".into();
        }
    }

    pub fn move_clip_with_snap(
        &mut self,
        clip_id: usize,
        raw_start_secs: f32,
        snap_threshold_secs: f32,
    ) {
        let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) else {
            return;
        };
        if self.is_clip_track_locked(&self.clips[idx]) {
            return;
        }

        let old_start = self.clips[idx].start_secs;
        let duration = self.clips[idx].duration_secs;
        let linked_id = self.clips[idx].linked_clip_id;
        let mut new_start = raw_start_secs.max(0.0);

        if self.snap_enabled {
            let mut snap_targets = vec![0.0_f32, self.playhead_secs];
            for marker in &self.markers {
                snap_targets.push(marker.time_secs);
            }
            for other in &self.clips {
                if other.id != clip_id && Some(other.id) != linked_id {
                    snap_targets.push(other.start_secs);
                    snap_targets.push(other.end_secs());
                }
            }

            let mut best_dist = snap_threshold_secs;
            let mut snapped_start = new_start;

            for &target in &snap_targets {
                let d_start = (new_start - target).abs();
                if d_start < best_dist {
                    best_dist = d_start;
                    snapped_start = target;
                }
                let d_end = ((new_start + duration) - target).abs();
                if d_end < best_dist {
                    best_dist = d_end;
                    snapped_start = (target - duration).max(0.0);
                }
            }
            new_start = snapped_start;
        }

        let delta = new_start - old_start;
        self.clips[idx].start_secs = new_start;
        if new_start + duration > self.total_duration_secs {
            self.total_duration_secs = (new_start + duration + 5.0).ceil();
        }

        if let Some(lid) = linked_id {
            if let Some(l_idx) = self.clips.iter().position(|c| c.id == lid) {
                if !self.is_clip_track_locked(&self.clips[l_idx]) {
                    self.clips[l_idx].start_secs = (self.clips[l_idx].start_secs + delta).max(0.0);
                }
            }
        }
    }

    pub fn trim_clip_left(&mut self, clip_id: usize, new_start_secs: f32) {
        let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) else {
            return;
        };
        if self.is_clip_track_locked(&self.clips[idx]) {
            return;
        }

        let old_start = self.clips[idx].start_secs;
        let old_end = self.clips[idx].end_secs();
        let old_source_in = self.clips[idx].source_in_secs;

        let min_start = (old_start - old_source_in).max(0.0);
        let max_start = old_end - 0.3;
        let clamped_start = new_start_secs.clamp(min_start, max_start);

        let delta = clamped_start - old_start;
        self.clips[idx].start_secs = clamped_start;
        self.clips[idx].source_in_secs = (old_source_in + delta).max(0.0);
        self.clips[idx].duration_secs = (old_end - clamped_start).max(0.3);
    }

    pub fn trim_clip_right(&mut self, clip_id: usize, new_end_secs: f32) {
        let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) else {
            return;
        };
        if self.is_clip_track_locked(&self.clips[idx]) {
            return;
        }

        let start = self.clips[idx].start_secs;
        let max_dur = (self.clips[idx].max_source_duration_secs - self.clips[idx].source_in_secs)
            .max(0.3);
        let new_dur = (new_end_secs - start).clamp(0.3, max_dur);
        self.clips[idx].duration_secs = new_dur;
    }

    pub fn split_selected_or_at_playhead(&mut self) {
        let playhead = self.playhead_secs;
        let target_idx = if let Some(sel_id) = self.selected_clip_id {
            self.clips
                .iter()
                .position(|c| c.id == sel_id && c.contains(playhead))
        } else {
            None
        }
        .or_else(|| {
            self.clips
                .iter()
                .position(|c| c.track == TrackKind::Video && c.contains(playhead))
        })
        .or_else(|| self.clips.iter().position(|c| c.contains(playhead)));

        let Some(idx) = target_idx else {
            self.status_message = "No clip under playhead to split".into();
            return;
        };

        if self.is_clip_track_locked(&self.clips[idx]) {
            self.status_message = format!("Track {} is locked", self.clips[idx].track.label());
            return;
        }

        let left_dur = playhead - self.clips[idx].start_secs;
        let right_dur = self.clips[idx].end_secs() - playhead;
        if left_dur < 0.3 || right_dur < 0.3 {
            self.status_message = "Clip segment too short to split".into();
            return;
        }

        self.push_undo();

        let original = self.clips[idx].clone();
        self.clips[idx].duration_secs = left_dur;

        let new_id = self.next_id;
        self.next_id += 1;
        let mut right_clip = original.clone();
        right_clip.id = new_id;
        right_clip.name = format!("{} (2)", original.name);
        right_clip.start_secs = playhead;
        right_clip.source_in_secs = original.source_in_secs + left_dur;
        right_clip.duration_secs = right_dur;
        right_clip.linked_clip_id = None;

        self.clips.push(right_clip);
        self.selected_clip_id = Some(new_id);
        self.status_message = format!("Split {} at {:.1}s", original.name, playhead);
    }

    pub fn delete_selected_clip(&mut self) {
        let Some(id) = self.selected_clip_id else {
            self.status_message = "No clip selected".into();
            return;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = format!("Track {} is locked", self.clips[pos].track.label());
            return;
        }

        self.push_undo();
        let removed = self.clips.remove(pos);
        if let Some(lid) = removed.linked_clip_id {
            if let Some(l_pos) = self.clips.iter().position(|c| c.id == lid) {
                if !self.is_clip_track_locked(&self.clips[l_pos]) {
                    self.clips.remove(l_pos);
                }
            }
        }
        if self.main_track_magnet {
            self.compact_main_track();
        }
        self.selected_clip_id = None;
        self.status_message = format!("Removed {}", removed.name);
    }

    pub fn ripple_delete_selected(&mut self) {
        let Some(id) = self.selected_clip_id else {
            self.status_message = "No clip selected".into();
            return;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = format!("Track {} is locked", self.clips[pos].track.label());
            return;
        }

        self.push_undo();
        let removed = self.clips.remove(pos);
        let shift = removed.duration_secs;

        for clip in &mut self.clips {
            let same_track = if removed.track_id != 0 {
                clip.track_id == removed.track_id
            } else {
                clip.track == removed.track
            };
            if same_track && clip.start_secs >= removed.end_secs() - 0.001 {
                clip.start_secs = (clip.start_secs - shift).max(0.0);
            }
        }

        self.selected_clip_id = None;
        self.status_message = format!("Ripple deleted {}", removed.name);
    }

    pub fn copy_selected_clip(&mut self) -> bool {
        let Some(clip) = self.selected_clip().cloned() else {
            self.status_message = "No clip selected to copy".into();
            return false;
        };
        let name = clip.name.clone();
        self.copied_clip = Some(clip);
        self.status_message = format!("Copied {}", name);
        true
    }

    pub fn paste_clip_at_playhead(&mut self) -> Option<usize> {
        let mut clip = self.copied_clip.clone()?;
        if self.is_clip_track_locked(&clip) {
            self.status_message = format!("Track {} is locked", clip.track.label());
            return None;
        }
        self.push_undo();
        self.next_id += 1;
        let new_id = self.next_id;
        clip.id = new_id;
        clip.start_secs = self.playhead_secs.max(0.0);
        clip.linked_clip_id = None;
        let name = clip.name.clone();
        let is_main_video = clip.track == TrackKind::Video
            && (clip.track_id == 0
                || self
                    .tracks
                    .iter()
                    .find(|t| t.id == clip.track_id)
                    .map(|t| t.is_main)
                    .unwrap_or(false));
        let clip_end = clip.end_secs();
        self.clips.push(clip);
        if self.main_track_magnet && is_main_video {
            self.compact_main_track();
        }
        if clip_end + 5.0 > self.total_duration_secs {
            self.total_duration_secs = (clip_end + 5.0).ceil();
        }
        self.selected_clip_id = Some(new_id);
        self.status_message = format!("Pasted {}", name);
        Some(new_id)
    }

    pub fn reset_crop_selected(&mut self) -> bool {
        let Some(id) = self.selected_clip_id else {
            return false;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = format!("Track {} is locked", self.clips[pos].track.label());
            return false;
        }
        self.push_undo();
        self.clips[pos].crop = ClipCrop::default();
        self.status_message = "Reset crop".into();
        true
    }

    pub fn adjust_zoom(&mut self, factor: f32) {
        self.px_per_sec = (self.px_per_sec * factor).clamp(4.0, 120.0);
    }

    pub fn toggle_mirror_selected(&mut self) -> bool {
        let Some(id) = self.selected_clip_id else {
            self.status_message = "No clip selected".into();
            return false;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = "Track is locked".into();
            return false;
        }
        self.push_undo();
        self.clips[pos].flip_h = !self.clips[pos].flip_h;
        self.status_message = if self.clips[pos].flip_h {
            "Mirrored horizontally".into()
        } else {
            "Mirror removed".into()
        };
        true
    }

    pub fn rotate_selected_90(&mut self) -> bool {
        let Some(id) = self.selected_clip_id else {
            self.status_message = "No clip selected".into();
            return false;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = "Track is locked".into();
            return false;
        }
        self.push_undo();
        let playhead = self.playhead_secs;
        let (px, py, sc, cur_rot, op) = self.clips[pos].evaluate_at(playhead);
        let idx = (((cur_rot + 0.5).rem_euclid(360.0) / 90.0).floor() as i32).rem_euclid(4);
        let next_rot = (((idx + 1) % 4) as f32) * 90.0;
        self.clips[pos].pos_x = px;
        self.clips[pos].pos_y = py;
        self.clips[pos].scale = sc;
        self.clips[pos].rotation_deg = next_rot;
        self.clips[pos].opacity = op;
        self.clips[pos].sync_keyframe_if_animated(playhead);
        self.status_message = format!("Rotated → {:.0}°", next_rot);
        true
    }

    pub fn toggle_reverse_selected(&mut self) -> bool {
        let Some(id) = self.selected_clip_id else {
            self.status_message = "No clip selected".into();
            return false;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = "Track is locked".into();
            return false;
        }
        self.push_undo();
        self.clips[pos].reversed = !self.clips[pos].reversed;
        self.status_message = if self.clips[pos].reversed {
            "Reverse playback ON".into()
        } else {
            "Reverse playback OFF".into()
        };
        true
    }

    pub fn set_selected_speed(&mut self, new_speed: f32) -> bool {
        let Some(id) = self.selected_clip_id else {
            return false;
        };
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.is_clip_track_locked(&self.clips[pos]) {
            return false;
        }
        let clamped = new_speed.clamp(0.25, 4.0);
        let old_speed = self.clips[pos].speed.max(0.25);
        if (old_speed - clamped).abs() < 0.01 {
            return false;
        }
        let new_dur = (self.clips[pos].duration_secs * (old_speed / clamped)).max(0.3);
        self.clips[pos].speed = clamped;
        self.clips[pos].duration_secs = new_dur;
        self.status_message = format!("Speed: {:.2}x", clamped);
        true
    }

    pub fn toggle_keyframe_at_playhead(&mut self) -> bool {
        let Some(id) = self.selected_clip_id else {
            self.status_message = "Select a clip to add keyframe".into();
            return false;
        };
        let playhead = self.playhead_secs;
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        if !self.clips[pos].contains(playhead) {
            self.status_message = "Move playhead inside the selected clip".into();
            return false;
        }
        if self.is_clip_track_locked(&self.clips[pos]) {
            self.status_message = "Track is locked".into();
            return false;
        }

        self.push_undo();
        let local_t = (playhead - self.clips[pos].start_secs).clamp(0.0, self.clips[pos].duration_secs);
        let nearest_idx = self.clips[pos]
            .keyframes
            .iter()
            .enumerate()
            .filter(|(_, k)| (k.time_offset_secs - local_t).abs() < 0.08)
            .min_by(|(_, a), (_, b)| {
                (a.time_offset_secs - local_t)
                    .abs()
                    .partial_cmp(&(b.time_offset_secs - local_t).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);
        if let Some(kf_idx) = nearest_idx {
            let removed = self.clips[pos].keyframes.remove(kf_idx);
            if self.clips[pos].keyframes.is_empty() {
                self.clips[pos].pos_x = removed.pos_x;
                self.clips[pos].pos_y = removed.pos_y;
                self.clips[pos].scale = removed.scale;
                self.clips[pos].rotation_deg = removed.rotation_deg;
                self.clips[pos].opacity = removed.opacity;
            }
            self.status_message = format!("Removed keyframe at {:.1}s", local_t);
            return true;
        }

        let (px, py, sc, rot, op) = self.clips[pos].evaluate_at(playhead);
        self.clips[pos].keyframes.push(ClipKeyframe {
            time_offset_secs: local_t,
            pos_x: px,
            pos_y: py,
            scale: sc,
            rotation_deg: rot,
            opacity: op,
        });
        self.clips[pos].keyframes.sort_by(|a, b| {
            a.time_offset_secs
                .partial_cmp(&b.time_offset_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.status_message = format!("Added keyframe at {:.1}s", local_t);
        true
    }

    pub fn insert_freeze_frame_at_playhead(&mut self, image_path: PathBuf, freeze_dur: f32) -> bool {
        let playhead = self.playhead_secs;
        let overlay_track_id = self.default_track_id(TrackKind::Overlay);
        if self.track_flags_by_id(overlay_track_id).locked {
            self.status_message = "Overlay track is locked".into();
            return false;
        }

        self.push_undo();
        let dur = freeze_dur.clamp(0.5, 15.0);
        let clip_id = self.next_id;
        self.next_id += 1;
        if playhead + dur > self.total_duration_secs {
            self.total_duration_secs = (playhead + dur + 5.0).ceil();
        }
        let mut clip = Self::build_clip_base(
            clip_id,
            0,
            format!("Freeze {:.1}s", playhead),
            Some(image_path),
            TrackKind::Overlay,
            overlay_track_id,
            playhead,
            dur,
            3600.0,
        );
        clip.volume = 0;
        self.clips.push(clip);
        self.selected_clip_id = Some(clip_id);
        self.status_message = format!("Inserted freeze frame at {:.1}s", playhead);
        true
    }
}

pub fn classify_media_path(path: &Path) -> Option<TrackKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "wmv" => Some(TrackKind::Video),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" => Some(TrackKind::Overlay),
        "mp3" | "wav" | "aac" | "flac" | "ogg" | "m4a" | "opus" => Some(TrackKind::Audio),
        _ => None,
    }
}

pub fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_timecode(secs: f32) -> String {
    let total_tenths = (secs.max(0.0) * 10.0).round() as u32;
    let tenths = total_tenths % 10;
    let total_secs = total_tenths / 10;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{:02}:{:02}.{}", minutes, seconds, tenths)
}

pub fn recent_projects_file_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("OpenCut").join("recent_projects.json")
}

pub fn load_recent_projects() -> Vec<RecentProjectEntry> {
    let path = recent_projects_file_path();
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_recent_projects(entries: &[RecentProjectEntry]) {
    let path = recent_projects_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(path, json);
    }
}

pub fn record_recent_project(entry: RecentProjectEntry) -> Vec<RecentProjectEntry> {
    let mut list = load_recent_projects();
    list.retain(|item| item.path != entry.path);
    list.insert(0, entry);
    list.truncate(12);
    save_recent_projects(&list);
    list
}

pub fn remove_recent_project(path: &Path) -> Vec<RecentProjectEntry> {
    let mut list = load_recent_projects();
    list.retain(|item| item.path != path);
    save_recent_projects(&list);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_clip(id: usize, start: f32, dur: f32) -> TimelineClip {
        EditorCore::build_clip_base(
            id,
            id,
            format!("clip-{}.mp4", id),
            None,
            TrackKind::Video,
            2,
            start,
            dur,
            dur,
        )
    }

    #[test]
    fn undo_and_redo_delete() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 5.0));
        core.selected_clip_id = Some(1);

        core.delete_selected_clip();
        assert!(core.clips.is_empty());
        assert!(core.can_undo());

        assert!(core.undo());
        assert_eq!(core.clips.len(), 1);
        assert!(core.can_redo());

        assert!(core.redo());
        assert!(core.clips.is_empty());
    }

    #[test]
    fn ripple_delete_shifts_following_clips() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 4.0));
        core.clips.push(sample_clip(2, 4.0, 3.0));
        core.clips.push(sample_clip(3, 8.0, 2.0));
        core.selected_clip_id = Some(1);

        core.ripple_delete_selected();
        assert_eq!(core.clips.len(), 2);
        assert!((core.clips[0].start_secs - 0.0).abs() < 0.001);
        assert!((core.clips[1].start_secs - 4.0).abs() < 0.001);
    }

    #[test]
    fn trim_left_updates_start_and_source_in() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 2.0, 6.0));
        core.trim_clip_left(1, 4.0);

        let c = &core.clips[0];
        assert!((c.start_secs - 4.0).abs() < 0.001);
        assert!((c.source_in_secs - 2.0).abs() < 0.001);
        assert!((c.duration_secs - 4.0).abs() < 0.001);
    }

    #[test]
    fn snap_moves_clip_to_neighbor_end() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 5.0));
        core.clips.push(sample_clip(2, 8.0, 4.0));

        core.move_clip_with_snap(2, 5.15, 0.25);
        assert!((core.clips[1].start_secs - 5.0).abs() < 0.001);
    }

    #[test]
    fn locked_track_prevents_delete() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 5.0));
        core.selected_clip_id = Some(1);
        core.toggle_lock(TrackKind::Video);

        core.delete_selected_clip();
        assert_eq!(core.clips.len(), 1);
    }

    #[test]
    fn multi_track_add_and_remove() {
        let mut core = EditorCore::new();
        assert_eq!(core.tracks.len(), 3);

        let new_id = core.add_track(TrackKind::Audio);
        assert_eq!(core.tracks.len(), 4);
        assert!(core.remove_track(new_id).is_ok());
        assert_eq!(core.tracks.len(), 3);

        // Main track cannot be removed
        let main_id = core.tracks.iter().find(|t| t.is_main).unwrap().id;
        assert!(core.remove_track(main_id).is_err());
    }

    #[test]
    fn main_track_magnet_compacts_gaps() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 2.0, 3.0));
        core.clips.push(sample_clip(2, 8.0, 4.0));

        core.toggle_main_track_magnet();
        assert!(core.main_track_magnet);
        assert!((core.clips[0].start_secs - 0.0).abs() < 0.001);
        assert!((core.clips[1].start_secs - 3.0).abs() < 0.001);
    }

    #[test]
    fn link_clips_moves_together() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 1.0, 4.0));
        let mut audio = sample_clip(2, 1.0, 4.0);
        audio.track = TrackKind::Audio;
        audio.track_id = 3;
        core.clips.push(audio);

        assert!(core.link_clips(1, 2));
        core.move_clip_with_snap(1, 5.0, 0.1);
        assert!((core.clips[0].start_secs - 5.0).abs() < 0.001);
        assert!((core.clips[1].start_secs - 5.0).abs() < 0.001);
    }

    #[test]
    fn marker_acts_as_snap_target() {
        let mut core = EditorCore::new();
        core.add_marker(6.0, "Beat");
        core.clips.push(sample_clip(1, 1.0, 2.0));

        core.move_clip_with_snap(1, 5.85, 0.25);
        assert!((core.clips[0].start_secs - 6.0).abs() < 0.001);
    }

    #[test]
    fn add_text_clip_creates_text_track() {
        let mut core = EditorCore::new();
        core.seek_to(3.0);
        let clip_id = core.add_text_clip("Hello OpenCut", 4.0).unwrap();
        assert!(core.tracks.iter().any(|t| t.kind == TrackKind::Text));
        let clip = core.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.track, TrackKind::Text);
        assert_eq!(clip.text.as_ref().unwrap().content, "Hello OpenCut");
    }

    #[test]
    fn aspect_ratio_dimensions() {
        assert_eq!(AspectRatio::Ratio16x9.dimensions(720), (1280, 720));
        assert_eq!(AspectRatio::Ratio9x16.dimensions(720), (720, 1280));
        assert_eq!(AspectRatio::Ratio1x1.dimensions(720), (720, 720));
    }

    #[test]
    fn keyframe_interpolation_evaluates_midpoint() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 4.0));
        core.selected_clip_id = Some(1);

        core.seek_to(0.0);
        assert!(core.toggle_keyframe_at_playhead());
        core.clips[0].pos_x = 100.0;
        core.clips[0].rotation_deg = 90.0;
        core.seek_to(4.0);
        // evaluate_at clamps to duration; test explicit second keyframe at 2.0s
        core.seek_to(2.0);
        assert!(core.toggle_keyframe_at_playhead());
        core.clips[0].keyframes[1].pos_x = 100.0;
        core.clips[0].keyframes[1].rotation_deg = 90.0;

        let (px, _, _, rot, _) = core.clips[0].evaluate_at(1.0);
        assert!((px - 50.0).abs() < 0.01);
        assert!((rot - 45.0).abs() < 0.01);
    }

    #[test]
    fn mirror_rotate_reverse_speed_work() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 4.0));
        core.selected_clip_id = Some(1);

        assert!(core.toggle_mirror_selected());
        assert!(core.clips[0].flip_h);
        assert!(core.rotate_selected_90());
        assert!((core.clips[0].rotation_deg - 90.0).abs() < 0.01);
        assert!(core.toggle_reverse_selected());
        assert!(core.clips[0].reversed);
        assert!(core.set_selected_speed(2.0));
        assert!((core.clips[0].duration_secs - 2.0).abs() < 0.01);
    }

    #[test]
    fn copy_and_paste_clip_at_playhead() {
        let mut core = EditorCore::new();
        core.clips.push(sample_clip(1, 0.0, 3.0));
        core.selected_clip_id = Some(1);
        assert!(core.copy_selected_clip());

        core.seek_to(5.0);
        let pasted_id = core.paste_clip_at_playhead().expect("should paste");
        assert_ne!(pasted_id, 1);
        assert_eq!(core.clips.len(), 2);
        let pasted = core.clips.iter().find(|c| c.id == pasted_id).unwrap();
        assert!((pasted.start_secs - 5.0).abs() < 0.01);
        assert_eq!(core.selected_clip_id, Some(pasted_id));
    }

    #[test]
    fn phase4_color_filter_and_transition_evaluate() {
        let mut clip = sample_clip(1, 2.0, 4.0);
        clip.filter_preset = ColorFilterPreset::Bw;
        clip.color.brightness = 0.1;
        let (b, c, s, _t) = clip.effective_color();
        assert!((b - 0.12).abs() < 0.01);
        assert!((c - 0.25).abs() < 0.01);
        assert!((s + 1.0).abs() < 0.01);

        clip.transition_in = TransitionKind::Dissolve;
        clip.transition_dur_secs = 1.0;
        let (a0, _, _) = clip.evaluate_transition(2.0);
        let (a_mid, _, _) = clip.evaluate_transition(2.5);
        let (a_end, _, _) = clip.evaluate_transition(3.0);
        assert!(a0 < 0.01);
        assert!((a_mid - 0.75).abs() < 0.01);
        assert!((a_end - 1.0).abs() < 0.01);
    }
}
