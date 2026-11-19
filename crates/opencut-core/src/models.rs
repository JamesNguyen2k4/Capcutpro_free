use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AspectRatio {
    #[default]
    Ratio16x9,
    Ratio9x16,
    Ratio1x1,
    Ratio4x3,
    Ratio3x4,
    Ratio21x9,
}

impl AspectRatio {
    pub const fn all() -> &'static [AspectRatio] {
        &[
            AspectRatio::Ratio16x9,
            AspectRatio::Ratio9x16,
            AspectRatio::Ratio1x1,
            AspectRatio::Ratio4x3,
            AspectRatio::Ratio3x4,
            AspectRatio::Ratio21x9,
        ]
    }

    pub const fn label(&self) -> &'static str {
        match self {
            AspectRatio::Ratio16x9 => "16:9",
            AspectRatio::Ratio9x16 => "9:16",
            AspectRatio::Ratio1x1 => "1:1",
            AspectRatio::Ratio4x3 => "4:3",
            AspectRatio::Ratio3x4 => "3:4",
            AspectRatio::Ratio21x9 => "21:9",
        }
    }

    pub const fn ratio_f32(&self) -> f32 {
        match self {
            AspectRatio::Ratio16x9 => 16.0 / 9.0,
            AspectRatio::Ratio9x16 => 9.0 / 16.0,
            AspectRatio::Ratio1x1 => 1.0,
            AspectRatio::Ratio4x3 => 4.0 / 3.0,
            AspectRatio::Ratio3x4 => 3.0 / 4.0,
            AspectRatio::Ratio21x9 => 21.0 / 9.0,
        }
    }

    /// Compute even-aligned (width, height) dimensions given a reference height.
    pub fn dimensions(&self, base_height: u32) -> (u32, u32) {
        let (w, h) = match self {
            AspectRatio::Ratio16x9 => ((base_height * 16) / 9, base_height),
            AspectRatio::Ratio9x16 => (base_height, (base_height * 16) / 9),
            AspectRatio::Ratio1x1 => (base_height, base_height),
            AspectRatio::Ratio4x3 => ((base_height * 4) / 3, base_height),
            AspectRatio::Ratio3x4 => ((base_height * 3) / 4, base_height),
            AspectRatio::Ratio21x9 => ((base_height * 21) / 9, base_height),
        };
        (w.max(16) & !1, h.max(16) & !1)
    }

    pub fn next(&self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|r| r == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Overlay,
    Audio,
    Text,
    Adjustment,
}

impl TrackKind {
    pub fn label(&self) -> &'static str {
        match self {
            TrackKind::Video => "V1",
            TrackKind::Overlay => "V2",
            TrackKind::Audio => "A1",
            TrackKind::Text => "T1",
            TrackKind::Adjustment => "ADJ",
        }
    }

    pub fn prefix(&self) -> &'static str {
        match self {
            TrackKind::Video => "V",
            TrackKind::Overlay => "O",
            TrackKind::Audio => "A",
            TrackKind::Text => "T",
            TrackKind::Adjustment => "ADJ",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            TrackKind::Video => "Video",
            TrackKind::Overlay => "Image",
            TrackKind::Audio => "Audio",
            TrackKind::Text => "Text",
            TrackKind::Adjustment => "Adjustment",
        }
    }

    pub fn color_hex(&self) -> u32 {
        match self {
            TrackKind::Video => 0x2563eb,
            TrackKind::Overlay => 0x8b5cf6,
            TrackKind::Audio => 0x059669,
            TrackKind::Text => 0xd97706,
            TrackKind::Adjustment => 0xdb2777,
        }
    }

    pub fn index(&self) -> usize {
        match self {
            TrackKind::Overlay => 0,
            TrackKind::Video => 1,
            TrackKind::Audio => 2,
            TrackKind::Text => 3,
            TrackKind::Adjustment => 4,
        }
    }

    pub fn is_visual(&self) -> bool {
        !matches!(self, TrackKind::Audio)
    }

    pub fn is_audible(&self) -> bool {
        matches!(self, TrackKind::Video | TrackKind::Audio)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackFlags {
    pub muted: bool,
    pub hidden: bool,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: usize,
    pub kind: TrackKind,
    pub name: String,
    pub flags: TrackFlags,
    pub is_main: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipCrop {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorAdjustment {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub highlights: f32,
    pub shadows: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextClipData {
    pub content: String,
    pub font_family: String,
    pub font_size: f32,
    pub color_hex: u32,
    pub bg_color_hex: Option<u32>,
    pub stroke_color_hex: Option<u32>,
    pub stroke_width: f32,
    pub align: TextAlign,
}

impl Default for TextClipData {
    fn default() -> Self {
        Self {
            content: "Default Text".into(),
            font_family: "Sans".into(),
            font_size: 48.0,
            color_hex: 0xffffff,
            bg_color_hex: None,
            stroke_color_hex: Some(0x000000),
            stroke_width: 2.0,
            align: TextAlign::Center,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineMarker {
    pub id: usize,
    pub time_secs: f32,
    pub label: String,
    pub color_hex: u32,
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

fn default_speed() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineClip {
    pub id: usize,
    pub asset_id: usize,
    pub name: String,
    pub source_path: Option<PathBuf>,
    pub track: TrackKind,
    #[serde(default)]
    pub track_id: usize,
    pub start_secs: f32,
    pub source_in_secs: f32,
    pub duration_secs: f32,
    pub max_source_duration_secs: f32,
    pub opacity: u8,
    pub volume: u8,
    pub scale: u16,
    #[serde(default)]
    pub pos_x: f32,
    #[serde(default)]
    pub pos_y: f32,
    #[serde(default)]
    pub rotation_deg: f32,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    #[serde(default)]
    pub crop: ClipCrop,
    #[serde(default = "default_speed")]
    pub speed: f32,
    #[serde(default)]
    pub reversed: bool,
    #[serde(default)]
    pub fade_in_secs: f32,
    #[serde(default)]
    pub fade_out_secs: f32,
    #[serde(default)]
    pub color: ColorAdjustment,
    #[serde(default)]
    pub text: Option<TextClipData>,
    #[serde(default)]
    pub linked_clip_id: Option<usize>,
}

impl TimelineClip {
    pub fn end_secs(&self) -> f32 {
        self.start_secs + self.duration_secs
    }

    pub fn contains(&self, time_secs: f32) -> bool {
        time_secs >= self.start_secs && time_secs < self.end_secs()
    }
}

fn default_fps() -> u32 {
    30
}

fn default_base_height() -> u32 {
    720
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProjectEntry {
    pub name: String,
    pub path: PathBuf,
    pub aspect_ratio: AspectRatio,
    pub base_height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project_name: String,
    #[serde(default)]
    pub aspect_ratio: AspectRatio,
    #[serde(default = "default_base_height")]
    pub base_height: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default)]
    pub main_track_magnet: bool,
    pub assets: Vec<MediaAsset>,
    pub clips: Vec<TimelineClip>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub track_flags: [TrackFlags; 3],
    #[serde(default)]
    pub markers: Vec<TimelineMarker>,
    pub total_duration_secs: f32,
    pub next_id: usize,
}
