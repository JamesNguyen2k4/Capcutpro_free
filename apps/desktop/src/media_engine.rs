use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use opencut_core::{
    AspectRatio, ClipCrop, ClipKeyframe, TextAnimation, TextClipData, TimelineClip, Track,
    TrackFlags, TrackKind, TransitionKind, VideoEffect,
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

pub fn bgra_bytes_to_render_image(bytes: Vec<u8>, width: u32, height: u32) -> Option<Arc<RenderImage>> {
    let img = RgbaImage::from_raw(width, height, bytes)?;
    let frame = Frame::new(img);
    Some(Arc::new(RenderImage::new(smallvec![frame])))
}

pub fn load_overlay_bgra(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let dyn_img = image::open(path).ok()?;
    let w = dyn_img.width();
    let h = dyn_img.height();
    let max_dim = 480_u32;
    let rgba = if w > max_dim || h > max_dim {
        dyn_img
            .resize(max_dim, max_dim, image::imageops::FilterType::Triangle)
            .to_rgba8()
    } else {
        dyn_img.to_rgba8()
    };
    let rw = rgba.width();
    let rh = rgba.height();
    let mut bgra = rgba.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some((bgra, rw, rh))
}

pub fn rotate_bgra_bytes(
    src: &[u8],
    w: u32,
    h: u32,
    flip_h: bool,
    rot_deg: f32,
) -> (Vec<u8>, u32, u32) {
    let norm_rot = rot_deg.rem_euclid(360.0);
    if !flip_h && (norm_rot <= 0.5 || norm_rot >= 359.5) {
        return (src.to_vec(), w, h);
    }
    if w == 0 || h == 0 || src.len() < (w * h * 4) as usize {
        return (src.to_vec(), w, h);
    }

    let sw = w as usize;
    let sh = h as usize;

    // Fast exact path for 90/180/270 degrees
    if (norm_rot - 90.0).abs() <= 0.5 {
        let mut dst = vec![0u8; sw * sh * 4];
        for y in 0..sh {
            for x in 0..sw {
                let sx = if flip_h { sw - 1 - x } else { x };
                let dx = sh - 1 - y;
                let dy = x;
                let si = (y * sw + sx) * 4;
                let di = (dy * sh + dx) * 4;
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
        return (dst, h, w);
    }
    if (norm_rot - 180.0).abs() <= 0.5 {
        let mut dst = vec![0u8; sw * sh * 4];
        for y in 0..sh {
            for x in 0..sw {
                let sx = if flip_h { sw - 1 - x } else { x };
                let dx = sw - 1 - x;
                let dy = sh - 1 - y;
                let si = (y * sw + sx) * 4;
                let di = (dy * sw + dx) * 4;
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
        return (dst, w, h);
    }
    if (norm_rot - 270.0).abs() <= 0.5 {
        let mut dst = vec![0u8; sw * sh * 4];
        for y in 0..sh {
            for x in 0..sw {
                let sx = if flip_h { sw - 1 - x } else { x };
                let dx = y;
                let dy = sw - 1 - x;
                let si = (y * sw + sx) * 4;
                let di = (dy * sh + dx) * 4;
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
        return (dst, h, w);
    }
    if norm_rot <= 0.5 || norm_rot >= 359.5 {
        // flip_h only
        let mut dst = vec![0u8; sw * sh * 4];
        for y in 0..sh {
            for x in 0..sw {
                let sx = sw - 1 - x;
                let si = (y * sw + sx) * 4;
                let di = (y * sw + x) * 4;
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
        return (dst, w, h);
    }

    let rad = norm_rot.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();
    let dw = ((w as f32 * cos_a.abs() + h as f32 * sin_a.abs()).ceil() as usize).max(2);
    let dh = ((w as f32 * sin_a.abs() + h as f32 * cos_a.abs()).ceil() as usize).max(2);
    let mut dst = vec![0u8; dw * dh * 4];

    let scx = (sw as f32 - 1.0) * 0.5;
    let scy = (sh as f32 - 1.0) * 0.5;
    let dcx = (dw as f32 - 1.0) * 0.5;
    let dcy = (dh as f32 - 1.0) * 0.5;

    for dy in 0..dh {
        let ry = dy as f32 - dcy;
        let row_off = dy * dw * 4;
        for dx in 0..dw {
            let rx = dx as f32 - dcx;
            let mut sx = rx * cos_a + ry * sin_a + scx;
            let sy = -rx * sin_a + ry * cos_a + scy;
            if flip_h {
                sx = (sw as f32 - 1.0) - sx;
            }
            if sx >= 0.0 && sx < (sw - 1) as f32 && sy >= 0.0 && sy < (sh - 1) as f32 {
                let x0 = sx as usize;
                let y0 = sy as usize;
                let fx = sx - x0 as f32;
                let fy = sy - y0 as f32;
                let w00 = (1.0 - fx) * (1.0 - fy);
                let w10 = fx * (1.0 - fy);
                let w01 = (1.0 - fx) * fy;
                let w11 = fx * fy;

                let i00 = (y0 * sw + x0) * 4;
                let i10 = i00 + 4;
                let i01 = i00 + sw * 4;
                let i11 = i01 + 4;
                let di = row_off + dx * 4;
                for c in 0..4 {
                    let v = src[i00 + c] as f32 * w00
                        + src[i10 + c] as f32 * w10
                        + src[i01 + c] as f32 * w01
                        + src[i11 + c] as f32 * w11;
                    dst[di + c] = v as u8;
                }
            }
        }
    }
    (dst, dw as u32, dh as u32)
}

/// Fast 16.16 fixed-point nearest-neighbor rotation for smooth interactive preview dragging.
pub fn rotate_bgra_bytes_fast(
    src: &[u8],
    w: u32,
    h: u32,
    flip_h: bool,
    rot_deg: f32,
) -> (Vec<u8>, u32, u32) {
    const FAST_MAX_DIM: u32 = 540;
    if w.max(h) > FAST_MAX_DIM && w > 0 && h > 0 && src.len() >= (w * h * 4) as usize {
        // Decimate the source (nearest) so interactive drags touch ~10x fewer pixels.
        let step_num = w.max(h) as f32 / FAST_MAX_DIM as f32;
        let nw = ((w as f32 / step_num) as u32).max(1);
        let nh = ((h as f32 / step_num) as u32).max(1);
        let mut small = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            let sy = ((y as u64 * h as u64) / nh as u64) as usize;
            let src_row = sy * w as usize * 4;
            let dst_row = y as usize * nw as usize * 4;
            for x in 0..nw {
                let sx = ((x as u64 * w as u64) / nw as u64) as usize;
                let s = src_row + sx * 4;
                let d = dst_row + x as usize * 4;
                small[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
        return rotate_bgra_bytes_fast(&small, nw, nh, flip_h, rot_deg);
    }
    let norm_rot = rot_deg.rem_euclid(360.0);
    if (!flip_h && (norm_rot <= 0.5 || norm_rot >= 359.5))
        || (norm_rot - 90.0).abs() <= 0.5
        || (norm_rot - 180.0).abs() <= 0.5
        || (norm_rot - 270.0).abs() <= 0.5
        || norm_rot <= 0.5
        || norm_rot >= 359.5
        || w == 0
        || h == 0
        || src.len() < (w * h * 4) as usize
    {
        return rotate_bgra_bytes(src, w, h, flip_h, rot_deg);
    }

    let sw = w as usize;
    let sh = h as usize;
    let rad = norm_rot.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();
    let dw = ((w as f32 * cos_a.abs() + h as f32 * sin_a.abs()).ceil() as usize).max(2);
    let dh = ((w as f32 * sin_a.abs() + h as f32 * cos_a.abs()).ceil() as usize).max(2);
    let mut dst = vec![0u8; dw * dh * 4];

    let scx = (sw as f32 - 1.0) * 0.5;
    let scy = (sh as f32 - 1.0) * 0.5;
    let dcx = (dw as f32 - 1.0) * 0.5;
    let dcy = (dh as f32 - 1.0) * 0.5;

    let dxx = if flip_h { -cos_a } else { cos_a };
    let dxy = if flip_h { -sin_a } else { sin_a };
    let dxx_fp = (dxx * 65536.0).round() as i32;
    let dyx_fp = (-sin_a * 65536.0).round() as i32;

    for dy in 0..dh {
        let ry = dy as f32 - dcy;
        let start_sx = if flip_h {
            (sw as f32 - 1.0) - ((-dcx) * cos_a + ry * sin_a + scx)
        } else {
            (-dcx) * cos_a + ry * sin_a + scx
        };
        let start_sy = -(-dcx) * sin_a + ry * cos_a + scy;
        let mut sx_fp = (start_sx * 65536.0 + 32768.0) as i32;
        let mut sy_fp = (start_sy * 65536.0 + 32768.0) as i32;
        let row = &mut dst[dy * dw * 4..(dy + 1) * dw * 4];
        for dst_px in row.chunks_exact_mut(4) {
            let ix = (sx_fp >> 16) as usize;
            let iy = (sy_fp >> 16) as usize;
            if ix < sw && iy < sh {
                let si = (iy * sw + ix) * 4;
                dst_px.copy_from_slice(&src[si..si + 4]);
            }
            sx_fp = sx_fp.wrapping_add(dxx_fp);
            sy_fp = sy_fp.wrapping_add(dyx_fp);
        }
        let _ = dxy;
    }
    (dst, dw as u32, dh as u32)
}

pub fn apply_color_bgra(
    bytes: &mut [u8],
    brightness: f32,
    contrast: f32,
    saturation: f32,
    temperature: f32,
) {
    if brightness.abs() < 0.005
        && contrast.abs() < 0.005
        && saturation.abs() < 0.005
        && temperature.abs() < 0.005
    {
        return;
    }
    let bright_off = brightness * 128.0;
    let c_fac = 1.0 + contrast.clamp(-0.9, 1.5);
    let sat_fac = (1.0 + saturation).max(0.0);
    let temp_r = temperature * 36.0;
    let temp_b = -temperature * 36.0;

    for px in bytes.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        let mut b = px[0] as f32 + bright_off + temp_b;
        let mut g = px[1] as f32 + bright_off;
        let mut r = px[2] as f32 + bright_off + temp_r;

        if contrast.abs() >= 0.005 {
            b = (b - 128.0) * c_fac + 128.0;
            g = (g - 128.0) * c_fac + 128.0;
            r = (r - 128.0) * c_fac + 128.0;
        }
        if saturation.abs() >= 0.005 {
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            r = luma + (r - luma) * sat_fac;
            g = luma + (g - luma) * sat_fac;
            b = luma + (b - luma) * sat_fac;
        }
        px[0] = b.clamp(0.0, 255.0) as u8;
        px[1] = g.clamp(0.0, 255.0) as u8;
        px[2] = r.clamp(0.0, 255.0) as u8;
    }
}

fn append_color_filter(
    parts: &mut Vec<String>,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    temperature: f32,
) {
    if brightness.abs() >= 0.005 || contrast.abs() >= 0.005 || saturation.abs() >= 0.005 {
        let eq_b = (brightness * 0.5).clamp(-0.5, 0.5);
        let eq_c = (1.0 + contrast).clamp(0.2, 2.5);
        let eq_s = (1.0 + saturation).clamp(0.0, 2.5);
        parts.push(format!(
            "eq=brightness={eq_b:.3}:contrast={eq_c:.3}:saturation={eq_s:.3}"
        ));
    }
    if temperature.abs() >= 0.005 {
        let rs = (temperature * 0.25).clamp(-0.4, 0.4);
        let bs = (-temperature * 0.25).clamp(-0.4, 0.4);
        parts.push(format!("colorbalance=rs={rs:.3}:bs={bs:.3}"));
    }
}

pub fn apply_effect_bgra(
    bytes: &mut [u8],
    w: u32,
    h: u32,
    effect: VideoEffect,
    intensity: f32,
) {
    apply_effect_bgra_at(bytes, w, h, effect, 0.0, intensity, 1.0, 1.0, 0.5, 0xfacc15);
}

pub fn apply_effect_bgra_at(
    bytes: &mut [u8],
    w: u32,
    h: u32,
    effect: VideoEffect,
    time_secs: f32,
    intensity: f32,
    speed: f32,
    size: f32,
    distance: f32,
    color_hex: u32,
) {
    let amt = intensity.clamp(0.0, 1.0);
    if effect == VideoEffect::None || amt < 0.01 || w < 4 || h < 4 {
        return;
    }
    let sw = w as usize;
    let sh = h as usize;
    if bytes.len() < sw * sh * 4 {
        return;
    }
    let spd = speed.clamp(0.1, 4.0);
    let sz = size.clamp(0.1, 2.5);
    let dist = distance.clamp(0.0, 1.5);
    let er = ((color_hex >> 16) & 0xff) as f32;
    let eg = ((color_hex >> 8) & 0xff) as f32;
    let eb = (color_hex & 0xff) as f32;
    let t = time_secs * spd;

    match effect {
        VideoEffect::None => {}
        VideoEffect::Sepia | VideoEffect::OldCam => {
            let t_tick = (t * 24.0) as u32;
            for (idx, px) in bytes.chunks_exact_mut(4).enumerate() {
                if px[3] == 0 {
                    continue;
                }
                let b = px[0] as f32;
                let g = px[1] as f32;
                let r = px[2] as f32;
                let sr = (0.393 * r + 0.769 * g + 0.189 * b).min(255.0);
                let sg = (0.349 * r + 0.686 * g + 0.168 * b).min(255.0);
                let sb = (0.272 * r + 0.534 * g + 0.131 * b).min(255.0);
                let mut nr = r + (sr - r) * amt;
                let mut ng = g + (sg - g) * amt;
                let mut nb = b + (sb - b) * amt;
                if effect == VideoEffect::OldCam {
                    let y = idx / sw;
                    let n = ((idx as u32).wrapping_mul(747796405).wrapping_add(t_tick * 2891336453) >> 24) as f32 - 128.0;
                    let grain = n * amt * 0.25;
                    let scan = if y % 3 == 0 { 0.88 } else { 1.0 };
                    nr = (nr * scan + grain).clamp(0.0, 255.0);
                    ng = (ng * scan + grain).clamp(0.0, 255.0);
                    nb = (nb * scan + grain).clamp(0.0, 255.0);
                }
                px[2] = nr.clamp(0.0, 255.0) as u8;
                px[1] = ng.clamp(0.0, 255.0) as u8;
                px[0] = nb.clamp(0.0, 255.0) as u8;
            }
        }
        VideoEffect::Grain => {
            let t_tick = (t * 30.0) as u32;
            let amp = amt * 48.0;
            for (idx, px) in bytes.chunks_exact_mut(4).enumerate() {
                if px[3] == 0 {
                    continue;
                }
                let hash = ((idx as u32).wrapping_mul(1664525).wrapping_add(1013904223 ^ t_tick.wrapping_mul(2654435761)) >> 24) as f32 / 255.0 - 0.5;
                let d = hash * amp;
                px[0] = (px[0] as f32 + d).clamp(0.0, 255.0) as u8;
                px[1] = (px[1] as f32 + d).clamp(0.0, 255.0) as u8;
                px[2] = (px[2] as f32 + d).clamp(0.0, 255.0) as u8;
            }
        }
        VideoEffect::Vignette => {
            let cx = sw as f32 * 0.5;
            let cy = sh as f32 * 0.5;
            let inv_rx2 = 1.0 / ((cx * cx) * sz.max(0.25)).max(1.0);
            let inv_ry2 = 1.0 / ((cy * cy) * sz.max(0.25)).max(1.0);
            let strength = amt * 0.9;
            for y in 0..sh {
                let dy = y as f32 - cy;
                let dy2 = dy * dy * inv_ry2;
                let row = y * sw * 4;
                for x in 0..sw {
                    let dx = x as f32 - cx;
                    let d2 = ((dx * dx * inv_rx2 + dy2) * 0.5).min(1.5);
                    let fac = (1.0 - d2 * strength).clamp(0.1, 1.0);
                    let i = row + x * 4;
                    bytes[i] = (bytes[i] as f32 * fac) as u8;
                    bytes[i + 1] = (bytes[i + 1] as f32 * fac) as u8;
                    bytes[i + 2] = (bytes[i + 2] as f32 * fac) as u8;
                }
            }
        }
        VideoEffect::Blur => {
            let r = ((amt * sz * 6.0).round() as usize).clamp(1, 10);
            let mut tmp = bytes.to_vec();
            for y in 0..sh {
                let row = y * sw * 4;
                for x in 0..sw {
                    let x0 = x.saturating_sub(r);
                    let x1 = (x + r).min(sw - 1);
                    let cnt = (x1 - x0 + 1) as f32;
                    let (mut sb, mut sg, mut sr) = (0u32, 0u32, 0u32);
                    for xx in x0..=x1 {
                        let idx = row + xx * 4;
                        sb += bytes[idx] as u32;
                        sg += bytes[idx + 1] as u32;
                        sr += bytes[idx + 2] as u32;
                    }
                    let di = row + x * 4;
                    tmp[di] = (sb as f32 / cnt) as u8;
                    tmp[di + 1] = (sg as f32 / cnt) as u8;
                    tmp[di + 2] = (sr as f32 / cnt) as u8;
                }
            }
            for y in 0..sh {
                let y0 = y.saturating_sub(r);
                let y1 = (y + r).min(sh - 1);
                let cnt = (y1 - y0 + 1) as f32;
                let row = y * sw * 4;
                for x in 0..sw {
                    let (mut sb, mut sg, mut sr) = (0u32, 0u32, 0u32);
                    for yy in y0..=y1 {
                        let idx = (yy * sw + x) * 4;
                        sb += tmp[idx] as u32;
                        sg += tmp[idx + 1] as u32;
                        sr += tmp[idx + 2] as u32;
                    }
                    let di = row + x * 4;
                    bytes[di] = (sb as f32 / cnt) as u8;
                    bytes[di + 1] = (sg as f32 / cnt) as u8;
                    bytes[di + 2] = (sr as f32 / cnt) as u8;
                }
            }
        }
        VideoEffect::Sharpen => {
            let src = bytes.to_vec();
            let k = amt * 1.6;
            for y in 1..(sh - 1) {
                for x in 1..(sw - 1) {
                    let i = (y * sw + x) * 4;
                    for c in 0..3 {
                        let center = src[i + c] as f32;
                        let avg = (src[i - 4 + c] as f32
                            + src[i + 4 + c] as f32
                            + src[i - sw * 4 + c] as f32
                            + src[i + sw * 4 + c] as f32)
                            * 0.25;
                        bytes[i + c] = (center + (center - avg) * k).clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        VideoEffect::Magnify | VideoEffect::PulseZoom => {
            let src = bytes.to_vec();
            let zoom = if effect == VideoEffect::PulseZoom {
                let pulse = (t * 6.0).sin().abs();
                1.0 + amt * sz * 0.35 * pulse
            } else {
                1.0 + amt * sz * 0.45
            };
            let cx = sw as f32 * 0.5;
            let cy = sh as f32 * 0.5;
            let inv_z = 1.0 / zoom.max(1.0);
            for y in 0..sh {
                let sy = ((y as f32 - cy) * inv_z + cy).clamp(0.0, (sh - 1) as f32) as usize;
                let row = y * sw * 4;
                let srow = sy * sw * 4;
                for x in 0..sw {
                    let sx = ((x as f32 - cx) * inv_z + cx).clamp(0.0, (sw - 1) as f32) as usize;
                    let di = row + x * 4;
                    let si = srow + sx * 4;
                    bytes[di..di + 4].copy_from_slice(&src[si..si + 4]);
                }
            }
        }
        VideoEffect::RgbSplit => {
            let src = bytes.to_vec();
            let shift = ((amt * dist * 24.0).round() as isize).max(1);
            for y in 0..sh {
                let row = y * sw * 4;
                for x in 0..sw {
                    let xr = (x as isize + shift).clamp(0, sw as isize - 1) as usize;
                    let xb = (x as isize - shift).clamp(0, sw as isize - 1) as usize;
                    bytes[row + x * 4 + 2] = src[row + xr * 4 + 2];
                    bytes[row + x * 4] = src[row + xb * 4];
                }
            }
        }
        VideoEffect::Vhs | VideoEffect::Glitch => {
            let src = bytes.to_vec();
            let tick = (t * 15.0) as u32;
            let max_shift = (amt * dist * 28.0).max(2.0);
            for y in 0..sh {
                let row = y * sw * 4;
                let band = (y / 8) as u32;
                let hval = band.wrapping_mul(1103515245).wrapping_add(tick * 12345);
                let jitter = if (hval % 7) == 0 {
                    (((hval >> 8) % 21) as f32 - 10.0) * (max_shift / 10.0)
                } else if effect == VideoEffect::Vhs {
                    ((y as f32 * 0.12 + t * 8.0).sin()) * amt * 3.0
                } else {
                    0.0
                } as isize;
                let scan = if effect == VideoEffect::Vhs && y % 2 == 0 {
                    0.85
                } else {
                    1.0
                };
                for x in 0..sw {
                    let sx = (x as isize + jitter).clamp(0, sw as isize - 1) as usize;
                    let xr = (sx as isize + 3).clamp(0, sw as isize - 1) as usize;
                    let di = row + x * 4;
                    let si = row + sx * 4;
                    bytes[di] = (src[si] as f32 * scan) as u8;
                    bytes[di + 1] = (src[si + 1] as f32 * scan) as u8;
                    bytes[di + 2] = (src[row + xr * 4 + 2] as f32 * scan) as u8;
                }
            }
        }
        VideoEffect::Shake | VideoEffect::Swing => {
            let src = bytes.to_vec();
            let (ox, oy) = if effect == VideoEffect::Shake {
                (
                    ((t * 28.0).sin() * amt * dist * 22.0) as isize,
                    ((t * 35.0).cos() * amt * dist * 16.0) as isize,
                )
            } else {
                (((t * 4.0).sin() * amt * dist * 32.0) as isize, 0)
            };
            for y in 0..sh {
                let sy = (y as isize + oy).clamp(0, sh as isize - 1) as usize;
                let row = y * sw * 4;
                let srow = sy * sw * 4;
                for x in 0..sw {
                    let sx = (x as isize + ox).clamp(0, sw as isize - 1) as usize;
                    let di = row + x * 4;
                    let si = srow + sx * 4;
                    bytes[di..di + 4].copy_from_slice(&src[si..si + 4]);
                }
            }
        }
        VideoEffect::LensFlare | VideoEffect::LightLeak | VideoEffect::Rays => {
            let lx = sw as f32 * (0.2 + dist * 0.6).clamp(0.1, 0.9);
            let ly = sh as f32 * 0.25;
            let rad = (sw.min(sh) as f32 * 0.45 * sz).max(20.0);
            let inv_r2 = 1.0 / (rad * rad);
            for y in 0..sh {
                let dy = y as f32 - ly;
                let row = y * sw * 4;
                for x in 0..sw {
                    let dx = x as f32 - lx;
                    let d2 = (dx * dx + dy * dy) * inv_r2;
                    let mut glow = (1.0 - d2).max(0.0).powi(2) * amt * 0.75;
                    if effect == VideoEffect::Rays {
                        let ang = dy.atan2(dx);
                        glow *= 0.5 + 0.5 * (ang * 8.0 + t).sin().abs();
                    } else if effect == VideoEffect::LightLeak {
                        let edge = ((sw as f32 - x as f32) / sw as f32).powi(2);
                        glow = (glow + edge * amt * 0.45).min(0.85);
                    }
                    if glow > 0.01 {
                        let i = row + x * 4;
                        bytes[i] = (bytes[i] as f32 + eb * glow).min(255.0) as u8;
                        bytes[i + 1] = (bytes[i + 1] as f32 + eg * glow).min(255.0) as u8;
                        bytes[i + 2] = (bytes[i + 2] as f32 + er * glow).min(255.0) as u8;
                    }
                }
            }
        }
        VideoEffect::Flash => {
            let pulse = ((t * 8.0).sin() * 0.5 + 0.5).powi(3) * amt * 0.85;
            if pulse > 0.02 {
                for px in bytes.chunks_exact_mut(4) {
                    if px[3] == 0 {
                        continue;
                    }
                    px[0] = (px[0] as f32 + eb * pulse).min(255.0) as u8;
                    px[1] = (px[1] as f32 + eg * pulse).min(255.0) as u8;
                    px[2] = (px[2] as f32 + er * pulse).min(255.0) as u8;
                }
            }
        }
        VideoEffect::Rain | VideoEffect::Snow | VideoEffect::Dust | VideoEffect::Sparkle | VideoEffect::Bokeh => {
            let count = (28.0 + amt * 60.0) as usize;
            let p_rad = (2.0 * sz).max(1.0) as isize;
            for idx in 0..count {
                let seed = (idx as u32).wrapping_mul(2654435761);
                let x_norm = ((seed & 0xffff) as f32) / 65535.0;
                let y_norm = (((seed >> 16) & 0xffff) as f32) / 65535.0;
                let y_pos = match effect {
                    VideoEffect::Rain => ((y_norm + t * 1.6) % 1.0) * sh as f32,
                    VideoEffect::Snow => ((y_norm + t * 0.35) % 1.0) * sh as f32,
                    _ => y_norm * sh as f32,
                };
                let x_pos = match effect {
                    VideoEffect::Snow => ((x_norm + (t * 1.5 + idx as f32).sin() * 0.03).rem_euclid(1.0)) * sw as f32,
                    _ => x_norm * sw as f32,
                };
                let alpha = match effect {
                    VideoEffect::Sparkle => ((t * 6.0 + idx as f32).sin() * 0.5 + 0.5) * amt,
                    VideoEffect::Bokeh => amt * 0.35,
                    _ => amt * 0.75,
                };
                if alpha < 0.05 {
                    continue;
                }
                let cx = x_pos as isize;
                let cy = y_pos as isize;
                let (rx, ry) = match effect {
                    VideoEffect::Rain => (0isize, (6.0 * sz) as isize),
                    VideoEffect::Bokeh => ((p_rad * 4).max(4), (p_rad * 4).max(4)),
                    _ => (p_rad, p_rad),
                };
                for dy in -ry..=ry {
                    let py = cy + dy;
                    if py < 0 || py >= sh as isize {
                        continue;
                    }
                    for dx in -rx..=rx {
                        let px_x = cx + dx;
                        if px_x < 0 || px_x >= sw as isize {
                            continue;
                        }
                        if rx > 1 && ry > 1 && dx * dx + dy * dy > rx * rx {
                            continue;
                        }
                        let i = (py as usize * sw + px_x as usize) * 4;
                        let (cr, cg, cb) = if matches!(effect, VideoEffect::Rain | VideoEffect::Snow) {
                            (235.0, 245.0, 255.0)
                        } else {
                            (er, eg, eb)
                        };
                        bytes[i] = (bytes[i] as f32 * (1.0 - alpha) + cb * alpha).min(255.0) as u8;
                        bytes[i + 1] = (bytes[i + 1] as f32 * (1.0 - alpha) + cg * alpha).min(255.0) as u8;
                        bytes[i + 2] = (bytes[i + 2] as f32 * (1.0 - alpha) + cr * alpha).min(255.0) as u8;
                    }
                }
            }
        }
        VideoEffect::FilmBorder | VideoEffect::CamFrame | VideoEffect::RecHud => {
            let border_h = ((sh as f32 * 0.11 * sz * amt) as usize).clamp(4, sh / 3);
            if effect == VideoEffect::FilmBorder {
                for y in 0..sh {
                    if y < border_h || y >= sh - border_h {
                        let row = y * sw * 4;
                        for x in 0..sw {
                            let i = row + x * 4;
                            bytes[i] = 10;
                            bytes[i + 1] = 10;
                            bytes[i + 2] = 12;
                        }
                    }
                }
            } else {
                let pad = (sw.min(sh) / 16).max(6);
                let len = (sw.min(sh) / 7).max(12);
                let thick = (2.0 * sz).max(2.0) as usize;
                let draw_px = |buf: &mut [u8], x: usize, y: usize, b: u8, g: u8, r: u8| {
                    if x < sw && y < sh {
                        let i = (y * sw + x) * 4;
                        buf[i] = b;
                        buf[i + 1] = g;
                        buf[i + 2] = r;
                    }
                };
                for t_off in 0..thick {
                    for d in 0..len {
                        // 4 corner L-brackets
                        draw_px(bytes, pad + d, pad + t_off, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, pad + t_off, pad + d, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, sw - 1 - pad - d, pad + t_off, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, sw - 1 - pad - t_off, pad + d, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, pad + d, sh - 1 - pad - t_off, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, pad + t_off, sh - 1 - pad - d, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, sw - 1 - pad - d, sh - 1 - pad - t_off, eb as u8, eg as u8, er as u8);
                        draw_px(bytes, sw - 1 - pad - t_off, sh - 1 - pad - d, eb as u8, eg as u8, er as u8);
                    }
                }
                if effect == VideoEffect::RecHud && ((t * 2.0) as u32 % 2 == 0) {
                    // Blinking red REC dot
                    let r_cx = pad + 16;
                    let r_cy = pad + 16;
                    for dy in -6isize..=6 {
                        for dx in -6isize..=6 {
                            if dx * dx + dy * dy <= 36 {
                                draw_px(bytes, (r_cx as isize + dx) as usize, (r_cy as isize + dy) as usize, 40, 40, 239);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn append_effect_filter(parts: &mut Vec<String>, effect: VideoEffect, intensity: f32) {
    let amt = intensity.clamp(0.0, 1.0);
    if effect == VideoEffect::None || amt < 0.01 {
        return;
    }
    match effect {
        VideoEffect::None => {}
        VideoEffect::Blur => {
            let luma_r = (amt * 12.0).clamp(1.0, 14.0);
            parts.push(format!("boxblur={luma_r:.1}:1"));
        }
        VideoEffect::Grain | VideoEffect::OldCam => {
            let strength = (amt * 28.0).clamp(4.0, 35.0);
            parts.push(format!("noise=alls={strength:.0}:allf=t+u"));
        }
        VideoEffect::Vignette => {
            let angle = (0.25 + amt * 0.65).clamp(0.2, 1.0);
            parts.push(format!("vignette=angle=PI/{:.2}", 1.0 / angle));
        }
        VideoEffect::Sharpen => {
            let la = (0.3 + amt * 1.7).clamp(0.3, 2.0);
            parts.push(format!("unsharp=5:5:{la:.2}:5:5:0.0"));
        }
        VideoEffect::Sepia => {
            let m = amt;
            let inv = 1.0 - m;
            parts.push(format!(
                "colorchannelmixer=rr={:.3}:rg={:.3}:rb={:.3}:gr={:.3}:gg={:.3}:gb={:.3}:br={:.3}:bg={:.3}:bb={:.3}",
                inv + 0.393 * m,
                0.769 * m,
                0.189 * m,
                0.349 * m,
                inv + 0.686 * m,
                0.168 * m,
                0.272 * m,
                0.534 * m,
                inv + 0.131 * m
            ));
        }
        VideoEffect::RgbSplit | VideoEffect::Vhs | VideoEffect::Glitch => {
            let shift = (amt * 12.0).round().max(2.0);
            parts.push(format!("rgbashift=rh={shift:.0}:bh=-{shift:.0}"));
        }
        VideoEffect::Magnify | VideoEffect::PulseZoom => {
            let z = 1.0 + amt * 0.25;
            parts.push(format!("crop=iw/{z:.3}:ih/{z:.3},scale=iw*{z:.3}:ih*{z:.3}"));
        }
        VideoEffect::FilmBorder => {
            let bh = (amt * 0.12).clamp(0.02, 0.18);
            parts.push(format!(
                "drawbox=x=0:y=0:w=iw:h=ih*{bh:.3}:color=black:t=fill,drawbox=x=0:y=ih*(1-{bh:.3}):w=iw:h=ih*{bh:.3}:color=black:t=fill"
            ));
        }
        VideoEffect::LensFlare
        | VideoEffect::LightLeak
        | VideoEffect::Rays
        | VideoEffect::Flash => {
            let b = (amt * 0.18).clamp(0.02, 0.25);
            parts.push(format!("eq=brightness={b:.3}"));
        }
        VideoEffect::Shake
        | VideoEffect::Swing
        | VideoEffect::Rain
        | VideoEffect::Snow
        | VideoEffect::Dust
        | VideoEffect::Sparkle
        | VideoEffect::Bokeh
        | VideoEffect::CamFrame
        | VideoEffect::RecHud => {
            // Subtle contrast/sharpen accent on export for overlay-style effects
            let c = (1.0 + amt * 0.12).clamp(1.0, 1.2);
            parts.push(format!("eq=contrast={c:.3}"));
        }
    }
}

#[derive(Clone)]
pub struct CubeLut {
    pub size: usize,
    pub table: Vec<[f32; 3]>,
}

pub fn parse_cube_lut(path: &Path) -> Option<CubeLut> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut size: usize = 0;
    let mut table: Vec<[f32; 3]> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("LUT_3D_SIZE") {
            size = rest.trim().parse::<usize>().ok()?;
            table.reserve(size * size * size);
            continue;
        }
        if trimmed.starts_with("TITLE")
            || trimmed.starts_with("DOMAIN_MIN")
            || trimmed.starts_with("DOMAIN_MAX")
            || trimmed.starts_with("LUT_1D_SIZE")
        {
            continue;
        }
        let mut nums = trimmed.split_whitespace();
        let (Some(r), Some(g), Some(b)) = (nums.next(), nums.next(), nums.next()) else {
            continue;
        };
        if let (Ok(rv), Ok(gv), Ok(bv)) = (r.parse::<f32>(), g.parse::<f32>(), b.parse::<f32>()) {
            table.push([rv.clamp(0.0, 1.0), gv.clamp(0.0, 1.0), bv.clamp(0.0, 1.0)]);
        }
    }
    if size >= 2 && table.len() >= size * size * size {
        table.truncate(size * size * size);
        Some(CubeLut { size, table })
    } else {
        None
    }
}

pub fn apply_cube_lut_bgra(bytes: &mut [u8], lut: &CubeLut) {
    let n = lut.size;
    if n < 2 || lut.table.len() < n * n * n {
        return;
    }
    let max_idx = (n - 1) as f32;
    let n2 = n * n;
    for px in bytes.chunks_exact_mut(4) {
        if px[3] == 0 {
            continue;
        }
        let rf = (px[2] as f32 / 255.0) * max_idx;
        let gf = (px[1] as f32 / 255.0) * max_idx;
        let bf = (px[0] as f32 / 255.0) * max_idx;

        let r0 = (rf as usize).min(n - 2);
        let g0 = (gf as usize).min(n - 2);
        let b0 = (bf as usize).min(n - 2);
        let r1 = r0 + 1;
        let g1 = g0 + 1;
        let b1 = b0 + 1;
        let dr = rf - r0 as f32;
        let dg = gf - g0 as f32;
        let db = bf - b0 as f32;

        let idx = |r: usize, g: usize, b: usize| b * n2 + g * n + r;
        let c000 = lut.table[idx(r0, g0, b0)];
        let c100 = lut.table[idx(r1, g0, b0)];
        let c010 = lut.table[idx(r0, g1, b0)];
        let c110 = lut.table[idx(r1, g1, b0)];
        let c001 = lut.table[idx(r0, g0, b1)];
        let c101 = lut.table[idx(r1, g0, b1)];
        let c011 = lut.table[idx(r0, g1, b1)];
        let c111 = lut.table[idx(r1, g1, b1)];

        for ch in 0..3 {
            let c00 = c000[ch] + (c100[ch] - c000[ch]) * dr;
            let c10 = c010[ch] + (c110[ch] - c010[ch]) * dr;
            let c01 = c001[ch] + (c101[ch] - c001[ch]) * dr;
            let c11 = c011[ch] + (c111[ch] - c011[ch]) * dr;
            let c0 = c00 + (c10 - c00) * dg;
            let c1 = c01 + (c11 - c01) * dg;
            let val = c0 + (c1 - c0) * db;
            let byte = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
            match ch {
                0 => px[2] = byte,
                1 => px[1] = byte,
                _ => px[0] = byte,
            }
        }
    }
}

fn append_lut_filter(parts: &mut Vec<String>, lut_path: Option<&PathBuf>) {
    if let Some(p) = lut_path {
        let esc = p.to_string_lossy().replace('\\', "/").replace(':', "\\:");
        parts.push(format!("lut3d='{}'", esc));
    }
}

pub struct VoiceoverRecorder {
    child: Child,
    pub output_path: PathBuf,
    started_at: std::time::Instant,
}

fn find_dshow_audio_device() -> Option<String> {
    let mut cmd = Command::new("ffmpeg");
    apply_no_window(&mut cmd);
    cmd.args(["-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd.output().ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        if line.contains("(audio)") {
            if let Some(q1) = line.find('"') {
                if let Some(q2) = line[q1 + 1..].find('"') {
                    return Some(line[q1 + 1..q1 + 1 + q2].to_string());
                }
            }
        }
    }
    None
}

impl VoiceoverRecorder {
    pub fn start() -> Result<Self, String> {
        let device = find_dshow_audio_device()
            .ok_or_else(|| "No microphone (DirectShow audio device) found".to_string())?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let out_dir = std::env::temp_dir().join("opencut_voiceover");
        let _ = std::fs::create_dir_all(&out_dir);
        let output_path = out_dir.join(format!("voiceover_{}.wav", ts));

        let mut cmd = Command::new("ffmpeg");
        apply_no_window(&mut cmd);
        cmd.args([
            "-y",
            "-f",
            "dshow",
            "-i",
            &format!("audio={}", device),
            "-acodec",
            "pcm_s16le",
            "-ar",
            "44100",
            "-ac",
            "2",
        ])
        .arg(&output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to start microphone recorder: {}", e))?;
        Ok(Self {
            child,
            output_path,
            started_at: std::time::Instant::now(),
        })
    }

    pub fn elapsed_secs(&self) -> f32 {
        self.started_at.elapsed().as_secs_f32()
    }

    pub fn stop(mut self) -> Option<(PathBuf, f32)> {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = stdin.write_all(b"q\n");
            let _ = stdin.flush();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let dur = probe_duration_secs(&self.output_path)
            .unwrap_or_else(|| self.started_at.elapsed().as_secs_f32().max(0.5));
        if self.output_path.exists() {
            Some((self.output_path, dur))
        } else {
            None
        }
    }
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

pub fn decode_single_frame_raw(
    path: &Path,
    time_secs: f32,
    aspect_ratio: AspectRatio,
    preview_height: u32,
    crop: ClipCrop,
) -> Option<(Vec<u8>, u32, u32)> {
    let (pw, ph) = preview_dimensions(aspect_ratio, preview_height);
    let frame_bytes = (pw * ph * 4) as usize;
    let mut vf_parts = Vec::new();
    append_crop_filter(&mut vf_parts, crop);
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
    Some((buf, pw, ph))
}

pub fn render_unrotated_text_bgra(
    td: &TextClipData,
    font_size_px: f32,
    box_w: u32,
    box_h: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let base_w = (box_w + 16).max(32);
    let base_h = (box_h + 16).max(32);
    let frame_bytes = (base_w * base_h * 4) as usize;

    let formatted = td.formatted_content();
    let display_text = if formatted.is_empty() {
        "Text"
    } else {
        formatted.as_str()
    };
    let escaped = display_text
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'");
    let box_part = if let Some(bg_hex) = td.bg_color_hex {
        format!(
            ":box=1:boxcolor=0x{:06x}@{:.2}:boxborderw={:.0}",
            bg_hex,
            td.bg_opacity.clamp(0.05, 1.0),
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

    let xf = format!(
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
    Some((buf, base_w, base_h))
}

pub struct VideoStreamPlayer {
    stop_flag: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    latest_frame: Arc<Mutex<Option<Arc<RenderImage>>>>,
    latest_raw_frame: Arc<Mutex<Option<(Vec<u8>, u32, u32)>>>,
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
            latest_raw_frame: Arc::new(Mutex::new(None)),
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
        let latest_raw_frame = Arc::new(Mutex::new(None));

        self.stop_flag = stop_flag.clone();
        self.child = child_slot.clone();
        self.latest_frame = latest_frame.clone();
        self.latest_raw_frame = latest_raw_frame.clone();
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
                if let Ok(mut guard) = latest_raw_frame.lock() {
                    *guard = Some((buf.clone(), pw, ph));
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

    pub fn take_latest_raw_frame(&self) -> Option<(Vec<u8>, u32, u32)> {
        self.latest_raw_frame.lock().ok()?.take()
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
    if clip.audio_normalized {
        af_parts.push("loudnorm=I=-16:TP=-1.5:LRA=11".to_string());
    }
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
    let (cb, cc, cs, ct) = clip.effective_color();
    append_color_filter(&mut vf_parts, cb, cc, cs, ct);
    append_effect_filter(&mut vf_parts, clip.effect, clip.effect_intensity);
    append_lut_filter(&mut vf_parts, clip.lut_path.as_ref());
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
        } else if is_video && matches!(clip.transition_in, TransitionKind::None | TransitionKind::SlideLeft | TransitionKind::SlideRight) {
            vf_parts.push("format=yuv420p".into());
        }
    }

    let td = clip
        .transition_dur_secs
        .clamp(0.1, (clip.duration_secs * 0.5).max(0.1));
    if matches!(
        clip.transition_in,
        TransitionKind::Dissolve | TransitionKind::ZoomIn
    ) {
        vf_parts.push(format!(
            "format=rgba,fade=t=in:st={st:.3}:d={td:.3}:alpha=1",
            st = clip.start_secs,
            td = td
        ));
    }
    if matches!(
        clip.transition_out,
        TransitionKind::Dissolve | TransitionKind::ZoomIn
    ) {
        let out_st = (clip.end_secs() - td).max(clip.start_secs);
        vf_parts.push(format!(
            "format=rgba,fade=t=out:st={out_st:.3}:d={td:.3}:alpha=1",
            out_st = out_st,
            td = td
        ));
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
    let ox_base = if (res_scale - 1.0).abs() > 0.001 {
        format!("({ox_raw})*{res_scale:.4}")
    } else {
        ox_raw
    };
    let ox_in = match clip.transition_in {
        TransitionKind::SlideLeft => format!(
            "if(lt(t-{st:.3},{td:.3}),({ox_base})+({ow:.0}*(1-(t-{st:.3})/{td:.3})),{ox_base})",
            st = clip.start_secs,
            td = td,
            ow = out_w as f32
        ),
        TransitionKind::SlideRight => format!(
            "if(lt(t-{st:.3},{td:.3}),({ox_base})-({ow:.0}*(1-(t-{st:.3})/{td:.3})),{ox_base})",
            st = clip.start_secs,
            td = td,
            ow = out_w as f32
        ),
        _ => ox_base,
    };
    let out_st = (clip.end_secs() - td).max(clip.start_secs);
    let ox_expr = match clip.transition_out {
        TransitionKind::SlideLeft => format!(
            "if(gt(t,{out_st:.3}),({ox_in})-({ow:.0}*((t-{out_st:.3})/{td:.3})),{ox_in})",
            out_st = out_st,
            td = td,
            ow = out_w as f32
        ),
        TransitionKind::SlideRight => format!(
            "if(gt(t,{out_st:.3}),({ox_in})+({ow:.0}*((t-{out_st:.3})/{td:.3})),{ox_in})",
            out_st = out_st,
            td = td,
            ow = out_w as f32
        ),
        _ => ox_in,
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
                let has_media = c.source_path.is_some()
                    || (c.track == TrackKind::Text && c.text.is_some())
                    || c.track == TrackKind::Adjustment;
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
                                .formatted_content()
                                .replace('\\', "\\\\")
                                .replace(':', "\\:")
                                .replace('\'', "\\'");
                            let alpha = (clip.opacity as f32 / 100.0).clamp(0.0, 1.0);
                            let box_part = if let Some(bg_hex) = td.bg_color_hex {
                                format!(
                                    ":box=1:boxcolor=0x{:06x}@{:.2}:boxborderw={:.0}",
                                    bg_hex,
                                    td.bg_opacity.clamp(0.05, 1.0),
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

            if clip.track == TrackKind::Adjustment {
                if !audio_only && !flags.hidden {
                    let mut adj_parts = Vec::new();
                    let (cb, cc, cs, ct) = clip.effective_color();
                    append_color_filter(&mut adj_parts, cb, cc, cs, ct);
                    append_effect_filter(&mut adj_parts, clip.effect, clip.effect_intensity);
                    append_lut_filter(&mut adj_parts, clip.lut_path.as_ref());
                    if !adj_parts.is_empty() {
                        let a_base = format!("adj_a{}", video_step);
                        let a_copy = format!("adj_b{}", video_step);
                        let a_filt = format!("adj_f{}", video_step);
                        let v_out = format!("vout{}", video_step);
                        video_step += 1;
                        let st = clip.start_secs;
                        let en = clip.end_secs();
                        filter_parts.push(format!(
                            "{prev}split[{ab}][{ac}];[{ac}]{chain}[{af}];[{ab}][{af}]overlay=enable='between(t,{st:.3},{en:.3})'[{vo}]",
                            prev = last_video_label,
                            ab = a_base,
                            ac = a_copy,
                            chain = adj_parts.join(","),
                            af = a_filt,
                            st = st,
                            en = en,
                            vo = v_out,
                        ));
                        last_video_label = format!("[{}]", v_out);
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

                    if !clip.audio_detached && !flags.muted && clip.volume > 0 && has_audio_stream(path) {
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

pub type ReverseCacheKey = (PathBuf, u64, u64);

pub fn make_reverse_cache_key(path: &Path, src_in_secs: f32, src_dur_secs: f32) -> ReverseCacheKey {
    let in_ms = (src_in_secs.max(0.0) * 1000.0).round() as u64;
    let dur_ms = (src_dur_secs.max(0.1) * 1000.0).round() as u64;
    (path.to_path_buf(), in_ms, dur_ms)
}

#[derive(Clone)]
pub struct ReverseHandle {
    pub clip_id: usize,
    pub cache_key: ReverseCacheKey,
    pub output_path: PathBuf,
    pub progress_pct: Arc<Mutex<f32>>,
    pub status_label: Arc<Mutex<String>>,
    pub finished: Arc<AtomicBool>,
    pub cancelled: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
}

fn run_ffmpeg_reverse_chunk(
    source_path: &Path,
    out_path: &Path,
    start_secs: f32,
    dur_secs: f32,
    has_audio: bool,
    progress_pct: &Arc<Mutex<f32>>,
    base_pct: f32,
    weight_pct: f32,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), String> {
    let mut cmd = Command::new("ffmpeg");
    apply_no_window(&mut cmd);
    cmd.args([
        "-y",
        "-v",
        "error",
        "-ss",
        &format!("{:.3}", start_secs.max(0.0)),
        "-t",
        &format!("{:.3}", dur_secs.max(0.05)),
        "-i",
    ])
    .arg(source_path)
    .args([
        "-vf",
        "reverse",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-crf",
        "17",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "15",
    ]);

    if has_audio {
        cmd.args(["-af", "areverse", "-c:a", "aac", "-b:a", "192k"]);
    } else {
        cmd.arg("-an");
    }

    cmd.args(["-progress", "pipe:1", "-nostats"])
        .arg(out_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut process = cmd.spawn().map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;
    let total_us = (dur_secs.max(0.05) as f64) * 1_000_000.0;

    if let Some(stdout) = process.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if cancelled.load(Ordering::Relaxed) {
                let _ = process.kill();
                let _ = process.wait();
                let _ = std::fs::remove_file(out_path);
                return Err("Cancelled".to_string());
            }
            if let Some(val) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = val.trim().parse::<f64>() {
                    let frac = (us / total_us).clamp(0.0, 1.0) as f32;
                    if let Ok(mut p) = progress_pct.lock() {
                        *p = (base_pct + weight_pct * frac).clamp(0.0, 99.5);
                    }
                }
            }
        }
    }

    if cancelled.load(Ordering::Relaxed) {
        let _ = process.kill();
        let _ = process.wait();
        let _ = std::fs::remove_file(out_path);
        return Err("Cancelled".to_string());
    }

    let output = process
        .wait_with_output()
        .map_err(|e| format!("FFmpeg wait failed: {e}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(out_path);
        let msg = String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap_or("FFmpeg reverse failed")
            .to_string();
        return Err(msg);
    }
    Ok(())
}

/// Pre-renders a reversed MP4 (video + audio) in a background thread with chunked progress.
pub fn start_reverse_video(
    clip_id: usize,
    source_path: PathBuf,
    src_in_secs: f32,
    src_dur_secs: f32,
) -> ReverseHandle {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let cache_key = make_reverse_cache_key(&source_path, src_in_secs, src_dur_secs);
    let mut h = DefaultHasher::new();
    cache_key.hash(&mut h);
    let hash_id = h.finish();
    let output_path = std::env::temp_dir().join(format!("opencut_rev_{:016x}.mp4", hash_id));

    let progress_pct = Arc::new(Mutex::new(0.0_f32));
    let status_label = Arc::new(Mutex::new("Preparing reverse...".to_string()));
    let finished = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let error = Arc::new(Mutex::new(None));

    let handle = ReverseHandle {
        clip_id,
        cache_key,
        output_path: output_path.clone(),
        progress_pct: progress_pct.clone(),
        status_label: status_label.clone(),
        finished: finished.clone(),
        cancelled: cancelled.clone(),
        error: error.clone(),
    };

    std::thread::spawn(move || {
        let has_audio = has_audio_stream(&source_path);
        let total_dur = src_dur_secs.max(0.1);
        const CHUNK_SECS: f32 = 15.0;

        if total_dur <= 18.0 {
            if let Ok(mut lbl) = status_label.lock() {
                *lbl = "Reversing video & audio...".to_string();
            }
            if let Err(err_msg) = run_ffmpeg_reverse_chunk(
                &source_path,
                &output_path,
                src_in_secs,
                total_dur,
                has_audio,
                &progress_pct,
                0.0,
                99.5,
                &cancelled,
            ) {
                if !cancelled.load(Ordering::Relaxed) {
                    if let Ok(mut e) = error.lock() {
                        *e = Some(err_msg);
                    }
                }
                finished.store(true, Ordering::Relaxed);
                return;
            }
        } else {
            let num_chunks = (total_dur / CHUNK_SECS).ceil().max(1.0) as usize;
            let mut chunk_ranges = Vec::with_capacity(num_chunks);
            for i in 0..num_chunks {
                let offset = i as f32 * CHUNK_SECS;
                let dur = (total_dur - offset).min(CHUNK_SECS);
                if dur > 0.04 {
                    chunk_ranges.push((src_in_secs + offset, dur));
                }
            }
            // Reverse order of chunks so the output plays backwards from end to start
            chunk_ranges.reverse();
            let total_c = chunk_ranges.len().max(1);
            let mut chunk_files: Vec<PathBuf> = Vec::with_capacity(total_c);

            for (idx, (c_start, c_dur)) in chunk_ranges.iter().enumerate() {
                if cancelled.load(Ordering::Relaxed) {
                    for f in &chunk_files {
                        let _ = std::fs::remove_file(f);
                    }
                    finished.store(true, Ordering::Relaxed);
                    return;
                }
                if let Ok(mut lbl) = status_label.lock() {
                    *lbl = format!("Reversing segment {}/{}...", idx + 1, total_c);
                }
                let chunk_path = std::env::temp_dir()
                    .join(format!("opencut_rev_{:016x}_c{}.mp4", hash_id, idx));
                let base_pct = (idx as f32 / total_c as f32) * 95.0;
                let weight_pct = (1.0 / total_c as f32) * 95.0;

                if let Err(err_msg) = run_ffmpeg_reverse_chunk(
                    &source_path,
                    &chunk_path,
                    *c_start,
                    *c_dur,
                    has_audio,
                    &progress_pct,
                    base_pct,
                    weight_pct,
                    &cancelled,
                ) {
                    for f in &chunk_files {
                        let _ = std::fs::remove_file(f);
                    }
                    if !cancelled.load(Ordering::Relaxed) {
                        if let Ok(mut e) = error.lock() {
                            *e = Some(err_msg);
                        }
                    }
                    finished.store(true, Ordering::Relaxed);
                    return;
                }
                chunk_files.push(chunk_path);
            }

            if let Ok(mut lbl) = status_label.lock() {
                *lbl = "Finalizing reversed video...".to_string();
            }
            let list_path =
                std::env::temp_dir().join(format!("opencut_rev_{:016x}_list.txt", hash_id));
            let mut list_content = String::new();
            for f in &chunk_files {
                let escaped = f.to_string_lossy().replace('\\', "/").replace('\'', "'\\''");
                list_content.push_str(&format!("file '{}'\n", escaped));
            }
            if let Err(e) = std::fs::write(&list_path, list_content) {
                for f in &chunk_files {
                    let _ = std::fs::remove_file(f);
                }
                if let Ok(mut err) = error.lock() {
                    *err = Some(format!("Failed to write concat list: {e}"));
                }
                finished.store(true, Ordering::Relaxed);
                return;
            }

            let mut concat_cmd = Command::new("ffmpeg");
            apply_no_window(&mut concat_cmd);
            concat_cmd
                .args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
                .arg(&list_path)
                .args(["-c", "copy"])
                .arg(&output_path);

            let concat_res = concat_cmd.output();
            let _ = std::fs::remove_file(&list_path);
            for f in &chunk_files {
                let _ = std::fs::remove_file(f);
            }

            match concat_res {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let msg = String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .last()
                        .unwrap_or("FFmpeg concat failed")
                        .to_string();
                    if let Ok(mut err) = error.lock() {
                        *err = Some(msg);
                    }
                    finished.store(true, Ordering::Relaxed);
                    return;
                }
                Err(e) => {
                    if let Ok(mut err) = error.lock() {
                        *err = Some(e.to_string());
                    }
                    finished.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }

        if let Ok(mut p) = progress_pct.lock() {
            *p = 100.0;
        }
        finished.store(true, Ordering::Relaxed);
    });

    handle
}
