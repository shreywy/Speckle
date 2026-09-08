//! Turning any of six family of files into pixels.
//!
//! Ordinary rasters go through the `image` crate. RAW and PSD carry a
//! full-quality JPEG preview inside them, which is far cheaper to lift out than
//! demosaicing a sensor dump — so we scan for it. HEIC and every video format
//! go out to ffmpeg, which is the only thing on a Windows box that reliably
//! reads both.

use anyhow::{anyhow, bail, Context, Result};
use image::{DynamicImage, GenericImageView};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::db::{KIND_LAYERED, KIND_PHOTO, KIND_RAW, KIND_VIDEO};

pub const PHOTO_EXT: &[&str] =
    &["jpg", "jpeg", "jpe", "png", "webp", "bmp", "gif", "tif", "tiff", "avif", "jfif", "ico"];
pub const HEIC_EXT: &[&str] = &["heic", "heif", "hif"];
pub const RAW_EXT: &[&str] = &[
    "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef", "srw",
    "erf", "kdc", "dcr", "mrw", "3fr", "iiq", "x3f",
];
pub const LAYERED_EXT: &[&str] = &["psd", "psb", "xcf"];
pub const VIDEO_EXT: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "3gp", "3g2", "mts", "m2ts", "mpg",
    "mpeg", "vob", "ogv", "mxf", "ts",
];

/// Which decode path an extension takes, or `None` if we do not handle it.
pub fn classify(ext: &str) -> Option<i64> {
    let e = ext.to_ascii_lowercase();
    let e = e.as_str();
    if PHOTO_EXT.contains(&e) || HEIC_EXT.contains(&e) {
        Some(KIND_PHOTO)
    } else if RAW_EXT.contains(&e) {
        Some(KIND_RAW)
    } else if LAYERED_EXT.contains(&e) {
        Some(KIND_LAYERED)
    } else if VIDEO_EXT.contains(&e) {
        Some(KIND_VIDEO)
    } else {
        None
    }
}

pub fn is_heic(ext: &str) -> bool {
    HEIC_EXT.contains(&ext.to_ascii_lowercase().as_str())
}

/// True when a browser can play the container/codec pair directly. Anything
/// else has to be transcoded on the way out — iPhone .MOV is HEVC with PCM
/// audio, which no WebView2 build will touch.
pub fn video_plays_natively(ext: &str, vcodec: Option<&str>, acodec: Option<&str>) -> bool {
    let e = ext.to_ascii_lowercase();
    if !matches!(e.as_str(), "mp4" | "m4v" | "webm" | "mov") {
        return false;
    }
    let v_ok = matches!(vcodec, Some("h264") | Some("vp8") | Some("vp9") | Some("av1"));
    let a_ok = match acodec {
        None => true,
        Some(a) => matches!(a, "aac" | "mp3" | "opus" | "vorbis"),
    };
    v_ok && a_ok
}

// ---------------------------------------------------------------- ffmpeg ----

fn ffmpeg_bin() -> &'static str {
    "ffmpeg"
}
fn ffprobe_bin() -> &'static str {
    "ffprobe"
}

/// Is ffmpeg reachable? Checked once at startup so the UI can say so plainly
/// rather than silently producing blank video tiles.
pub fn ffmpeg_available() -> bool {
    Command::new(ffmpeg_bin())
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn hidden(cmd: &mut Command) -> &mut Command {
    cmd
}

/// Ask ffmpeg for a single frame as JPEG on stdout, optionally pre-scaled so
/// the decoder does the downsizing rather than us.
fn ffmpeg_frame(path: &Path, seek: Option<f64>, max_edge: u32) -> Result<Vec<u8>> {
    let mut cmd = Command::new(ffmpeg_bin());
    hidden(&mut cmd);
    cmd.arg("-v").arg("error").arg("-nostdin");
    if let Some(s) = seek {
        // Before -i so ffmpeg seeks by keyframe index instead of decoding up to it.
        cmd.arg("-ss").arg(format!("{s:.3}"));
    }
    cmd.arg("-i")
        .arg(path)
        .arg("-map")
        .arg("v:0")
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(format!(
            "scale='if(gt(iw,ih),min({m},iw),-2)':'if(gt(iw,ih),-2,min({m},ih))':flags=bilinear",
            m = max_edge
        ))
        .arg("-f")
        .arg("mjpeg")
        .arg("-q:v")
        .arg("3")
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let out = cmd.output().context("failed to launch ffmpeg")?;
    if out.stdout.is_empty() {
        bail!("ffmpeg produced no frame: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

#[derive(Debug, Default, Clone)]
pub struct Probe {
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub dur: Option<f64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub taken: Option<i64>,
    pub rotation: i32,
}

/// ffprobe a video for the handful of fields the UI actually shows.
pub fn probe_video(path: &Path) -> Result<Probe> {
    let mut cmd = Command::new(ffprobe_bin());
    hidden(&mut cmd);
    let out = cmd
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .context("failed to launch ffprobe")?;

    let j: serde_json::Value = serde_json::from_slice(&out.stdout).context("ffprobe json")?;
    let mut p = Probe::default();

    if let Some(streams) = j["streams"].as_array() {
        for s in streams {
            match s["codec_type"].as_str() {
                Some("video") if p.vcodec.is_none() => {
                    p.vcodec = s["codec_name"].as_str().map(str::to_owned);
                    p.w = s["width"].as_u64().map(|v| v as u32);
                    p.h = s["height"].as_u64().map(|v| v as u32);
                    if let Some(sd) = s["side_data_list"].as_array() {
                        for d in sd {
                            if let Some(r) = d["rotation"].as_f64() {
                                p.rotation = r as i32;
                            }
                        }
                    }
                }
                Some("audio") if p.acodec.is_none() => {
                    p.acodec = s["codec_name"].as_str().map(str::to_owned);
                }
                _ => {}
            }
        }
    }
    p.dur = j["format"]["duration"].as_str().and_then(|s| s.parse().ok());
    // QuickTime creation_time is the closest thing a video has to "date taken".
    if let Some(t) = j["format"]["tags"]["creation_time"].as_str() {
        p.taken = parse_iso8601(t);
    }
    Ok(p)
}

fn parse_iso8601(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
}

// ------------------------------------------------------- embedded previews ---

/// RAW and PSD files carry one or more complete JPEGs inside them. Finding the
/// largest is dramatically cheaper than a real decode and is what every fast
/// photo browser does. Candidates are tried largest-first because an `FFD9`
/// byte pair can occur inside entropy-coded data and produce a short false end.
fn largest_embedded_jpeg(bytes: &[u8]) -> Option<DynamicImage> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;

    while i + 3 < bytes.len() {
        if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 && bytes[i + 2] == 0xFF {
            // Walk forward to the next SOI so we can take the last EOI before it.
            let mut j = i + 3;
            let mut last_eoi = None;
            let limit = bytes.len();
            while j + 1 < limit {
                if bytes[j] == 0xFF {
                    if bytes[j + 1] == 0xD9 {
                        last_eoi = Some(j + 2);
                    } else if bytes[j + 1] == 0xD8 && j > i + 3 {
                        break;
                    }
                }
                j += 1;
            }
            if let Some(e) = last_eoi {
                if e > i + 1024 {
                    spans.push((i, e));
                }
                i = e;
                continue;
            }
        }
        i += 1;
    }

    spans.sort_by_key(|(s, e)| std::cmp::Reverse(e - s));
    for (s, e) in spans.into_iter().take(4) {
        if let Ok(img) = image::load_from_memory_with_format(&bytes[s..e], image::ImageFormat::Jpeg)
        {
            // Reject postage-stamp previews; RAW files also embed a 160px icon.
            if img.width() >= 160 && img.height() >= 160 {
                return Some(img);
            }
        }
    }
    None
}

fn read_capped(path: &Path, cap: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata().map(|m| m.len() as usize).unwrap_or(cap);
    let mut buf = Vec::with_capacity(len.min(cap));
    f.take(cap as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

// ------------------------------------------------------------- exif ---------

#[derive(Debug, Default, Clone)]
pub struct Meta {
    pub taken: Option<i64>,
    pub orient: i64,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub fnum: Option<f64>,
    pub expo: Option<String>,
    pub focal: Option<f64>,
}

pub fn read_exif(path: &Path) -> Meta {
    let mut m = Meta { orient: 1, ..Default::default() };
    let Ok(file) = std::fs::File::open(path) else { return m };
    let mut r = std::io::BufReader::new(file);

    // Plenty of real-world cameras write EXIF that is slightly out of spec.
    // Strict parsing throws the entire block away over one bad tag, which costs
    // the capture date on files that plainly have one, so take partial results.
    let exif = match exif::Reader::new().continue_on_error(true).read_from_container(&mut r) {
        Ok(e) => e,
        Err(exif::Error::PartialResult(partial)) => partial.into_inner().0,
        Err(_) => return scan_dt_fallback(path, m),
    };

    use exif::{In, Tag, Value};
    let s = |t: Tag| {
        exif.get_field(t, In::PRIMARY)
            .map(|f| f.display_value().to_string().trim_matches('"').trim().to_string())
            .filter(|v| !v.is_empty())
    };

    if let Some(f) = exif.get_field(Tag::Orientation, In::PRIMARY) {
        if let Some(v) = f.value.get_uint(0) {
            m.orient = v as i64;
        }
    }
    m.camera = match (s(Tag::Make), s(Tag::Model)) {
        (Some(mk), Some(md)) if md.to_lowercase().starts_with(&mk.to_lowercase()) => Some(md),
        (Some(mk), Some(md)) => Some(format!("{mk} {md}")),
        (None, Some(md)) => Some(md),
        (Some(mk), None) => Some(mk),
        _ => None,
    };
    m.lens = s(Tag::LensModel);
    m.expo = s(Tag::ExposureTime);
    if let Some(f) = exif.get_field(Tag::PhotographicSensitivity, In::PRIMARY) {
        m.iso = f.value.get_uint(0).map(|v| v as i64);
    }
    if let Some(f) = exif.get_field(Tag::FNumber, In::PRIMARY) {
        if let Value::Rational(ref v) = f.value {
            m.fnum = v.first().map(|r| r.to_f64());
        }
    }
    if let Some(f) = exif.get_field(Tag::FocalLength, In::PRIMARY) {
        if let Value::Rational(ref v) = f.value {
            m.focal = v.first().map(|r| r.to_f64());
        }
    }
    m.taken = exif_taken(&exif);
    if m.taken.is_none() {
        return scan_dt_fallback(path, m);
    }
    m
}

/// The capture date, preferring the moment the shutter fired over the moment
/// the file was written.
pub fn exif_taken(exif: &exif::Exif) -> Option<i64> {
    use exif::{In, Tag};
    for tag in [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime] {
        if let Some(f) = exif.get_field(tag, In::PRIMARY) {
            if let Some(t) = parse_exif_dt(&f.display_value().to_string()) {
                return Some(t);
            }
        }
    }
    None
}

/// Last resort when the EXIF parser cannot make sense of the block: look for a
/// literal `YYYY:MM:DD HH:MM:SS` in the file's header. Bounded to the first
/// 128 KB, which is where APP1 always lives.
fn scan_dt_fallback(path: &Path, mut m: Meta) -> Meta {
    let Ok(head) = read_capped(path, 128 * 1024) else { return m };
    let digit = |b: u8| b.is_ascii_digit();
    let mut i = 0usize;
    while i + 19 <= head.len() {
        let w = &head[i..i + 19];
        if digit(w[0]) && digit(w[1]) && digit(w[2]) && digit(w[3])
            && (w[4] == b':' || w[4] == b'-')
            && digit(w[5]) && digit(w[6])
            && (w[7] == b':' || w[7] == b'-')
            && digit(w[8]) && digit(w[9])
            && (w[10] == b' ' || w[10] == b'T')
            && digit(w[11]) && digit(w[12]) && w[13] == b':'
            && digit(w[14]) && digit(w[15]) && w[16] == b':'
            && digit(w[17]) && digit(w[18])
        {
            if let Ok(text) = std::str::from_utf8(w) {
                if let Some(t) = parse_exif_dt(text) {
                    m.taken = Some(t);
                    return m;
                }
            }
        }
        i += 1;
    }
    m
}

/// EXIF stores local time with no zone. The raw tag is `YYYY:MM:DD HH:MM:SS`,
/// but kamadak-exif's `display_value` already renders it with dashes in the
/// date, so both spellings have to be accepted — normalising blindly turns
/// `20:24:17` into `20-24-17` and loses every capture date in the library.
fn parse_exif_dt(s: &str) -> Option<i64> {
    let s = s.trim().trim_matches('"').trim();
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return to_local(n);
        }
    }
    // Date with no time still beats falling back to the file's mtime.
    for fmt in ["%Y-%m-%d", "%Y:%m:%d"] {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
            return to_local(d.and_hms_opt(12, 0, 0)?);
        }
    }
    None
}

fn to_local(n: chrono::NaiveDateTime) -> Option<i64> {
    use chrono::TimeZone;
    match chrono::Local.from_local_datetime(&n) {
        chrono::offset::LocalResult::Single(d) => Some(d.timestamp()),
        chrono::offset::LocalResult::Ambiguous(d, _) => Some(d.timestamp()),
        // A time inside the spring-forward gap does not exist locally; nudge it.
        chrono::offset::LocalResult::None => chrono::Local
            .from_local_datetime(&(n + chrono::Duration::hours(1)))
            .single()
            .map(|d| d.timestamp()),
    }
}

/// Phone cameras put the capture time in the filename, and scans, screenshots
/// and exports frequently carry no EXIF at all. `PXL_20260607_164721605.jpg`
/// and `20260605_202417.jpg` both parse here.
pub fn date_from_name(name: &str) -> Option<i64> {
    let d: Vec<char> = name.chars().collect();
    for i in 0..d.len().saturating_sub(7) {
        if !d[i..i + 8].iter().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let num: String = d[i..i + 8].iter().collect();
        let year: i32 = num[0..4].parse().ok()?;
        if !(1990..=2099).contains(&year) {
            continue;
        }
        let month: u32 = num[4..6].parse().ok()?;
        let day: u32 = num[6..8].parse().ok()?;
        let Some(date) = chrono::NaiveDate::from_ymd_opt(year, month, day) else { continue };

        // An optional HHMMSS run follows, usually after a separator.
        let mut j = i + 8;
        if j < d.len() && (d[j] == '_' || d[j] == '-' || d[j] == ' ' || d[j] == 'T') {
            j += 1;
        }
        let time = if j + 6 <= d.len() && d[j..j + 6].iter().all(|c| c.is_ascii_digit()) {
            let t: String = d[j..j + 6].iter().collect();
            chrono::NaiveTime::from_hms_opt(
                t[0..2].parse().ok()?,
                t[2..4].parse().ok()?,
                t[4..6].parse().ok()?,
            )
        } else {
            None
        };
        return to_local(date.and_time(time.unwrap_or(chrono::NaiveTime::from_hms_opt(12, 0, 0)?)));
    }
    None
}

pub fn apply_orientation(img: DynamicImage, orient: i64) -> DynamicImage {
    match orient {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

// --------------------------------------------------------------- loading ----

/// Decode any supported file to pixels, at roughly `max_edge` where the format
/// lets us ask for that cheaply. Orientation is already applied.
pub fn load(path: &Path, kind: i64, ext: &str, max_edge: u32) -> Result<DynamicImage> {
    let img = match kind {
        KIND_VIDEO => {
            let probe = probe_video(path).unwrap_or_default();
            // 10% in, so we skip the black frame most clips open on.
            let seek = probe.dur.map(|d| (d * 0.1).clamp(0.0, d.max(0.0))).filter(|s| *s > 0.05);
            let jpg = ffmpeg_frame(path, seek, max_edge)
                .or_else(|_| ffmpeg_frame(path, None, max_edge))?;
            let img = image::load_from_memory_with_format(&jpg, image::ImageFormat::Jpeg)?;
            return Ok(match probe.rotation {
                90 | -270 => img.rotate90(),
                180 | -180 => img.rotate180(),
                270 | -90 => img.rotate270(),
                _ => img,
            });
        }
        KIND_RAW | KIND_LAYERED => {
            let bytes = read_capped(path, 320 * 1024 * 1024)?;
            match largest_embedded_jpeg(&bytes) {
                Some(i) => i,
                // No usable preview. ffmpeg can decode some DNG/TIFF-ish RAWs.
                None => {
                    let jpg = ffmpeg_frame(path, None, max_edge)
                        .map_err(|e| anyhow!("no embedded preview and ffmpeg failed: {e}"))?;
                    image::load_from_memory_with_format(&jpg, image::ImageFormat::Jpeg)?
                }
            }
        }
        _ if is_heic(ext) => {
            let jpg = ffmpeg_frame(path, None, max_edge)?;
            image::load_from_memory_with_format(&jpg, image::ImageFormat::Jpeg)?
        }
        _ => image::open(path).with_context(|| format!("decoding {}", path.display()))?,
    };

    let orient = read_exif(path).orient;
    Ok(apply_orientation(img, orient))
}

// ------------------------------------------------------------ thumbnails ----

pub fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let rgb = img.to_rgb8();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    enc.encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)?;
    Ok(out)
}

/// Centre-crop to a square, then downsample. Uniform tiles are what let the
/// grid position 250k items by arithmetic instead of measurement.
pub fn square_thumb(img: &DynamicImage, edge: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let s = w.min(h);
    let x = (w - s) / 2;
    let y = (h - s) / 2;
    img.crop_imm(x, y, s, s).thumbnail(edge, edge)
}

pub fn fit_thumb(img: &DynamicImage, max_edge: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w <= max_edge && h <= max_edge {
        return img.clone();
    }
    img.thumbnail(max_edge, max_edge)
}

/// 64-bit difference hash: downsample to 9x8 greyscale and record whether each
/// pixel is brighter than its right-hand neighbour. Robust to rescaling and
/// re-encoding, which is exactly what "the same photo twice" looks like.
pub fn dhash(img: &DynamicImage) -> i64 {
    let small = img.to_luma8();
    let small = image::imageops::resize(&small, 9, 8, image::imageops::FilterType::Triangle);
    let mut bits: u64 = 0;
    let mut n = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let l = small.get_pixel(x, y).0[0];
            let r = small.get_pixel(x + 1, y).0[0];
            if l > r {
                bits |= 1 << n;
            }
            n += 1;
        }
    }
    bits as i64
}

pub fn hamming(a: i64, b: i64) -> u32 {
    ((a as u64) ^ (b as u64)).count_ones()
}
