//! Byte-range file serving and on-demand video transcoding.
//!
//! Two thirds of a phone camera roll will not play in a WebView: iPhone .MOV is
//! HEVC with PCM audio, old camcorder .AVI is MJPEG. Rather than pre-converting
//! a library (slow, destructive, enormous), anything unplayable is re-encoded
//! to fragmented MP4 on the way out and thrown away afterwards.

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::Path;
use std::process::Stdio;
use tokio_util::io::ReaderStream;

/// `bytes=START-END` — the only form browsers actually send.
fn parse_range(h: &HeaderMap, len: u64) -> Option<(u64, u64)> {
    let v = h.get(header::RANGE)?.to_str().ok()?;
    let spec = v.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        // Suffix form: the last N bytes.
        let n: u64 = b.parse().ok()?;
        (len.saturating_sub(n), len - 1)
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() { len - 1 } else { b.parse::<u64>().ok()?.min(len - 1) };
        (s, e)
    };
    if start > end || start >= len {
        None
    } else {
        Some((start, end))
    }
}

/// Serve a file, honouring Range so the video element can seek and so large
/// images stream rather than buffering whole.
pub async fn file_range(path: &Path, headers: &HeaderMap, mime: &str, download_as: Option<&str>) -> Response {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let len = meta.len();

    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    if let Some(name) = download_as {
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", name.replace('"', ""))) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }

    let Ok(mut f) = tokio::fs::File::open(path).await else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    match parse_range(headers, len) {
        Some((start, end)) => {
            use tokio::io::AsyncSeekExt;
            if f.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                return (StatusCode::INTERNAL_SERVER_ERROR, "seek failed").into_response();
            }
            let n = end - start + 1;
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
            h.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")).unwrap(),
            );
            let body = Body::from_stream(ReaderStream::new(tokio::io::AsyncReadExt::take(f, n)));
            (StatusCode::PARTIAL_CONTENT, h, body).into_response()
        }
        None => {
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            (StatusCode::OK, h, Body::from_stream(ReaderStream::new(f))).into_response()
        }
    }
}

/// Re-encode to fragmented MP4 and stream it as it is produced. Fragmented MP4
/// can start playing before the encode finishes, which a normal MP4 cannot
/// because its index lives at the end of the file.
///
/// Seeking works by asking for a new stream at an offset — there is no index to
/// seek within, so the client re-requests with `t=`.
pub fn transcode(path: &Path, seek: f64, max_height: u32) -> Response {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.arg("-v").arg("error").arg("-nostdin");
    if seek > 0.05 {
        cmd.arg("-ss").arg(format!("{seek:.3}"));
    }
    cmd.arg("-i")
        .arg(path)
        .args(["-map", "0:v:0", "-map", "0:a:0?"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"])
        .args(["-profile:v", "high", "-level", "4.1", "-pix_fmt", "yuv420p"])
        .arg("-vf")
        .arg(format!("scale='trunc(min(1,{h}/ih)*iw/2)*2':'trunc(min(ih,{h})/2)*2'", h = max_height))
        .args(["-c:a", "aac", "-b:a", "160k", "-ac", "2"])
        .args(["-movflags", "frag_keyframe+empty_moov+default_base_moof"])
        .args(["-f", "mp4", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("ffmpeg unavailable: {e}"))
                .into_response()
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "no ffmpeg output").into_response();
    };

    // Reap the process once the client stops reading; dropping the stream closes
    // the pipe, ffmpeg sees a broken pipe and exits on its own.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Deliberately no Accept-Ranges: this stream cannot satisfy byte ranges,
    // and claiming otherwise makes browsers seek into nonsense.
    (StatusCode::OK, h, Body::from_stream(ReaderStream::new(stdout))).into_response()
}

pub fn bytes_response(data: Vec<u8>, mime: &'static str, cache: bool) -> Response {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(data.len()));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if cache { "private, max-age=31536000, immutable" } else { "no-store" }),
    );
    (StatusCode::OK, h, Body::from(data)).into_response()
}
