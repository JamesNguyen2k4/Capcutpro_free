use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use opencut_core::{
    AspectRatio, ClipCrop, ClipKeyframe, TextAnimation, TextClipData, TimelineClip, Track,
    TrackFlags, TrackKind,
};

pub fn preview_dimensions(aspect_ratio: AspectRatio, preview_height: u32) -> (u32, u32) {
    aspect_ratio.dimensions(preview_height.clamp(120, 1080))
}

#[cfg(windows)]
fn apply_no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn apply_no_window(_cmd: &mut Command) {}

pub fn probe_duration_secs(path: &Path) -> Option<f32> {
    let mut cmd = Command::new("ffprobe");
    apply_no_window(&mut cmd);
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ])
    .arg(path)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<f32>().ok().filter(|d| *d > 0.05)
}

pub fn probe_media_dimensions(path: &Path) -> Option<(u32, u32)> {
    if let Ok((w, h)) = image::image_dimensions(path) {
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }
    let mut cmd = Command::new("ffprobe");
    apply_no_window(&mut cmd);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height",
        "-of",
        "csv=s=x:p=0",
    ])
    .arg(path)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.trim().split('x');
    let w = parts.next()?.parse::<u32>().ok()?;
    let h = parts.next()?.parse::<u32>().ok()?;
    if w > 0 && h > 0 {
        Some((w, h))
    } else {
        None
    }
}

fn bgra_bytes_to_render_image(bytes: Vec<u8>, width: u32, height: u32) -> Option<Arc<RenderImage>> {
    let img = RgbaImage::from_raw(width, height, bytes)?;
    let frame = Frame::new(img);
    Some(Arc::new(RenderImage::new(smallvec![frame])))
}

fn append_crop_filter(parts: &mut Vec<String>, crop: ClipCrop) {
    let l = crop.left.clamp(0.0, 0.45);
    let r = crop.right.clamp(0.0, 0.45);
    let t = crop.top.clamp(0.0, 0.45);
    let b = crop.bottom.clamp(0.0, 0.45);
    if l > 0.001 || r > 0.001 || t > 0.001 || b > 0.001 {
        let w = (1.0 - l - r).max(0.1);
        let h = (1.0 - t - b).max(0.1);
        parts.push(format!("crop=iw*{w:.4}:ih*{h:.4}:iw*{l:.4}:ih*{t:.4}"));
    }
}

fn append_flip_rotate_filters(parts: &mut Vec<String>, flip_h: bool, rotation_deg: f32) {
    if flip_h {
        parts.push("hflip".into());
    }
    let norm_rot = rotation_deg.rem_euclid(360.0);
    if norm_rot.abs() > 0.5 && (norm_rot - 360.0).abs() > 0.5 {
        let rad = norm_rot * std::f32::consts::PI / 180.0;
        parts.push(format!(
            "format=rgba,rotate={rad:.5}:ow=rotw({rad:.5}):oh=roth({rad:.5}):fillcolor=none",
            rad = rad
        ));
    }
}

pub fn export_freeze_frame(path: &Path, time_secs: f32) -> Option<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let out_path = std::env::temp_dir().join(format!("opencut_freeze_{}.png", ts));

    let mut cmd = Command::new("ffmpeg");
    apply_no_window(&mut cmd);
    cmd.args([
        "-y",
        "-v",
        "error",
        "-ss",
        &format!("{:.3}", time_secs.max(0.0)),
    ])
    .arg("-i")
    .arg(path)
    .args(["-frames:v", "1"])
    .arg(&out_path)
    .stdout(Stdio::null())
    .stderr(Stdio::null());

    let status = cmd.status().ok()?;
    if status.success() && out_path.exists() {
        Some(out_path)
    } else {
        None
    }
}

pub fn decode_single_frame(
    path: &Path,
    time_secs: f32,
    aspect_ratio: AspectRatio,
    preview_height: u32,
    flip_h: bool,
    rotation_deg: f32,
    crop: ClipCrop,
) -> Option<Arc<RenderImage>> {
    let (pw, ph) = preview_dimensions(aspect_ratio, preview_height);
    let frame_bytes = (pw * ph * 4) as usize;
    let mut vf_parts = Vec::new();
    append_crop_filter(&mut vf_parts, crop);
    append_flip_rotate_filters(&mut vf_parts, flip_h, rotation_deg);
    vf_parts.push(format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
        w = pw,
        h = ph
    ));
    let vf = vf_parts.join(",");

    let mut cmd = Command::new("ffmpeg");
    apply_no_window(&mut cmd);
    cmd.args(["-v", "error", "-ss", &format!("{:.3}", time_secs.max(0.0))])
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &vf,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() || output.stdout.len() < frame_bytes {
        return None;
    }

    let mut buf = output.stdout;
    buf.truncate(frame_bytes);
    bgra_bytes_to_render_image(buf, pw, ph)
}

pub fn render_rotated_text_frame(
    td: &TextClipData,
    font_size_px: f32,
    box_w: u32,
    box_h: u32,
    flip_h: bool,
    rotation_deg: f32,
) -> Option<(Arc<RenderImage>, u32, u32)> {
    let base_w = (box_w + 8).max(32);
    let base_h = (box_h + 8).max(32);
    let norm_rot = rotation_deg.rem_euclid(360.0);
    let rad = norm_rot * std::f32::consts::PI / 180.0;
    let c = rad.cos().abs();
    let s = rad.sin().abs();
    let rot_w = ((base_w as f32 * c + base_h as f32 * s).ceil() as u32).max(16);
    let rot_h = ((base_w as f32 * s + base_h as f32 * c).ceil() as u32).max(16);
    let frame_bytes = (rot_w * rot_h * 4) as usize;

    let display_text = if td.content.is_empty() {
        "Text"
    } else {
        td.content.as_str()
    };
    let escaped = display_text
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'");
    let box_part = if let Some(bg_hex) = td.bg_color_hex {
        format!(
            ":box=1:boxcolor=0x{:06x}@0.75:boxborderw={:.0}",
            bg_hex,
            td.bg_padding.max(2.0)
        )
    } else {
        String::new()
    };
    let border_part = if td.stroke_width > 0.1 {
        let sc = td.stroke_color_hex.unwrap_or(0x000000);
        format!(":borderw={:.1}:bordercolor=0x{:06x}", td.stroke_width * 2.0, sc)
    } else {
        String::new()
    };
    let shadow_part = if let Some(sh_hex) = td.shadow_color_hex {
        format!(
            ":shadowx={:.1}:shadowy={:.1}:shadowcolor=0x{:06x}@0.75",
            td.shadow_offset_x, td.shadow_offset_y, sh_hex
        )
    } else {
        String::new()
    };
    let font_part = resolve_text_font_file(td)
        .map(|p| {
            let sp = p.to_string_lossy().replace('\\', "/").replace(':', "\\:");
            format!(":fontfile='{}'", sp)
        })
        .unwrap_or_default();

    let mut xf = format!(
        "color=c=black@0.0:s={w}x{h}:d=0.1,format=rgba,drawtext=text='{txt}'{fp}:fontsize={fs:.1}:fontcolor=0x{col:06x}:x=(w-text_w)/2:y=(h-text_h)/2{bp}{bd}{sh}",
        w = base_w,
        h = base_h,
        txt = escaped,
        fp = font_part,
        fs = font_size_px.max(8.0),
        col = td.color_hex,
        bp = box_part,
        bd = border_part,
        sh = shadow_part,
    );
    if flip_h {
        xf.push_str(",hflip");
    }
    xf.push_str(&format!(
        ",rotate={rad:.5}:ow={ow}:oh={oh}:fillcolor=none",
        rad = rad,
        ow = rot_w,
        oh = rot_h
    ));

    let mut cmd = Command::new("ffmpeg");
    apply_no_window(&mut cmd);
    cmd.args(["-v", "error", "-f", "lavfi", "-i", &xf])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() || output.stdout.len() < frame_bytes {
        return None;
    }

    let mut buf = output.stdout;
    buf.truncate(frame_bytes);
    let img = bgra_bytes_to_render_image(buf, rot_w, rot_h)?;
    Some((img, rot_w, rot_h))
}

pub struct VideoStreamPlayer {
    stop_flag: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    latest_frame: Arc<Mutex<Option<Arc<RenderImage>>>>,
    active_clip_id: Option<usize>,
    active_ratio: Option<AspectRatio>,
    active_height: Option<u32>,
    active_flip: bool,
    active_rot: f32,
    active_crop: ClipCrop,
    active_reversed: bool,
}

impl VideoStreamPlayer {
    pub fn new() -> Self {
        Self {
            stop_flag: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            latest_frame: Arc::new(Mutex::new(None)),
            active_clip_id: None,
            active_ratio: None,
            active_height: None,
            active_flip: false,
            active_rot: 0.0,
            active_crop: ClipCrop::default(),
            active_reversed: false,
        }
    }

    pub fn active_clip_id(&self) -> Option<usize> {
        self.active_clip_id
    }

    pub fn active_ratio(&self) -> Option<AspectRatio> {
        self.active_ratio
    }

    pub fn active_height(&self) -> Option<u32> {
        self.active_height
    }

    pub fn active_transform(&self) -> (bool, f32, ClipCrop, bool) {
        (self.active_flip, self.active_rot, self.active_crop, self.active_reversed)
    }

    pub fn start(
        &mut self,
        clip_id: usize,
        path: PathBuf,
        start_offset_secs: f32,
        aspect_ratio: AspectRatio,
        preview_height: u32,
        flip_h: bool,
        rotation_deg: f32,
        crop: ClipCrop,
        reversed: bool,
        rev_src_start_secs: f32,
        rev_src_dur_secs: f32,
    ) {
        self.stop();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let latest_frame = Arc::new(Mutex::new(None));

        self.stop_flag = stop_flag.clone();
        self.child = child_slot.clone();
        self.latest_frame = latest_frame.clone();
        self.active_clip_id = Some(clip_id);
        self.active_ratio = Some(aspect_ratio);
        self.active_height = Some(preview_height);
        self.active_flip = flip_h;
        self.active_rot = rotation_deg;
        self.active_crop = crop;
        self.active_reversed = reversed;

        let (pw, ph) = preview_dimensions(aspect_ratio, preview_height);
        let frame_bytes = (pw * ph * 4) as usize;

        std::thread::spawn(move || {
            let mut vf_parts = Vec::new();
            if reversed {
                vf_parts.push("reverse".to_string());
            }
            vf_parts.push("fps=24".to_string());
            append_crop_filter(&mut vf_parts, crop);
            append_flip_rotate_filters(&mut vf_parts, flip_h, rotation_deg);
            vf_parts.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
                w = pw,
                h = ph
            ));
            let vf = vf_parts.join(",");

            let mut cmd = Command::new("ffmpeg");
            apply_no_window(&mut cmd);
            if reversed {
                cmd.args([
                    "-v",
                    "error",
                    "-re",
                    "-ss",
                    &format!("{:.3}", rev_src_start_secs.max(0.0)),
                    "-t",
                    &format!("{:.3}", rev_src_dur_secs.max(0.1)),
                ]);
            } else {
                cmd.args([
                    "-v",
                    "error",
                    "-re",
                    "-ss",
                    &format!("{:.3}", start_offset_secs.max(0.0)),
                ]);
            }
            cmd.arg("-i")
            .arg(&path)
            .args([
                "-vf",
                &vf,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "bgra",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

            let Ok(mut process) = cmd.spawn() else {
                return;
            };
            let Some(mut stdout) = process.stdout.take() else {
                let _ = process.kill();
                return;
            };

            {
                if let Ok(mut guard) = child_slot.lock() {
                    *guard = Some(process);
                }
            }

            let mut buf = vec![0u8; frame_bytes];
            while !stop_flag.load(Ordering::Relaxed) {
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                if let Some(render_img) = bgra_bytes_to_render_image(buf.clone(), pw, ph) {
                    if let Ok(mut guard) = latest_frame.lock() {
                        *guard = Some(render_img);
                    }
                }
            }

            if let Ok(mut guard) = child_slot.lock() {
                if let Some(mut p) = guard.take() {
                    let _ = p.kill();
                    let _ = p.wait();
                }
            }
        });
    }

    pub fn take_latest_frame(&self) -> Option<Arc<RenderImage>> {
        self.latest_frame.lock().ok()?.take()
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.active_clip_id = None;
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut p) = guard.take() {
                let _ = p.kill();
                let _ = p.wait();
            }
        }
    }
}

impl Drop for VideoStreamPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct AudioStreamPlayer {
    active_processes: Vec<(usize, Child)>,
}

impl AudioStreamPlayer {
    pub fn new() -> Self {
        Self {
            active_processes: Vec::new(),
        }
    }

    pub fn sync_playing_clips(
        &mut self,
        clips: &[&TimelineClip],
        playhead_secs: f32,
    ) {
        let desired_ids: Vec<usize> = clips.iter().map(|c| c.id).collect();

        // Stop clips that are no longer active
        let mut i = 0;
        while i < self.active_processes.len() {
            let (id, _) = &self.active_processes[i];
            if !desired_ids.contains(id) {
                let (_, mut child) = self.active_processes.remove(i);
                let _ = child.kill();
                let _ = child.wait();
            } else {
                i += 1;
            }
        }

        // Start newly entered clips
        for clip in clips {
            if self.active_processes.iter().any(|(id, _)| *id == clip.id) {
                continue;
            }
            let Some(path) = &clip.source_path else {
                continue;
            };
            if clip.track == TrackKind::Video && !has_audio_stream(path) {
                continue;
            }

            let local_t = (playhead_secs - clip.start_secs).max(0.0);
            let spd = clip.speed.clamp(0.25, 4.0);
            let remaining_tl = (clip.end_secs() - playhead_secs).max(0.1);
            let offset = clip.source_in_secs + local_t * spd;

            let mut af_parts: Vec<String> = Vec::new();
            if (spd - 1.0).abs() > 0.01 {
                let mut rem = spd;
                while rem > 2.0 {
                    af_parts.push("atempo=2.000".to_string());
                    rem /= 2.0;
                }
                while rem < 0.5 {
                    af_parts.push("atempo=0.500".to_string());
                    rem /= 0.5;
                }
                af_parts.push(format!("atempo={:.3}", rem));
            }
            if clip.fade_in_secs > 0.05 && local_t < clip.fade_in_secs {
                let rem_in = (clip.fade_in_secs - local_t).clamp(0.05, remaining_tl);
                af_parts.push(format!("afade=t=in:st=0:d={rem_in:.3}"));
            }
            if clip.fade_out_secs > 0.05 {
                let fade_d = clip.fade_out_secs.min(clip.duration_secs);
                let out_start_tl = (clip.duration_secs - fade_d).max(0.0);
                if local_t >= out_start_tl {
                    let rem_out = (clip.duration_secs - local_t).max(0.05);
                    af_parts.push(format!("afade=t=out:st=0:d={rem_out:.3}"));
                } else {
                    let rel_st = (out_start_tl - local_t).max(0.0);
                    af_parts.push(format!("afade=t=out:st={rel_st:.3}:d={fade_d:.3}"));
                }
            }

            let mut cmd = Command::new("ffplay");
            apply_no_window(&mut cmd);
            cmd.args([
                "-nodisp",
                "-autoexit",
                "-vn",
                "-loglevel",
                "quiet",
                "-ss",
                &format!("{:.3}", offset.max(0.0)),
                "-t",
                &format!("{:.3}", remaining_tl),
                "-volume",
                &clip.volume.to_string(),
            ]);
            if !af_parts.is_empty() {
                cmd.args(["-af", &af_parts.join(",")]);
            }
            cmd.arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

            if let Ok(child) = cmd.spawn() {
                self.active_processes.push((clip.id, child));
            }
        }
    }

    pub fn stop(&mut self) {
        for (_, mut child) in self.active_processes.drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for AudioStreamPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn has_audio_stream(path: &Path) -> bool {
    let mut cmd = Command::new("ffprobe");
    apply_no_window(&mut cmd);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=index",
        "-of",
        "csv=p=0",
    ])
    .arg(path)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    cmd.output()
        .map(|out| out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty())
        .unwrap_or(false)
}

pub fn reveal_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let path_str = resolved.to_string_lossy().replace('/', "\\");
        let clean_path = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);

        if resolved.exists() {
            let _ = Command::new("explorer")
                .raw_arg(format!("/select,\"{}\"", clean_path))
                .spawn();
        } else if let Some(parent) = resolved.parent() {
            let _ = Command::new("explorer").arg(parent).spawn();
        }
    }
}

#[derive(Clone)]
pub struct ExportHandle {
    pub output_path: PathBuf,
    pub progress_pct: Arc<Mutex<f32>>,
    pub finished: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
}

fn resolve_text_font_file(td: &TextClipData) -> Option<PathBuf> {
    if let Some(ref p) = td.font_path {
        if p.exists() {
            return Some(p.clone());
        }
    }
    let filename = match td.font_family.as_str() {
        "Be Vietnam Pro" => match td.font_weight {
            0..=350 => "BeVietnamPro-Light.ttf",
            351..=450 => "BeVietnamPro-Regular.ttf",
            451..=600 => "BeVietnamPro-Medium.ttf",
            601..=800 => "BeVietnamPro-Bold.ttf",
            _ => "BeVietnamPro-Black.ttf",
        },
        "Roboto" => "Roboto-Bold.ttf",
        "Montserrat" => "Montserrat-Bold.ttf",
        "Oswald" => "Oswald-Bold.ttf",
        "Playfair Display" => "PlayfairDisplay-Bold.ttf",
        "Quicksand" => "Quicksand-Bold.ttf",
        "Pacifico" => "Pacifico-Regular.ttf",
        "Lobster" => "Lobster-Regular.ttf",
        "Space Grotesk" => "SpaceGrotesk-Bold.ttf",
        "Merriweather" => "Merriweather-Bold.ttf",
        _ => "BeVietnamPro-Bold.ttf",
    };
    let candidates = [
        PathBuf::from("apps/desktop/assets/fonts").join(filename),
        PathBuf::from("assets/fonts").join(filename),
    ];
    candidates.into_iter().find(|p| p.exists())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Mp4H264,
    Mp4Hevc,
    MovH264,
    AudioMp3,
    AudioWav,
}

impl ExportFormat {
    pub const fn all() -> &'static [ExportFormat] {
        &[
            ExportFormat::Mp4H264,
            ExportFormat::Mp4Hevc,
            ExportFormat::MovH264,
            ExportFormat::AudioMp3,
            ExportFormat::AudioWav,
        ]
    }

    pub const fn label(&self) -> &'static str {
        match self {
            ExportFormat::Mp4H264 => "MP4 (H.264)",
            ExportFormat::Mp4Hevc => "MP4 (HEVC)",
            ExportFormat::MovH264 => "MOV (H.264)",
            ExportFormat::AudioMp3 => "Audio (MP3)",
            ExportFormat::AudioWav => "Audio (WAV)",
        }
    }

    pub const fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Mp4H264 | ExportFormat::Mp4Hevc => "mp4",
            ExportFormat::MovH264 => "mov",
            ExportFormat::AudioMp3 => "mp3",
            ExportFormat::AudioWav => "wav",
        }
    }

    pub const fn is_audio_only(&self) -> bool {
        matches!(self, ExportFormat::AudioMp3 | ExportFormat::AudioWav)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub height: u32,
    pub fps: u32,
    pub crf: u8,
    pub use_gpu: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Mp4H264,
            height: 1080,
            fps: 30,
            crf: 22,
            use_gpu: true,
        }
    }
}

fn probe_encoder_works(encoder: &str) -> bool {
    let mut cmd = Command::new("ffmpeg");
    apply_no_window(&mut cmd);
    cmd.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=128x128:d=0.1",
        "-frames:v",
        "1",
        "-c:v",
        encoder,
        "-f",
        "null",
        "-",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

pub fn detect_gpu_encoder(is_hevc: bool) -> Option<&'static str> {
    static H264_GPU: OnceLock<Option<&'static str>> = OnceLock::new();
    static HEVC_GPU: OnceLock<Option<&'static str>> = OnceLock::new();

    if is_hevc {
        *HEVC_GPU.get_or_init(|| {
            ["hevc_nvenc", "hevc_qsv", "hevc_amf", "hevc_videotoolbox"]
                .into_iter()
                .find(|enc| probe_encoder_works(enc))
        })
    } else {
        *H264_GPU.get_or_init(|| {
            ["h264_nvenc", "h264_qsv", "h264_amf", "h264_videotoolbox"]
                .into_iter()
                .find(|enc| probe_encoder_works(enc))
        })
    }
}

fn keyframes_vary(keyframes: &[ClipKeyframe], extract: fn(&ClipKeyframe) -> f32) -> bool {
    if keyframes.len() < 2 {
        return false;
    }
    let first = extract(&keyframes[0]);
    keyframes.iter().any(|k| (extract(k) - first).abs() > 0.01)
}

fn build_keyframe_expr_with_var(
    keyframes: &[ClipKeyframe],
    clip_start: f32,
    default_val: f32,
    time_var: &str,
    extract: fn(&ClipKeyframe) -> f32,
) -> String {
    if keyframes.is_empty() {
        return format!("{:.2}", default_val);
    }
    if keyframes.len() == 1 {
        return format!("{:.2}", extract(&keyframes[0]));
    }
    let last_val = extract(&keyframes[keyframes.len() - 1]);
    let mut expr = format!("{:.2}", last_val);
    for pair in keyframes.windows(2).rev() {
        let a = &pair[0];
        let b = &pair[1];
        let t0 = clip_start + a.time_offset_secs;
        let t1 = clip_start + b.time_offset_secs;
        let v0 = extract(a);
        let v1 = extract(b);
        let dt = (t1 - t0).max(0.001);
        let interp = format!("{v0:.2}+({v1:.2}-({v0:.2}))*max(0,{time_var}-{t0:.3})/{dt:.3}");
        expr = format!("if(lt({time_var},{t1:.3}),{interp},{expr})");
    }
    let first_t = clip_start + keyframes[0].time_offset_secs;
    let first_v = extract(&keyframes[0]);
    format!("if(lt({time_var},{first_t:.3}),{first_v:.2},{expr})")
}

fn build_keyframe_angle_expr(
    keyframes: &[ClipKeyframe],
    clip_start: f32,
    default_val: f32,
) -> String {
    if keyframes.is_empty() {
        return format!("{:.2}", default_val);
    }
    if keyframes.len() == 1 {
        return format!("{:.2}", keyframes[0].rotation_deg);
    }
    let last_val = keyframes[keyframes.len() - 1].rotation_deg;
    let mut expr = format!("{:.2}", last_val);
    for pair in keyframes.windows(2).rev() {
        let a = &pair[0];
        let b = &pair[1];
        let t0 = clip_start + a.time_offset_secs;
        let t1 = clip_start + b.time_offset_secs;
        let v0 = a.rotation_deg;
        let mut diff = (b.rotation_deg - v0).rem_euclid(360.0);
        if diff > 180.0 {
            diff -= 360.0;
        }
        let dt = (t1 - t0).max(0.001);
        let interp = format!("{v0:.2}+({diff:.2})*max(0,t-{t0:.3})/{dt:.3}");
        expr = format!("if(lt(t,{t1:.3}),{interp},{expr})");
    }
    let first_t = clip_start + keyframes[0].time_offset_secs;
    let first_v = keyframes[0].rotation_deg;
    format!("if(lt(t,{first_t:.3}),{first_v:.2},{expr})")
}

fn build_keyframe_expr(
    keyframes: &[ClipKeyframe],
    clip_start: f32,
    default_val: f32,
    extract: fn(&ClipKeyframe) -> f32,
) -> String {
    build_keyframe_expr_with_var(keyframes, clip_start, default_val, "t", extract)
}

fn build_audio_filter_chain(clip: &TimelineClip) -> String {
    let spd = clip.speed.clamp(0.25, 4.0);
    let src_dur = clip.duration_secs * spd;
    let delay_ms = (clip.start_secs * 1000.0).round() as u64;
    let vol = clip.volume as f32 / 100.0;

    let mut af_parts = vec![
        format!("atrim=0:{:.3}", src_dur),
        "asetpts=PTS-STARTPTS".to_string(),
    ];
    if clip.reversed {
        af_parts.push("areverse".to_string());
    }
    if (spd - 1.0).abs() > 0.01 {
        let mut rem = spd;
        while rem > 2.0 {
            af_parts.push("atempo=2.000".to_string());
            rem /= 2.0;
        }
        while rem < 0.5 {
            af_parts.push("atempo=0.500".to_string());
            rem /= 0.5;
        }
        af_parts.push(format!("atempo={:.3}", rem));
    }
    af_parts.push(format!("volume={:.2}", vol));
    if clip.fade_in_secs > 0.05 {
        let d = clip.fade_in_secs.min(clip.duration_secs);
        af_parts.push(format!("afade=t=in:st=0:d={d:.3}"));
    }
    if clip.fade_out_secs > 0.05 {
        let d = clip.fade_out_secs.min(clip.duration_secs);
        let st = (clip.duration_secs - d).max(0.0);
        af_parts.push(format!("afade=t=out:st={st:.3}:d={d:.3}"));
    }
    af_parts.push(format!("adelay={delay_ms}|{delay_ms}"));
    af_parts.join(",")
}

fn append_visual_clip_filters(
    filter_parts: &mut Vec<String>,
    last_video_label: &mut String,
    video_step: &mut usize,
    input_idx: usize,
    clip: &TimelineClip,
    out_w: u32,
    out_h: u32,
    res_scale: f32,
    is_video: bool,
) {
    let v_prep = format!("vp{}", *video_step);
    let v_out = format!("vout{}", *video_step);
    *video_step += 1;

    let spd = if is_video {
        clip.speed.clamp(0.25, 4.0)
    } else {
        1.0
    };

    let mut vf_parts: Vec<String> = Vec::new();
    if is_video && clip.reversed {
        vf_parts.push("reverse".into());
    }
    append_crop_filter(&mut vf_parts, clip.crop);
    if clip.flip_h {
        vf_parts.push("hflip".into());
    }
    if clip.flip_v {
        vf_parts.push("vflip".into());
    }

    // Shift PTS to timeline time first so dynamic expressions using `t` / `T` match timeline seconds
    vf_parts.push(format!(
        "setpts=(PTS-STARTPTS)/{spd:.3}+{st:.3}/TB",
        spd = spd,
        st = clip.start_secs
    ));

    let scale_varies = keyframes_vary(&clip.keyframes, |k| k.scale as f32);
    if scale_varies {
        let sc_expr = build_keyframe_expr(
            &clip.keyframes,
            clip.start_secs,
            clip.scale as f32,
            |k| k.scale as f32,
        );
        vf_parts.push(format!(
            "scale=w='max(16,trunc({out_w}*({sc_expr})/100/2)*2)':h='max(16,trunc({out_h}*({sc_expr})/100/2)*2)':force_original_aspect_ratio=decrease:flags=lanczos:eval=frame"
        ));
    } else {
        let sc_val = if clip.keyframes.len() == 1 {
            clip.keyframes[0].scale
        } else {
            clip.scale
        };
        let scale_factor = (sc_val as f32 / 100.0).clamp(0.1, 2.0);
        let target_w = ((out_w as f32 * scale_factor) as u32).max(16) & !1;
        let target_h = ((out_h as f32 * scale_factor) as u32).max(16) & !1;
        vf_parts.push(format!(
            "scale={target_w}:{target_h}:force_original_aspect_ratio=decrease:flags=lanczos"
        ));
    }

    let rot_varies = keyframes_vary(&clip.keyframes, |k| k.rotation_deg);
    if rot_varies {
        let rot_expr = build_keyframe_angle_expr(
            &clip.keyframes,
            clip.start_secs,
            clip.rotation_deg,
        );
        vf_parts.push(format!(
            "format=rgba,rotate='({rot_expr})*PI/180':ow='hypot(iw,ih)':oh='hypot(iw,ih)':fillcolor=none"
        ));
    } else {
        let rot_val = if clip.keyframes.len() == 1 {
            clip.keyframes[0].rotation_deg
        } else {
            clip.rotation_deg
        };
        append_flip_rotate_filters(&mut vf_parts, false, rot_val);
    }

    let op_varies = keyframes_vary(&clip.keyframes, |k| k.opacity as f32);
    if op_varies {
        let op_expr = build_keyframe_expr_with_var(
            &clip.keyframes,
            clip.start_secs,
            clip.opacity as f32,
            "T",
            |k| k.opacity as f32,
        );
        vf_parts.push(format!(
            "format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='alpha(X,Y)*({op_expr})/100'"
        ));
    } else {
        let op_val = if clip.keyframes.len() == 1 {
            clip.keyframes[0].opacity
        } else {
            clip.opacity
        };
        if op_val < 100 {
            let opacity = (op_val as f32 / 100.0).clamp(0.0, 1.0);
            vf_parts.push(format!("format=yuva420p,colorchannelmixer=aa={opacity:.2}"));
        } else if is_video {
            vf_parts.push("format=yuv420p".into());
        }
    }

    let ox_raw = build_keyframe_expr(
        &clip.keyframes,
        clip.start_secs,
        clip.pos_x,
        |k| k.pos_x,
    );
    let oy_raw = build_keyframe_expr(
        &clip.keyframes,
        clip.start_secs,
        clip.pos_y,
        |k| k.pos_y,
    );
    let ox_expr = if (res_scale - 1.0).abs() > 0.001 {
        format!("({ox_raw})*{res_scale:.4}")
    } else {
        ox_raw
    };
    let oy_expr = if (res_scale - 1.0).abs() > 0.001 {
        format!("({oy_raw})*{res_scale:.4}")
    } else {
        oy_raw
    };

    filter_parts.push(format!(
        "[{input_idx}:v]{chain}[{v_prep}]",
        chain = vf_parts.join(",")
    ));
    filter_parts.push(format!(
        "{prev}[{vp}]overlay=x='(W-w)/2+({ox})':y='(H-h)/2+({oy})':enable='between(t,{st:.3},{en:.3})'[{vo}]",
        prev = *last_video_label,
        vp = v_prep,
        ox = ox_expr,
        oy = oy_expr,
        st = clip.start_secs,
        en = clip.end_secs(),
        vo = v_out
    ));
    *last_video_label = format!("[{}]", v_out);
}

pub fn start_export_timeline(
    clips: Vec<TimelineClip>,
    tracks: Vec<Track>,
    aspect_ratio: AspectRatio,
    base_height: u32,
    options: ExportOptions,
    output_path: PathBuf,
) -> ExportHandle {
    let progress_pct = Arc::new(Mutex::new(0.0_f32));
    let finished = Arc::new(AtomicBool::new(false));
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let handle = ExportHandle {
        output_path: output_path.clone(),
        progress_pct: progress_pct.clone(),
        finished: finished.clone(),
        error: error.clone(),
    };

    std::thread::spawn(move || {
        let resolve_flags = |clip: &TimelineClip| -> TrackFlags {
            tracks
                .iter()
                .find(|t| t.id == clip.track_id)
                .or_else(|| tracks.iter().find(|t| t.kind == clip.track))
                .map(|t| t.flags)
                .unwrap_or_default()
        };

        let valid_clips: Vec<TimelineClip> = clips
            .into_iter()
            .filter(|c| {
                let flags = resolve_flags(c);
                let visible = c.track == TrackKind::Audio || !flags.hidden;
                let audible = c.track != TrackKind::Audio || !flags.muted;
                let has_media = c.source_path.is_some() || (c.track == TrackKind::Text && c.text.is_some());
                has_media && c.duration_secs > 0.05 && (visible || audible)
            })
            .collect();

        if valid_clips.is_empty() {
            if let Ok(mut e) = error.lock() {
                *e = Some("Timeline has no valid media clips to export".into());
            }
            finished.store(true, Ordering::Relaxed);
            return;
        }

        let export_duration = valid_clips
            .iter()
            .map(|c| c.end_secs())
            .fold(0.0_f32, f32::max)
            .max(0.5);

        let (out_w, out_h) = aspect_ratio.dimensions(options.height.max(360));
        let res_scale = options.height.max(360) as f32 / base_height.max(360) as f32;
        let fps = options.fps.clamp(15, 60);
        let audio_only = options.format.is_audio_only();

        let mut cmd = Command::new("ffmpeg");
        apply_no_window(&mut cmd);

        // Input 0: black canvas matching project aspect ratio (used in video modes)
        if !audio_only {
            cmd.args([
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "color=c=black:s={}x{}:r={}:d={:.3}",
                    out_w, out_h, fps, export_duration
                ),
            ]);
        }

        // Input 0 (audio-only) or Input 1 (video): silent stereo audio base
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("anullsrc=r=44100:cl=stereo:d={:.3}", export_duration),
        ]);

        let mut filter_parts: Vec<String> = Vec::new();
        let mut last_video_label = "[0:v]".to_string();
        let base_audio_label = if audio_only { "[0:a]" } else { "[1:a]" };
        let mut audio_labels: Vec<String> = vec![base_audio_label.to_string()];
        let mut video_step = 0usize;
        let mut audio_step = 0usize;

        // Sort so Video renders before Overlay
        let mut sorted_clips = valid_clips.clone();
        sorted_clips.sort_by_key(|c| match c.track {
            TrackKind::Video => (0, (c.start_secs * 100.0) as i64),
            TrackKind::Overlay => (1, (c.start_secs * 100.0) as i64),
            TrackKind::Audio => (2, (c.start_secs * 100.0) as i64),
            TrackKind::Text => (3, (c.start_secs * 100.0) as i64),
            TrackKind::Adjustment => (4, (c.start_secs * 100.0) as i64),
        });

        let mut next_input_idx = if audio_only { 1usize } else { 2usize };
        for clip in &sorted_clips {
            let flags = resolve_flags(clip);

            if clip.track == TrackKind::Text {
                if !audio_only && !flags.hidden {
                    if let Some(td) = &clip.text {
                        if !td.content.is_empty() {
                            let v_out = format!("vout{}", video_step);
                            video_step += 1;
                            let escaped = td
                                .content
                                .replace('\\', "\\\\")
                                .replace(':', "\\:")
                                .replace('\'', "\\'");
                            let alpha = (clip.opacity as f32 / 100.0).clamp(0.0, 1.0);
                            let box_part = if let Some(bg_hex) = td.bg_color_hex {
                                format!(
                                    ":box=1:boxcolor=0x{:06x}@0.75:boxborderw={:.0}",
                                    bg_hex,
                                    (td.bg_padding.max(2.0) * res_scale)
                                )
                            } else {
                                String::new()
                            };
                            let border_part = if td.stroke_width > 0.1 {
                                let sc = td.stroke_color_hex.unwrap_or(0x000000);
                                format!(
                                    ":borderw={:.1}:bordercolor=0x{:06x}",
                                    (td.stroke_width * res_scale),
                                    sc
                                )
                            } else {
                                String::new()
                            };
                            let shadow_part = if let Some(sh_hex) = td.shadow_color_hex {
                                format!(
                                    ":shadowx={:.1}:shadowy={:.1}:shadowcolor=0x{:06x}@0.75",
                                    td.shadow_offset_x * res_scale,
                                    td.shadow_offset_y * res_scale,
                                    sh_hex
                                )
                            } else {
                                String::new()
                            };
                            let font_part = resolve_text_font_file(td)
                                .map(|p| {
                                    let s = p.to_string_lossy().replace('\\', "/").replace(':', "\\:");
                                    format!(":fontfile='{}'", s)
                                })
                                .unwrap_or_default();
                            let st = clip.start_secs;
                            let en = clip.end_secs();
                            let ad = td.anim_duration_secs.clamp(0.1, (clip.duration_secs * 0.5).max(0.1));
                            let base_fs =
                                (td.font_size * (clip.scale as f32 / 100.0) * res_scale).max(12.0);
                            let ox_raw = build_keyframe_expr(
                                &clip.keyframes,
                                clip.start_secs,
                                clip.pos_x,
                                |k| k.pos_x,
                            );
                            let oy_raw = build_keyframe_expr(
                                &clip.keyframes,
                                clip.start_secs,
                                clip.pos_y,
                                |k| k.pos_y,
                            );
                            let ox_expr = if (res_scale - 1.0).abs() > 0.001 {
                                format!("({ox_raw})*{res_scale:.4}")
                            } else {
                                ox_raw
                            };
                            let oy_expr = if (res_scale - 1.0).abs() > 0.001 {
                                format!("({oy_raw})*{res_scale:.4}")
                            } else {
                                oy_raw
                            };
                            let op_varies = keyframes_vary(&clip.keyframes, |k| k.opacity as f32);
                            let kf_op_factor = if op_varies {
                                let op_e = build_keyframe_expr(
                                    &clip.keyframes,
                                    clip.start_secs,
                                    clip.opacity as f32,
                                    |k| k.opacity as f32,
                                );
                                format!("(({op_e})/100)")
                            } else {
                                let op_val = if clip.keyframes.len() == 1 {
                                    clip.keyframes[0].opacity
                                } else {
                                    clip.opacity
                                };
                                format!("{:.2}", (op_val as f32 / 100.0).clamp(0.0, 1.0))
                            };
                            let anim_alpha_expr = match td.animation {
                                TextAnimation::FadeIn | TextAnimation::PopIn => {
                                    format!("if(lt(t-{st:.3},{ad:.3}),(t-{st:.3})/{ad:.3},1)")
                                }
                                TextAnimation::FadeOut => {
                                    format!("if(gt(t,{en:.3}-{ad:.3}),({en:.3}-t)/{ad:.3},1)")
                                }
                                TextAnimation::FadeInOut => {
                                    format!("if(lt(t-{st:.3},{ad:.3}),(t-{st:.3})/{ad:.3},if(gt(t,{en:.3}-{ad:.3}),({en:.3}-t)/{ad:.3},1))")
                                }
                                TextAnimation::None | TextAnimation::SlideUp => String::new(),
                            };
                            let alpha_expr = match (op_varies, anim_alpha_expr.is_empty()) {
                                (true, true) => format!(":alpha='{kf_op_factor}'"),
                                (true, false) => format!(":alpha='({kf_op_factor})*({anim_alpha_expr})'"),
                                (false, false) => format!(":alpha='{anim_alpha_expr}'"),
                                (false, true) => String::new(),
                            };
                            let slide_dist = 80.0 * res_scale;
                            let y_expr = match td.animation {
                                TextAnimation::SlideUp => format!(
                                    "(h-text_h)/2+({oy_expr})+if(lt(t-{st:.3},{ad:.3}),(1-(t-{st:.3})/{ad:.3})*{slide_dist:.1},0)"
                                ),
                                _ => format!("(h-text_h)/2+({oy_expr})"),
                            };
                            let norm_rot = clip.rotation_deg.rem_euclid(360.0);
                            let rot_varies = keyframes_vary(&clip.keyframes, |k| k.rotation_deg);
                            let has_rot_or_flip =
                                clip.flip_h || rot_varies || (norm_rot > 0.5 && norm_rot < 359.5);
                            if has_rot_or_flip {
                                let txt_raw = format!("txt_raw_{}", clip.id);
                                let txt_rot = format!("txt_xf_{}", clip.id);
                                let mut xf_parts = Vec::new();
                                if clip.flip_h {
                                    xf_parts.push("hflip".into());
                                }
                                if rot_varies {
                                    let rot_expr = build_keyframe_angle_expr(
                                        &clip.keyframes,
                                        clip.start_secs,
                                        clip.rotation_deg,
                                    );
                                    xf_parts.push(format!(
                                        "format=rgba,rotate='({rot_expr})*PI/180':ow='hypot(iw,ih)':oh='hypot(iw,ih)':fillcolor=none"
                                    ));
                                } else {
                                    append_flip_rotate_filters(&mut xf_parts, false, clip.rotation_deg);
                                }
                                let xf_chain = xf_parts.join(",");
                                let draw_op = if op_varies { 1.0 } else {
                                    let op_val = if clip.keyframes.len() == 1 { clip.keyframes[0].opacity } else { clip.opacity };
                                    (op_val as f32 / 100.0).clamp(0.0, 1.0)
                                };
                                filter_parts.push(format!(
                                    "color=c=black@0.0:s={w}x{h}:d={dur:.3},format=rgba,drawtext=text='{txt}'{fp}:fontsize={fs:.0}:fontcolor=0x{col:06x}@{op:.2}:x=(w-text_w)/2:y=(h-text_h)/2{ae}{bp}{bd}{sh}[{tr}];[{tr}]{xf}[{tx}]",
                                    w = out_w,
                                    h = out_h,
                                    dur = en.max(0.1),
                                    txt = escaped,
                                    fp = font_part,
                                    fs = base_fs,
                                    col = td.color_hex,
                                    op = draw_op,
                                    ae = alpha_expr,
                                    bp = box_part,
                                    bd = border_part,
                                    sh = shadow_part,
                                    tr = txt_raw,
                                    xf = xf_chain,
                                    tx = txt_rot,
                                ));
                                filter_parts.push(format!(
                                    "{prev}[{tx}]overlay=x='(W-w)/2+({ox})':y='(H-h)/2+({oy})':enable='between(t,{st:.3},{en:.3})'[{vo}]",
                                    prev = last_video_label,
                                    tx = txt_rot,
                                    ox = ox_expr,
                                    oy = oy_expr,
                                    st = st,
                                    en = en,
                                    vo = v_out,
                                ));
                            } else {
                                let draw_op = if op_varies { 1.0 } else { alpha };
                                filter_parts.push(format!(
                                    "{prev}drawtext=text='{txt}'{fp}:fontsize={fs:.0}:fontcolor=0x{col:06x}@{op:.2}:x='(w-text_w)/2+({ox})':y='{y}'{ae}{bp}{bd}{sh}:enable='between(t,{st:.3},{en:.3})'[{vo}]",
                                    prev = last_video_label,
                                    txt = escaped,
                                    fp = font_part,
                                    fs = base_fs,
                                    col = td.color_hex,
                                    op = draw_op,
                                    ox = ox_expr,
                                    y = y_expr,
                                    ae = alpha_expr,
                                    bp = box_part,
                                    bd = border_part,
                                    sh = shadow_part,
                                    st = st,
                                    en = en,
                                    vo = v_out
                                ));
                            }
                            last_video_label = format!("[{}]", v_out);
                        }
                    }
                }
                continue;
            }

            let Some(path) = &clip.source_path else {
                continue;
            };
            let input_idx = next_input_idx;
            next_input_idx += 1;

            match clip.track {
                TrackKind::Video => {
                    let src_dur = clip.duration_secs * clip.speed.clamp(0.25, 4.0);
                    cmd.args([
                        "-ss",
                        &format!("{:.3}", clip.source_in_secs.max(0.0)),
                        "-t",
                        &format!("{:.3}", src_dur),
                    ])
                    .arg("-i")
                    .arg(path);

                    if !audio_only && !flags.hidden {
                        append_visual_clip_filters(
                            &mut filter_parts,
                            &mut last_video_label,
                            &mut video_step,
                            input_idx,
                            clip,
                            out_w,
                            out_h,
                            res_scale,
                            true,
                        );
                    }

                    if !flags.muted && clip.volume > 0 && has_audio_stream(path) {
                        let a_label = format!("a{}", audio_step);
                        audio_step += 1;
                        let af = build_audio_filter_chain(clip);
                        filter_parts.push(format!("[{input_idx}:a]{af}[{a_label}]"));
                        audio_labels.push(format!("[{}]", a_label));
                    }
                }
                TrackKind::Overlay => {
                    if audio_only {
                        next_input_idx -= 1;
                        continue;
                    }
                    cmd.args(["-loop", "1", "-t", &format!("{:.3}", clip.duration_secs)])
                        .arg("-i")
                        .arg(path);

                    if !flags.hidden {
                        append_visual_clip_filters(
                            &mut filter_parts,
                            &mut last_video_label,
                            &mut video_step,
                            input_idx,
                            clip,
                            out_w,
                            out_h,
                            res_scale,
                            false,
                        );
                    }
                }
                TrackKind::Audio => {
                    let src_dur = clip.duration_secs * clip.speed.clamp(0.25, 4.0);
                    cmd.args([
                        "-ss",
                        &format!("{:.3}", clip.source_in_secs.max(0.0)),
                        "-t",
                        &format!("{:.3}", src_dur),
                    ])
                    .arg("-i")
                    .arg(path);

                    if !flags.muted && clip.volume > 0 {
                        let a_label = format!("a{}", audio_step);
                        audio_step += 1;
                        let af = build_audio_filter_chain(clip);
                        filter_parts.push(format!("[{input_idx}:a]{af}[{a_label}]"));
                        audio_labels.push(format!("[{}]", a_label));
                    }
                }
                TrackKind::Text | TrackKind::Adjustment => {}
            }
        }

        // Mix audio tracks
        let final_audio_label = if audio_labels.len() > 1 {
            let count = audio_labels.len();
            filter_parts.push(format!(
                "{}amix=inputs={}:duration=first:normalize=0[aout]",
                audio_labels.join(""),
                count
            ));
            "[aout]".to_string()
        } else {
            base_audio_label.to_string()
        };

        if audio_only {
            let filter_complex = filter_parts.join(";");
            if !filter_complex.is_empty() {
                cmd.args(["-filter_complex", &filter_complex]);
            }
            cmd.args(["-map", &final_audio_label]);
            match options.format {
                ExportFormat::AudioMp3 => {
                    cmd.args(["-c:a", "libmp3lame", "-b:a", "320k"]);
                }
                ExportFormat::AudioWav => {
                    cmd.args(["-c:a", "pcm_s16le"]);
                }
                _ => {}
            }
            cmd.args([
                "-t",
                &format!("{:.3}", export_duration),
                "-progress",
                "pipe:1",
                "-nostats",
                "-y",
            ])
            .arg(&output_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        } else {
            // Ensure pixel format yuv420p at end of video chain
            filter_parts.push(format!("{}format=yuv420p[vfinal]", last_video_label));
            let filter_complex = filter_parts.join(";");

            let is_hevc = options.format == ExportFormat::Mp4Hevc;
            let crf_val = options.crf.clamp(14, 32);
            let gpu_enc = if options.use_gpu {
                detect_gpu_encoder(is_hevc)
            } else {
                None
            };

            cmd.args(["-filter_complex", &filter_complex])
                .args(["-map", "[vfinal]", "-map", &final_audio_label]);

            match gpu_enc {
                Some(enc) if enc.ends_with("_nvenc") => {
                    cmd.args([
                        "-c:v",
                        enc,
                        "-preset",
                        "p4",
                        "-rc",
                        "vbr",
                        "-cq",
                        &crf_val.to_string(),
                    ]);
                }
                Some(enc) if enc.ends_with("_qsv") => {
                    cmd.args([
                        "-c:v",
                        enc,
                        "-preset",
                        "medium",
                        "-global_quality",
                        &crf_val.to_string(),
                    ]);
                }
                Some(enc) if enc.ends_with("_amf") => {
                    let qp = crf_val.to_string();
                    cmd.args([
                        "-c:v",
                        enc,
                        "-quality",
                        "balanced",
                        "-rc",
                        "cqp",
                        "-qp_i",
                        &qp,
                        "-qp_p",
                        &qp,
                    ]);
                }
                Some(enc) if enc.ends_with("_videotoolbox") => {
                    let q = match crf_val {
                        0..=19 => "75",
                        20..=24 => "65",
                        _ => "55",
                    };
                    cmd.args(["-c:v", enc, "-q:v", q]);
                }
                _ => {
                    let v_codec = if is_hevc { "libx265" } else { "libx264" };
                    cmd.args([
                        "-c:v",
                        v_codec,
                        "-preset",
                        "fast",
                        "-crf",
                        &crf_val.to_string(),
                    ]);
                }
            }
            if is_hevc {
                cmd.args(["-tag:v", "hvc1"]);
            }
            cmd.args([
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-t",
                &format!("{:.3}", export_duration),
                "-movflags",
                "+faststart",
                "-progress",
                "pipe:1",
                "-nostats",
                "-y",
            ])
            .arg(&output_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        }

        let Ok(mut process) = cmd.spawn() else {
            if let Ok(mut e) = error.lock() {
                *e = Some("Failed to start ffmpeg".into());
            }
            finished.store(true, Ordering::Relaxed);
            return;
        };

        if let Some(stdout) = process.stdout.take() {
            let reader = BufReader::new(stdout);
            let total_us = (export_duration as f64) * 1_000_000.0;

            for line in reader.lines().map_while(Result::ok) {
                if let Some(val) = line.strip_prefix("out_time_us=") {
                    if let Ok(us) = val.trim().parse::<f64>() {
                        let pct = ((us / total_us) * 100.0).clamp(0.0, 99.5) as f32;
                        if let Ok(mut p) = progress_pct.lock() {
                            *p = pct;
                        }
                    }
                } else if line.trim() == "progress=end" {
                    if let Ok(mut p) = progress_pct.lock() {
                        *p = 100.0;
                    }
                }
            }
        }

        let output = process.wait_with_output();
        match output {
            Ok(out) if out.status.success() => {
                if let Ok(mut p) = progress_pct.lock() {
                    *p = 100.0;
                }
            }
            Ok(out) => {
                let err_msg = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("FFmpeg export failed")
                    .to_string();
                if let Ok(mut e) = error.lock() {
                    *e = Some(err_msg);
                }
            }
            Err(err) => {
                if let Ok(mut e) = error.lock() {
                    *e = Some(err.to_string());
                }
            }
        }

        finished.store(true, Ordering::Relaxed);
    });

    handle
}
