use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Overlay,
    Audio,
}

impl TrackKind {
    pub fn label(&self) -> &'static str {
        match self {
            TrackKind::Video => "V1",
            TrackKind::Overlay => "V2",
            TrackKind::Audio => "A1",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            TrackKind::Video => "Video",
            TrackKind::Overlay => "Image",
            TrackKind::Audio => "Audio",
        }
    }

    pub fn color_hex(&self) -> u32 {
        match self {
            TrackKind::Video => 0x2563eb,
            TrackKind::Overlay => 0x8b5cf6,
            TrackKind::Audio => 0x059669,
        }
    }

    pub fn index(&self) -> usize {
        match self {
            TrackKind::Overlay => 0,
            TrackKind::Video => 1,
            TrackKind::Audio => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackFlags {
    pub muted: bool,
    pub hidden: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: usize,
    pub name: String,
    pub path: Option<PathBuf>,
    pub size_label: String,
    pub kind: TrackKind,
    pub duration_secs: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineClip {
    pub id: usize,
    pub asset_id: usize,
    pub name: String,
    pub source_path: Option<PathBuf>,
    pub track: TrackKind,
    pub start_secs: f32,
    pub source_in_secs: f32,
    pub duration_secs: f32,
    pub max_source_duration_secs: f32,
    pub opacity: u8,
    pub volume: u8,
    pub scale: u16,
}

impl TimelineClip {
    pub fn end_secs(&self) -> f32 {
        self.start_secs + self.duration_secs
    }

    pub fn contains(&self, time_secs: f32) -> bool {
        time_secs >= self.start_secs && time_secs < self.end_secs()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project_name: String,
    pub assets: Vec<MediaAsset>,
    pub clips: Vec<TimelineClip>,
    pub track_flags: [TrackFlags; 3],
    pub total_duration_secs: f32,
    pub next_id: usize,
}

pub struct EditorCore {
    pub project_name: String,
    pub project_path: Option<PathBuf>,
    pub assets: Vec<MediaAsset>,
    pub clips: Vec<TimelineClip>,
    pub track_flags: [TrackFlags; 3],
    pub selected_asset_id: Option<usize>,
    pub selected_clip_id: Option<usize>,
    pub playhead_secs: f32,
    pub total_duration_secs: f32,
    pub is_playing: bool,
    pub px_per_sec: f32,
    pub status_message: String,
    pub snap_enabled: bool,
    pub next_id: usize,
    undo_stack: Vec<ProjectSnapshot>,
    redo_stack: Vec<ProjectSnapshot>,
}

impl Default for EditorCore {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorCore {
    pub fn new() -> Self {
        Self {
            project_name: "Untitled Project".into(),
            project_path: None,
            assets: Vec::new(),
            clips: Vec::new(),
            track_flags: [TrackFlags::default(); 3],
            selected_asset_id: None,
            selected_clip_id: None,
            playhead_secs: 0.0,
            total_duration_secs: 30.0,
            is_playing: false,
            px_per_sec: 32.0,
            status_message: "Drop or import media to begin".into(),
            snap_enabled: true,
            next_id: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot {
            project_name: self.project_name.clone(),
            assets: self.assets.clone(),
            clips: self.clips.clone(),
            track_flags: self.track_flags,
            total_duration_secs: self.total_duration_secs,
            next_id: self.next_id,
        }
    }

    fn apply_snapshot(&mut self, snap: ProjectSnapshot) {
        self.project_name = snap.project_name;
        self.assets = snap.assets;
        self.clips = snap.clips;
        self.track_flags = snap.track_flags;
        self.total_duration_secs = snap.total_duration_secs;
        self.next_id = snap.next_id;

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

    pub fn track_flags(&self, track: TrackKind) -> TrackFlags {
        self.track_flags[track.index()]
    }

    pub fn toggle_mute(&mut self, track: TrackKind) {
        self.push_undo();
        let idx = track.index();
        self.track_flags[idx].muted = !self.track_flags[idx].muted;
    }

    pub fn toggle_hide(&mut self, track: TrackKind) {
        self.push_undo();
        let idx = track.index();
        self.track_flags[idx].hidden = !self.track_flags[idx].hidden;
    }

    pub fn toggle_lock(&mut self, track: TrackKind) {
        self.push_undo();
        let idx = track.index();
        self.track_flags[idx].locked = !self.track_flags[idx].locked;
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
            .filter(|c| !self.track_flags(c.track).hidden && c.contains(self.playhead_secs))
            .collect()
    }

    pub fn active_video_clip(&self) -> Option<&TimelineClip> {
        self.clips
            .iter()
            .filter(|c| {
                c.track != TrackKind::Audio
                    && !self.track_flags(c.track).hidden
                    && c.contains(self.playhead_secs)
            })
            .max_by_key(|c| match c.track {
                TrackKind::Overlay => 2,
                TrackKind::Video => 1,
                TrackKind::Audio => 0,
            })
    }

    pub fn active_audio_clips_at(&self, time_secs: f32) -> Vec<&TimelineClip> {
        self.clips
            .iter()
            .filter(|c| {
                (c.track == TrackKind::Audio || c.track == TrackKind::Video)
                    && !self.track_flags(c.track).muted
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

    pub fn add_asset_to_timeline(&mut self, asset_id: usize) {
        let Some(asset) = self.assets.iter().find(|a| a.id == asset_id).cloned() else {
            return;
        };

        if self.track_flags(asset.kind).locked {
            self.status_message = format!("Track {} is locked", asset.kind.label());
            return;
        }

        self.push_undo();

        let last_end = self
            .clips
            .iter()
            .filter(|c| c.track == asset.kind)
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

        self.clips.push(TimelineClip {
            id: clip_id,
            asset_id: asset.id,
            name: asset.name.clone(),
            source_path: asset.path.clone(),
            track: asset.kind,
            start_secs: start,
            source_in_secs: 0.0,
            duration_secs: asset.duration_secs,
            max_source_duration_secs: max_src,
            opacity: 100,
            volume: if asset.kind == TrackKind::Overlay {
                0
            } else {
                100
            },
            scale: 100,
        });
        self.selected_clip_id = Some(clip_id);
        self.status_message = format!("Added {} to {}", asset.name, asset.kind.label());
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
        let track = self.clips[idx].track;
        if self.track_flags(track).locked {
            return;
        }

        let duration = self.clips[idx].duration_secs;
        let mut new_start = raw_start_secs.max(0.0);

        if self.snap_enabled {
            let mut snap_targets = vec![0.0_f32, self.playhead_secs];
            for other in &self.clips {
                if other.id != clip_id {
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

        self.clips[idx].start_secs = new_start;
        if new_start + duration > self.total_duration_secs {
            self.total_duration_secs = (new_start + duration + 5.0).ceil();
        }
    }

    pub fn trim_clip_left(&mut self, clip_id: usize, new_start_secs: f32) {
        let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) else {
            return;
        };
        if self.track_flags(self.clips[idx].track).locked {
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
        if self.track_flags(self.clips[idx].track).locked {
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

        if self.track_flags(self.clips[idx].track).locked {
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
        let right_clip = TimelineClip {
            id: new_id,
            asset_id: original.asset_id,
            name: format!("{} (2)", original.name),
            source_path: original.source_path,
            track: original.track,
            start_secs: playhead,
            source_in_secs: original.source_in_secs + left_dur,
            duration_secs: right_dur,
            max_source_duration_secs: original.max_source_duration_secs,
            opacity: original.opacity,
            volume: original.volume,
            scale: original.scale,
        };
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
        if self.track_flags(self.clips[pos].track).locked {
            self.status_message = format!("Track {} is locked", self.clips[pos].track.label());
            return;
        }

        self.push_undo();
        let removed = self.clips.remove(pos);
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
        let track = self.clips[pos].track;
        if self.track_flags(track).locked {
            self.status_message = format!("Track {} is locked", track.label());
            return;
        }

        self.push_undo();
        let removed = self.clips.remove(pos);
        let shift = removed.duration_secs;

        for clip in &mut self.clips {
            if clip.track == track && clip.start_secs >= removed.end_secs() - 0.001 {
                clip.start_secs = (clip.start_secs - shift).max(0.0);
            }
        }

        self.selected_clip_id = None;
        self.status_message = format!("Ripple deleted {}", removed.name);
    }

    pub fn adjust_zoom(&mut self, factor: f32) {
        self.px_per_sec = (self.px_per_sec * factor).clamp(4.0, 120.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_clip(id: usize, start: f32, dur: f32) -> TimelineClip {
        TimelineClip {
            id,
            asset_id: id,
            name: format!("clip-{}.mp4", id),
            source_path: None,
            track: TrackKind::Video,
            start_secs: start,
            source_in_secs: 0.0,
            duration_secs: dur,
            max_source_duration_secs: dur,
            opacity: 100,
            volume: 100,
            scale: 100,
        }
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
}
