use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use opencut_core::{AspectRatio, TextAnimation, TextClipData, TimelineClip, Track, TrackFlags, TrackKind};

pub fn preview_dimensions(aspect_ratio: AspectRatio) -> (u32, u32) {
    aspect_ratio.dimensions(360)
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

fn bgra_bytes_to_render_image(bytes: Vec<u8>, width: u32, height: u32) -> Option<Arc<RenderImage>> {
    let img = RgbaImage::from_raw(width, height, bytes)?;
    let frame = Frame::new(img);
    Some(Arc::new(RenderImage::new(smallvec![frame])))
}

pub fn decode_single_frame(
    path: &Path,
    time_secs: f32,
    aspect_ratio: AspectRatio,
) -> Option<Arc<RenderImage>> {
    let (pw, ph) = preview_dimensions(aspect_ratio);
    let frame_bytes = (pw * ph * 4) as usize;
    let vf = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
        w = pw,
        h = ph
    );

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

pub struct VideoStreamPlayer {
    stop_flag: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    latest_frame: Arc<Mutex<Option<Arc<RenderImage>>>>,
    active_clip_id: Option<usize>,
    active_ratio: Option<AspectRatio>,
}

impl VideoStreamPlayer {
    pub fn new() -> Self {
        Self {
            stop_flag: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            latest_frame: Arc::new(Mutex::new(None)),
            active_clip_id: None,
            active_ratio: None,
        }
    }

    pub fn active_clip_id(&self) -> Option<usize> {
        self.active_clip_id
    }

    pub fn active_ratio(&self) -> Option<AspectRatio> {
        self.active_ratio
    }

    pub fn start(
        &mut self,
        clip_id: usize,
        path: PathBuf,
        start_offset_secs: f32,
        aspect_ratio: AspectRatio,
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

        let (pw, ph) = preview_dimensions(aspect_ratio);
        let frame_bytes = (pw * ph * 4) as usize;

        std::thread::spawn(move || {
            let vf = format!(
                "fps=24,scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
                w = pw,
                h = ph
            );

            let mut cmd = Command::new("ffmpeg");
            apply_no_window(&mut cmd);
            cmd.args([
                "-v",
                "error",
                "-re",
                "-ss",
                &format!("{:.3}", start_offset_secs.max(0.0)),
            ])
            .arg("-i")
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

            let offset = clip.source_in_secs + (playhead_secs - clip.start_secs).max(0.0);
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
                &format!("{:.3}", (clip.end_secs() - playhead_secs).max(0.1)),
                "-volume",
                &clip.volume.to_string(),
            ])
            .arg(path)
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

pub fn start_export_timeline(
    clips: Vec<TimelineClip>,
    tracks: Vec<Track>,
    aspect_ratio: AspectRatio,
    base_height: u32,
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

        let (out_w, out_h) = aspect_ratio.dimensions(base_height.max(360));

        let mut cmd = Command::new("ffmpeg");
        apply_no_window(&mut cmd);

        // Input 0: black canvas matching project aspect ratio at 30fps
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:s={}x{}:r=30:d={:.3}", out_w, out_h, export_duration),
        ]);

        // Input 1: silent stereo audio base
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("anullsrc=r=44100:cl=stereo:d={:.3}", export_duration),
        ]);

        let mut filter_parts: Vec<String> = Vec::new();
        let mut last_video_label = "[0:v]".to_string();
        let mut audio_labels: Vec<String> = vec!["[1:a]".to_string()];
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

        let mut next_input_idx = 2usize;
        for clip in &sorted_clips {
            let flags = resolve_flags(clip);

            if clip.track == TrackKind::Text {
                if !flags.hidden {
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
                                    td.bg_padding.max(2.0)
                                )
                            } else {
                                String::new()
                            };
                            let border_part = if td.stroke_width > 0.1 {
                                let sc = td.stroke_color_hex.unwrap_or(0x000000);
                                format!(":borderw={:.1}:bordercolor=0x{:06x}", td.stroke_width, sc)
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
                                    let s = p.to_string_lossy().replace('\\', "/").replace(':', "\\:");
                                    format!(":fontfile='{}'", s)
                                })
                                .unwrap_or_default();
                            let st = clip.start_secs;
                            let en = clip.end_secs();
                            let ad = td.anim_duration_secs.clamp(0.1, (clip.duration_secs * 0.5).max(0.1));
                            let base_fs =
                                (td.font_size * (clip.scale as f32 / 100.0)).max(12.0);
                            let alpha_expr = match td.animation {
                                TextAnimation::FadeIn | TextAnimation::PopIn => {
                                    format!(":alpha='if(lt(t-{st:.3},{ad:.3}),(t-{st:.3})/{ad:.3},1)'")
                                }
                                TextAnimation::FadeOut => {
                                    format!(":alpha='if(gt(t,{en:.3}-{ad:.3}),({en:.3}-t)/{ad:.3},1)'")
                                }
                                TextAnimation::FadeInOut => {
                                    format!(":alpha='if(lt(t-{st:.3},{ad:.3}),(t-{st:.3})/{ad:.3},if(gt(t,{en:.3}-{ad:.3}),({en:.3}-t)/{ad:.3},1))'")
                                }
                                TextAnimation::None | TextAnimation::SlideUp => String::new(),
                            };
                            let y_expr = match td.animation {
                                TextAnimation::SlideUp => format!(
                                    "(h-text_h)/2+({oy:.0})+if(lt(t-{st:.3},{ad:.3}),(1-(t-{st:.3})/{ad:.3})*80,0)",
                                    oy = clip.pos_y,
                                    st = st,
                                    ad = ad
                                ),
                                _ => format!("(h-text_h)/2+({:.0})", clip.pos_y),
                            };
                            filter_parts.push(format!(
                                "{prev}drawtext=text='{txt}'{fp}:fontsize={fs:.0}:fontcolor=0x{col:06x}@{op:.2}:x=(w-text_w)/2+({ox:.0}):y='{y}'{ae}{bp}{bd}{sh}:enable='between(t,{st:.3},{en:.3})'[{vo}]",
                                prev = last_video_label,
                                txt = escaped,
                                fp = font_part,
                                fs = base_fs,
                                col = td.color_hex,
                                op = alpha,
                                ox = clip.pos_x,
                                y = y_expr,
                                ae = alpha_expr,
                                bp = box_part,
                                bd = border_part,
                                sh = shadow_part,
                                st = st,
                                en = en,
                                vo = v_out
                            ));
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
                    cmd.args([
                        "-ss",
                        &format!("{:.3}", clip.source_in_secs.max(0.0)),
                        "-t",
                        &format!("{:.3}", clip.duration_secs),
                    ])
                    .arg("-i")
                    .arg(path);

                    if !flags.hidden {
                        let v_prep = format!("vp{}", video_step);
                        let v_out = format!("vout{}", video_step);
                        video_step += 1;

                        let scale_factor = (clip.scale as f32 / 100.0).clamp(0.1, 2.0);
                        let target_w = ((out_w as f32 * scale_factor) as u32).max(16) & !1;
                        let target_h = ((out_h as f32 * scale_factor) as u32).max(16) & !1;
                        let opacity = (clip.opacity as f32 / 100.0).clamp(0.0, 1.0);

                        filter_parts.push(format!(
                            "[{in_idx}:v]scale={tw}:{th}:force_original_aspect_ratio=decrease,pad={tw}:{th}:(ow-iw)/2:(oh-ih)/2:black,format=yuva420p,colorchannelmixer=aa={op:.2},setpts=PTS-STARTPTS+{st:.3}/TB[{vp}]",
                            in_idx = input_idx,
                            tw = target_w,
                            th = target_h,
                            op = opacity,
                            st = clip.start_secs,
                            vp = v_prep
                        ));
                        filter_parts.push(format!(
                            "{prev}[{vp}]overlay=(W-w)/2:(H-h)/2:enable='between(t,{st:.3},{en:.3})'[{vo}]",
                            prev = last_video_label,
                            vp = v_prep,
                            st = clip.start_secs,
                            en = clip.end_secs(),
                            vo = v_out
                        ));
                        last_video_label = format!("[{}]", v_out);
                    }

                    if !flags.muted && clip.volume > 0 && has_audio_stream(path) {
                        let a_label = format!("a{}", audio_step);
                        audio_step += 1;
                        let delay_ms = (clip.start_secs * 1000.0).round() as u64;
                        let vol = clip.volume as f32 / 100.0;
                        filter_parts.push(format!(
                            "[{in_idx}:a]atrim=0:{dur:.3},asetpts=PTS-STARTPTS,volume={vol:.2},adelay={d}|{d}[{al}]",
                            in_idx = input_idx,
                            dur = clip.duration_secs,
                            vol = vol,
                            d = delay_ms,
                            al = a_label
                        ));
                        audio_labels.push(format!("[{}]", a_label));
                    }
                }
                TrackKind::Overlay => {
                    cmd.args(["-loop", "1", "-t", &format!("{:.3}", clip.duration_secs)])
                        .arg("-i")
                        .arg(path);

                    let v_prep = format!("vp{}", video_step);
                    let v_out = format!("vout{}", video_step);
                    video_step += 1;

                    let scale_factor = (clip.scale as f32 / 100.0).clamp(0.1, 2.0);
                    let target_w = ((out_w as f32 * scale_factor) as u32).max(16) & !1;
                    let target_h = ((out_h as f32 * scale_factor) as u32).max(16) & !1;
                    let opacity = (clip.opacity as f32 / 100.0).clamp(0.0, 1.0);

                    filter_parts.push(format!(
                        "[{in_idx}:v]scale={tw}:{th}:force_original_aspect_ratio=decrease,format=rgba,colorchannelmixer=aa={op:.2},setpts=PTS-STARTPTS+{st:.3}/TB[{vp}]",
                        in_idx = input_idx,
                        tw = target_w,
                        th = target_h,
                        op = opacity,
                        st = clip.start_secs,
                        vp = v_prep
                    ));
                    filter_parts.push(format!(
                        "{prev}[{vp}]overlay=(W-w)/2:(H-h)/2:enable='between(t,{st:.3},{en:.3})'[{vo}]",
                        prev = last_video_label,
                        vp = v_prep,
                        st = clip.start_secs,
                        en = clip.end_secs(),
                        vo = v_out
                    ));
                    last_video_label = format!("[{}]", v_out);
                }
                TrackKind::Audio => {
                    cmd.args([
                        "-ss",
                        &format!("{:.3}", clip.source_in_secs.max(0.0)),
                        "-t",
                        &format!("{:.3}", clip.duration_secs),
                    ])
                    .arg("-i")
                    .arg(path);

                    if !flags.muted && clip.volume > 0 {
                        let a_label = format!("a{}", audio_step);
                        audio_step += 1;
                        let delay_ms = (clip.start_secs * 1000.0).round() as u64;
                        let vol = clip.volume as f32 / 100.0;
                        filter_parts.push(format!(
                            "[{in_idx}:a]atrim=0:{dur:.3},asetpts=PTS-STARTPTS,volume={vol:.2},adelay={d}|{d}[{al}]",
                            in_idx = input_idx,
                            dur = clip.duration_secs,
                            vol = vol,
                            d = delay_ms,
                            al = a_label
                        ));
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
            "[1:a]".to_string()
        };

        // Ensure pixel format yuv420p at end of video chain
        filter_parts.push(format!("{}format=yuv420p[vfinal]", last_video_label));

        let filter_complex = filter_parts.join(";");

        cmd.args(["-filter_complex", &filter_complex])
            .args(["-map", "[vfinal]", "-map", &final_audio_label])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                "22",
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
