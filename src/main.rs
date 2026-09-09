//! Speckle — a fast local photo and video library.
//!
//! One process does everything: it serves the entire UI over HTTP on the local
//! machine, and opens a WebView2 window pointed at that same server. The
//! desktop window and a phone on your tailnet are therefore the *same client*
//! talking to the *same API*, which is why there is only one UI codebase to
//! maintain and why anything you can do at the desk you can also do from bed.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod db;
mod decode;
mod ml;
mod scan;
mod sets;
mod stream;
mod tools;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::net::TcpListener as StdListener;
use std::path::PathBuf;
use std::sync::Arc;

pub struct App {
    pub data_dir: PathBuf,
    pub index: db::Pool,
    pub thumbs: db::Pool,
    pub job: scan::JobState,
    pub ffmpeg: bool,
    pub port: u16,
    pub addrs: Vec<String>,
    pub started: std::time::Instant,
    pub jobs: RwLock<tools::JobBoard>,
    /// Loaded lazily on first use and kept alive afterwards — an ONNX session
    /// costs a second to build and is fine to share.
    pub clip: RwLock<Option<std::sync::Arc<ml::Clip>>>,
    pub faces: RwLock<Option<std::sync::Arc<ml::FaceNet>>>,
    /// Set when a scan was asked for while something else was running. The
    /// background worker picks it up, so a request is never silently lost.
    pub rescan_pending: std::sync::atomic::AtomicBool,
    /// Set when the deferred scan should also re-attempt previously unreadable
    /// files, so asking for a retry during a long pass is not silently downgraded.
    pub retry_pending: std::sync::atomic::AtomicBool,
    /// Counts for the status bar, recomputed at most every couple of seconds.
    /// The UI polls continuously, and a dozen COUNT(*) queries over a quarter of
    /// a million rows is not something to run on every poll.
    pub counts_cache: RwLock<Option<(std::time::Instant, serde_json::Value)>>,
}

fn main() -> Result<()> {
    let headless = std::env::args().any(|a| a == "--server" || a == "--headless");

    let data_dir = pick_data_dir();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;

    // Prefer our own ffmpeg over PATH, when one has been downloaded.
    decode::set_tools_dir(data_dir.join("tools"));

    let index = db::Pool::new(data_dir.join("index.db"), db::INDEX_SCHEMA)?;
    let thumbs = db::Pool::new(data_dir.join("thumbs.db"), db::THUMB_SCHEMA)?;
    let port = free_port(7420);
    let ffmpeg = decode::ffmpeg_available();
    let addrs = local_addrs();

    if !ffmpeg {
        eprintln!(
            "[speckle] ffmpeg not found on PATH — video thumbnails, HEIC decoding and \
             transcoding will be unavailable. Everything else works."
        );
    }

    let app = Arc::new(App {
        data_dir: data_dir.clone(),
        index,
        thumbs,
        job: Arc::new(RwLock::new(scan::Job::default())),
        ffmpeg,
        port,
        addrs: addrs.clone(),
        started: std::time::Instant::now(),
        jobs: RwLock::new(tools::JobBoard::default()),
        clip: RwLock::new(None),
        faces: RwLock::new(None),
        rescan_pending: std::sync::atomic::AtomicBool::new(false),
        retry_pending: std::sync::atomic::AtomicBool::new(false),
        counts_cache: RwLock::new(None),
    });

    println!("[speckle] data      {}", data_dir.display());
    println!("[speckle] listening http://127.0.0.1:{port}");
    for a in &addrs {
        println!("[speckle] remote    http://{a}:{port}");
    }

    // The HTTP server owns its own tokio runtime on a dedicated thread; the
    // main thread has to stay free for the Windows message loop.
    let server_app = app.clone();
    std::thread::Builder::new().name("speckle-http".into()).spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("speckle-rt")
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            if let Err(e) = api::serve(server_app).await {
                eprintln!("[speckle] server stopped: {e:#}");
            }
        });
    })?;

    // Pick up where we left off: anything added but never thumbnailed, plus a
    // re-walk to catch files that changed while we were closed.
    let boot = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let has_libs = boot
            .index
            .get()
            .ok()
            .and_then(|c| c.query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get::<_, i64>(0)).ok())
            .unwrap_or(0)
            > 0;
        let days = boot
            .index
            .get()
            .ok()
            .and_then(|c| db::get_setting(&c, "bin_days"))
            .and_then(|v| v.trim_matches('"').parse::<i64>().ok())
            .unwrap_or(30);
        match tools::auto_purge(&boot, days) {
            Ok(n) if n > 0 => println!("[speckle] purged {n} expired bin entries"),
            Err(e) => eprintln!("[speckle] auto-purge: {e:#}"),
            _ => {}
        }
        if has_libs {
            scan::spawn(boot.clone(), None, false);
        }
        // From here on, tagging and face grouping look after themselves: the
        // worker picks up any backlog left by a new folder, an upload or a
        // rescan, and works through it whenever nothing else is running.
        ml::spawn_worker(boot);
    });

    if headless {
        println!("[speckle] headless — press Ctrl+C to stop");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    open_window(port)
}

/// Portable by preference: keep the cache and index beside the executable so
/// the whole thing can live on an external drive. Falls back to the usual
/// per-user location when the executable sits somewhere unwritable, such as
/// Program Files.
fn pick_data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // A cargo target directory is not where anyone wants a 3 GB cache.
            let in_target = dir.components().any(|c| c.as_os_str() == "target");
            let candidate = dir.join("speckle-data");
            if !in_target && std::fs::create_dir_all(&candidate).is_ok() {
                let probe = candidate.join(".writable");
                if std::fs::write(&probe, b"1").is_ok() {
                    let _ = std::fs::remove_file(&probe);
                    return candidate;
                }
            }
        }
    }
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Speckle")
}

fn free_port(start: u16) -> u16 {
    for p in start..start + 40 {
        if StdListener::bind(("0.0.0.0", p)).is_ok() {
            return p;
        }
    }
    start
}

/// Addresses worth printing so the user knows what to type on their phone.
/// Tailscale is asked directly when it is installed, because its address is the
/// one that works from anywhere.
fn local_addrs() -> Vec<String> {
    let mut out = Vec::new();

    let mut cmd = std::process::Command::new("tailscale");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    if let Ok(o) = cmd.args(["ip", "-4"]).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let t = line.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
    }

    // The address this machine would use to reach the outside world: no packet
    // is actually sent, the OS just resolves the route.
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(a) = s.local_addr() {
                let ip = a.ip().to_string();
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
    }
    out
}

fn open_window(port: u16) -> Result<()> {
    use tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Speckle")
        .with_inner_size(LogicalSize::new(1440.0, 920.0))
        .with_min_inner_size(LogicalSize::new(880.0, 560.0))
        .build(&event_loop)?;

    let _webview = WebViewBuilder::new()
        .with_url(format!("http://127.0.0.1:{port}/"))
        .with_background_color((16, 17, 20, 255))
        .with_devtools(cfg!(debug_assertions))
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
