use std::path::{Path, PathBuf};

use crate::media_engine::probe_duration_secs;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
            TrackKind::Video => Theme::CLIP_VIDEO,
            TrackKind::Overlay => Theme::CLIP_OVERLAY,
            TrackKind::Audio => Theme::CLIP_AUDIO,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MediaAsset {
    pub id: usize,
    pub name: String,
    pub path: Option<PathBuf>,
    pub size_label: String,
    pub kind: TrackKind,
    pub duration_secs: f32,
}

#[derive(Clone, Debug)]
pub struct TimelineClip {
    pub id: usize,
    pub asset_id: usize,
    pub name: String,
    pub source_path: Option<PathBuf>,
    pub track: TrackKind,
    pub start_secs: f32,
    pub source_in_secs: f32,
    pub duration_secs: f32,
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

pub struct EditorState {
    pub project_name: String,
    pub assets: Vec<MediaAsset>,
    pub clips: Vec<TimelineClip>,
    pub selected_asset_id: Option<usize>,
    pub selected_clip_id: Option<usize>,
    pub playhead_secs: f32,
    pub total_duration_secs: f32,
    pub is_playing: bool,
    pub px_per_sec: f32,
    pub status_message: String,
    next_id: usize,
}

impl EditorState {
    pub fn new() -> Self {
        Self {
            project_name: "Untitled Project".into(),
            assets: Vec::new(),
            clips: Vec::new(),
            selected_asset_id: None,
            selected_clip_id: None,
            playhead_secs: 0.0,
            total_duration_secs: 30.0,
            is_playing: false,
            px_per_sec: 32.0,
            status_message: "Drop or import media to begin".into(),
            next_id: 1,
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
            .filter(|c| c.contains(self.playhead_secs))
            .collect()
    }

    pub fn active_video_clip(&self) -> Option<&TimelineClip> {
        self.clips
            .iter()
            .filter(|c| c.track != TrackKind::Audio && c.contains(self.playhead_secs))
            .max_by_key(|c| match c.track {
                TrackKind::Overlay => 2,
                TrackKind::Video => 1,
                TrackKind::Audio => 0,
            })
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

    pub fn import_paths(&mut self, paths: Vec<PathBuf>) {
        let mut imported_count = 0;
        let mut unsupported_count = 0;

        for path in paths {
            let Some(kind) = classify_media_path(&path) else {
                unsupported_count += 1;
                continue;
            };

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".into());

            let size_label = std::fs::metadata(&path)
                .map(|m| format_file_size(m.len()))
                .unwrap_or_else(|_| "Local file".into());

            let duration_secs = match kind {
                TrackKind::Overlay => 4.0,
                TrackKind::Video | TrackKind::Audio => probe_duration_secs(&path).unwrap_or(8.0),
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

        self.clips.push(TimelineClip {
            id: clip_id,
            asset_id: asset.id,
            name: asset.name.clone(),
            source_path: asset.path.clone(),
            track: asset.kind,
            start_secs: start,
            source_in_secs: 0.0,
            duration_secs: asset.duration_secs,
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

        let left_dur = playhead - self.clips[idx].start_secs;
        let right_dur = self.clips[idx].end_secs() - playhead;
        if left_dur < 0.3 || right_dur < 0.3 {
            self.status_message = "Clip segment too short to split".into();
            return;
        }

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
        if let Some(pos) = self.clips.iter().position(|c| c.id == id) {
            let removed = self.clips.remove(pos);
            self.selected_clip_id = None;
            self.status_message = format!("Removed {}", removed.name);
        }
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
