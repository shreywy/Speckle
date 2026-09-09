//! The indexer: walk libraries, then thumbnail everything new.
//!
//! Two phases, because they have wildly different costs. Walking is pure
//! filesystem metadata and finishes in seconds even on a spinning disk;
//! thumbnailing decodes every file and is what actually takes the time. Keeping
//! them separate means the grid is populated and browsable almost immediately,
//! with tiles filling in behind you.
//!
//! Sub-directories are walked to full depth and all belong to the top-level
//! library the user added — a folder with `dump/`, `edits/` and `selection/`
//! inside it is one library, not four.

use anyhow::Result;
use parking_lot::RwLock;
use rayon::prelude::*;
use rusqlite::params;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;
use walkdir::WalkDir;

use crate::db::{self, KIND_VIDEO, ST_DONE, ST_ERR, TIER_GRID};
use crate::decode;
use crate::App;

pub const GRID_EDGE: u32 = 320;
pub const PREVIEW_EDGE: u32 = 1600;
pub const BIN_DIRNAME: &str = ".speckle-bin";
/// Bins written before the rename; still skipped so old binned files stay hidden.
pub const LEGACY_BIN_DIRNAME: &str = ".loupe-bin";

#[derive(Clone, Debug, serde::Serialize)]
pub struct Job {
    pub running: bool,
    pub phase: String,
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub rate: f64,
    pub errors: u64,
    pub eta: f64,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            running: false,
            phase: "idle".into(),
            label: String::new(),
            done: 0,
            total: 0,
            rate: 0.0,
            errors: 0,
            eta: 0.0,
        }
    }
}

pub type JobState = Arc<RwLock<Job>>;

fn set<F: FnOnce(&mut Job)>(j: &JobState, f: F) {
    f(&mut j.write());
}

/// Kick off a scan on a background thread. Returns immediately; progress is
/// readable from the shared `Job`.
pub fn spawn(app: Arc<App>, libs: Option<Vec<i64>>, rethumb: bool) {
    spawn_ex(app, libs, rethumb, false)
}

/// `retry_failed` re-queues only the files that previously could not be read —
/// useful after a decoder fix, and far cheaper than rebuilding every thumbnail.
pub fn spawn_ex(app: Arc<App>, libs: Option<Vec<i64>>, rethumb: bool, retry_failed: bool) {
    if app.job.read().running {
        // Something else has the disk. Remember that a scan is owed rather than
        // dropping it — otherwise adding a folder during a tagging pass would
        // quietly never index anything.
        app.rescan_pending.store(true, std::sync::atomic::Ordering::Relaxed);
        if retry_failed {
            app.retry_pending.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    set(&app.job, |j| {
        *j = Job { running: true, phase: "scanning".into(), ..Default::default() }
    });

    std::thread::spawn(move || {
        if retry_failed {
            if let Ok(c) = app.index.get() {
                match c.execute("UPDATE media SET state=0, err=NULL WHERE state=2", []) {
                    Ok(n) => println!("[scan] retrying {n} previously unreadable files"),
                    Err(e) => eprintln!("[scan] retry reset failed: {e}"),
                }
            }
        }
        if let Err(e) = run(&app, libs, rethumb) {
            eprintln!("[scan] failed: {e:#}");
            set(&app.job, |j| j.label = format!("failed: {e}"));
        }
        set(&app.job, |j| {
            j.running = false;
            j.phase = "idle".into();
            j.label.clear();
        });
    });
}

fn run(app: &Arc<App>, libs: Option<Vec<i64>>, rethumb: bool) -> Result<()> {
    let conn = app.index.standalone()?;

    let targets: Vec<(i64, String)> = {
        let mut st = conn.prepare("SELECT id, path FROM libraries ORDER BY id")?;
        let all: Vec<(i64, String)> =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        match &libs {
            Some(ids) => all.into_iter().filter(|(id, _)| ids.contains(id)).collect(),
            None => all,
        }
    };

    for (lib_id, root) in &targets {
        let excludes: Vec<String> = conn
            .query_row("SELECT excludes FROM libraries WHERE id=?1", [lib_id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap_or_default()
            .lines()
            .map(|s| s.trim().trim_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        walk_library(app, &conn, *lib_id, Path::new(root), &excludes)?;
    }

    if rethumb {
        let mut q = String::from("UPDATE media SET state=0, err=NULL WHERE 1=1");
        if let Some(ids) = &libs {
            q.push_str(&format!(
                " AND lib IN ({})",
                ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
            ));
        }
        conn.execute(&q, [])?;
    }

    thumbnail_pass(app, conn)
}

/// Phase one. Recursive, depth-unlimited, and cheap: nothing here opens a file.
fn walk_library(
    app: &Arc<App>,
    conn: &rusqlite::Connection,
    lib: i64,
    root: &Path,
    excludes: &[String],
) -> Result<()> {
    set(&app.job, |j| {
        j.phase = "scanning".into();
        j.label = root.display().to_string();
        j.done = 0;
        j.total = 0;
    });

    let data_dir = app.data_dir.clone();
    let mut seen: HashSet<String> = HashSet::new();
    let mut batch: Vec<Row> = Vec::with_capacity(1024);
    let mut found: u64 = 0;

    let walker = WalkDir::new(root).follow_links(false).into_iter().filter_entry(|e| {
        if !e.file_type().is_dir() {
            return true;
        }
        let name = e.file_name().to_string_lossy();
        // Our own bin and cache never get indexed, nor do the usual noise dirs.
        if name == BIN_DIRNAME || name == LEGACY_BIN_DIRNAME
            || name.eq_ignore_ascii_case("speckle-data")
            || name.eq_ignore_ascii_case("loupe-data")
        {
            return false;
        }
        if name.starts_with('$') || name == "System Volume Information" {
            return false;
        }
        // Folders the user has chosen to leave out, matched on the path
        // relative to the library root so "Screenshots" cannot accidentally
        // exclude "Holiday/Screenshots of maps" elsewhere.
        if let Ok(rel) = e.path().strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if excludes.iter().any(|x| rel == *x || rel.starts_with(&format!("{x}/"))) {
                return false;
            }
        }
        e.path() != data_dir
    });

    for entry in walker.filter_map(Result::ok) {
        if app.job.read().running == false {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
        let Some(kind) = decode::classify(ext) else { continue };

        let Ok(md) = entry.metadata() else { continue };
        let bytes = md.len() as i64;
        if bytes == 0 {
            continue;
        }
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let abs = path.to_string_lossy().replace('\\', "/");
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        // The chain of sub-directories under the library root, '' at the top.
        let sub = match rel.rfind('/') {
            Some(i) => rel[..i].to_string(),
            None => String::new(),
        };

        seen.insert(abs.clone());
        batch.push(Row {
            lib,
            path: abs,
            rel,
            sub,
            name: entry.file_name().to_string_lossy().to_string(),
            ext: ext.to_ascii_lowercase(),
            kind,
            bytes,
            mtime,
        });
        found += 1;

        if batch.len() >= 1024 {
            flush(conn, &mut batch)?;
            set(&app.job, |j| {
                j.done = found;
                j.total = found;
                j.label = format!("{} files", found);
            });
        }
    }
    flush(conn, &mut batch)?;

    // Anything in the database we did not meet on this walk has been moved or
    // deleted outside the app. Flag rather than drop, so ratings survive a
    // drive being temporarily offline.
    let mut st = conn.prepare("SELECT id, path FROM media WHERE lib=?1 AND deleted IS NULL")?;
    let rows: Vec<(i64, String)> =
        st.query_map([lib], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let gone: Vec<i64> = rows.into_iter().filter(|(_, p)| !seen.contains(p)).map(|(i, _)| i).collect();
    if !gone.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for id in &gone {
            tx.execute("UPDATE media SET missing=1 WHERE id=?1", [id])?;
        }
        tx.commit()?;
    }

    set(&app.job, |j| j.label = format!("{} files in {}", found, root.display()));
    Ok(())
}

struct Row {
    lib: i64,
    path: String,
    rel: String,
    sub: String,
    name: String,
    ext: String,
    kind: i64,
    bytes: i64,
    mtime: i64,
}

/// Insert new rows; for rows we already have, only reset the thumbnail state if
/// the file actually changed on disk.
fn flush(conn: &rusqlite::Connection, batch: &mut Vec<Row>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO media (lib,path,rel,sub,name,ext,kind,bytes,mtime,state,added_at,missing)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,0)
             ON CONFLICT(path) DO UPDATE SET
               missing = 0,
               lib     = excluded.lib,
               rel     = excluded.rel,
               sub     = excluded.sub,
               state   = CASE WHEN media.bytes <> excluded.bytes OR media.mtime <> excluded.mtime
                              THEN 0 ELSE media.state END,
               bytes   = excluded.bytes,
               mtime   = excluded.mtime",
        )?;
        let now = db::now();
        for r in batch.iter() {
            ins.execute(params![
                r.lib, r.path, r.rel, r.sub, r.name, r.ext, r.kind, r.bytes, r.mtime, now
            ])?;
        }
    }
    tx.commit()?;
    batch.clear();
    Ok(())
}

// -------------------------------------------------------------- phase two ---

struct Product {
    id: i64,
    grid: Option<Vec<u8>>,
    w: u32,
    h: u32,
    dhash: i64,
    meta: decode::Meta,
    probe: Option<decode::Probe>,
    err: Option<String>,
}

/// Phase two. Decode every file that has no thumbnail yet, in parallel, and
/// funnel the results to a single writer so SQLite only ever sees one writer.
fn thumbnail_pass(app: &Arc<App>, conn: rusqlite::Connection) -> Result<()> {
    let pending: Vec<(i64, String, i64, String)> = {
        let mut st = conn.prepare(
            "SELECT id, path, kind, ext FROM media
             WHERE state=0 AND missing=0 ORDER BY mtime DESC",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?
    };

    let total = pending.len() as u64;
    if total == 0 {
        return Ok(());
    }
    set(&app.job, |j| {
        j.phase = "thumbnailing".into();
        j.done = 0;
        j.total = total;
        j.errors = 0;
    });

    let (tx, rx) = mpsc::sync_channel::<Product>(256);
    let job = app.job.clone();
    let thumbs_path = app.thumbs.path().to_path_buf();

    // Writer thread: owns both connections for the duration of the pass.
    let writer = std::thread::spawn(move || -> Result<()> {
        let tconn = rusqlite::Connection::open(&thumbs_path)?;
        tconn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=15000;",
        )?;

        let start = Instant::now();
        let mut done: u64 = 0;
        let mut errors: u64 = 0;
        let mut pending_batch = Vec::with_capacity(64);

        let commit = |conn: &rusqlite::Connection,
                      tconn: &rusqlite::Connection,
                      batch: &mut Vec<Product>|
         -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let tx1 = conn.unchecked_transaction()?;
            let tx2 = tconn.unchecked_transaction()?;
            {
                let mut up = tx1.prepare_cached(
                    "UPDATE media SET state=?2, err=?3, w=?4, h=?5, taken=?6, orient=?7,
                       camera=?8, lens=?9, iso=?10, fnum=?11, expo=?12, focal=?13,
                       dur=?14, vcodec=?15, acodec=?16, dhash=?17
                     WHERE id=?1",
                )?;
                let mut put = tx2.prepare_cached(
                    "INSERT INTO t(id,tier,data) VALUES(?1,?2,?3)
                     ON CONFLICT(id,tier) DO UPDATE SET data=excluded.data",
                )?;
                for p in batch.iter() {
                    let state = if p.err.is_some() { ST_ERR } else { ST_DONE };
                    let pr = p.probe.clone().unwrap_or_default();
                    up.execute(params![
                        p.id,
                        state,
                        p.err,
                        p.w,
                        p.h,
                        p.meta.taken.or(pr.taken),
                        p.meta.orient,
                        p.meta.camera,
                        p.meta.lens,
                        p.meta.iso,
                        p.meta.fnum,
                        p.meta.expo,
                        p.meta.focal,
                        pr.dur,
                        pr.vcodec,
                        pr.acodec,
                        p.dhash,
                    ])?;
                    if let Some(g) = &p.grid {
                        put.execute(params![p.id, TIER_GRID, g])?;
                    }
                }
            }
            tx1.commit()?;
            tx2.commit()?;
            batch.clear();
            Ok(())
        };

        while let Ok(p) = rx.recv() {
            if p.err.is_some() {
                errors += 1;
            }
            pending_batch.push(p);
            done += 1;
            if pending_batch.len() >= 48 {
                commit(&conn, &tconn, &mut pending_batch)?;
            }
            if done % 8 == 0 || done == total {
                let secs = start.elapsed().as_secs_f64().max(0.001);
                let rate = done as f64 / secs;
                let mut j = job.write();
                j.done = done;
                j.errors = errors;
                j.rate = rate;
                j.eta = if rate > 0.0 { (total - done) as f64 / rate } else { 0.0 };
            }
        }
        commit(&conn, &tconn, &mut pending_batch)?;
        Ok(())
    });

    let label = app.job.clone();
    pending.par_iter().for_each_with(tx, |tx, (id, path, kind, ext)| {
        let p = build_one(*id, Path::new(path), *kind, ext);
        {
            let mut j = label.write();
            if j.done % 16 == 0 {
                j.label = Path::new(path)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
        }
        let _ = tx.send(p);
    });

    writer.join().map_err(|_| anyhow::anyhow!("thumbnail writer panicked"))??;
    Ok(())
}

fn build_one(id: i64, path: &Path, kind: i64, ext: &str) -> Product {
    let mut out = Product {
        id,
        grid: None,
        w: 0,
        h: 0,
        dhash: 0,
        meta: decode::Meta { orient: 1, ..Default::default() },
        probe: None,
        err: None,
    };

    if kind == KIND_VIDEO {
        out.probe = decode::probe_video(path).ok();
    } else {
        out.meta = decode::read_exif(path);
    }
    // Fall back to a date encoded in the filename before giving up and letting
    // the query fall through to the file's mtime.
    if out.meta.taken.is_none() && out.probe.as_ref().and_then(|p| p.taken).is_none() {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            out.meta.taken = decode::date_from_name(name);
        }
    }

    // Decode at preview size: big enough that the square crop is clean, small
    // enough that we are not hauling 24 megapixels through memory per worker.
    match decode::load(path, kind, ext, 1024) {
        Ok(img) => {
            use image::GenericImageView;
            let (w, h) = img.dimensions();
            out.w = w;
            out.h = h;
            out.dhash = decode::dhash(&img);
            let sq = decode::square_thumb(&img, GRID_EDGE);
            match decode::encode_jpeg(&sq, 78) {
                Ok(b) => out.grid = Some(b),
                Err(e) => out.err = Some(e.to_string()),
            }
        }
        Err(e) => out.err = Some(format!("{e:#}")),
    }

    // Videos report real pixel dimensions from ffprobe, which beats whatever
    // the extracted frame happened to be scaled to.
    if let Some(p) = &out.probe {
        if let (Some(w), Some(h)) = (p.w, p.h) {
            let swap = matches!(p.rotation, 90 | -90 | 270 | -270);
            out.w = if swap { h } else { w };
            out.h = if swap { w } else { h };
        }
    }
    out
}

/// Build (and cache) the larger preview tier on demand. Doing this lazily keeps
/// the initial index fast and the cache small — most photos are never opened.
pub fn ensure_preview(app: &App, id: i64) -> Result<Vec<u8>> {
    {
        let t = app.thumbs.get()?;
        if let Ok(b) = t.query_row(
            "SELECT data FROM t WHERE id=?1 AND tier=?2",
            params![id, db::TIER_PREVIEW],
            |r| r.get::<_, Vec<u8>>(0),
        ) {
            return Ok(b);
        }
    }

    let (path, kind, ext): (String, i64, String) = {
        let c = app.index.get()?;
        c.query_row("SELECT path, kind, ext FROM media WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
    };

    let img = decode::load(Path::new(&path), kind, &ext, PREVIEW_EDGE)?;
    let fit = decode::fit_thumb(&img, PREVIEW_EDGE);
    let bytes = decode::encode_jpeg(&fit, 84)?;

    let t = app.thumbs.get()?;
    t.execute(
        "INSERT INTO t(id,tier,data) VALUES(?1,?2,?3)
         ON CONFLICT(id,tier) DO UPDATE SET data=excluded.data",
        params![id, db::TIER_PREVIEW, &bytes],
    )?;
    Ok(bytes)
}

/// Content hashes are only needed for exact-duplicate detection, so they are
/// computed when the user asks for that rather than during indexing.
pub fn ensure_hashes(app: &Arc<App>) -> Result<()> {
    let conn = app.index.standalone()?;
    let todo: Vec<(i64, String)> = {
        let mut st = conn.prepare(
            "SELECT id, path FROM media WHERE hash IS NULL AND missing=0 AND deleted IS NULL",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
    };
    if todo.is_empty() {
        return Ok(());
    }

    set(&app.job, |j| {
        j.running = true;
        j.phase = "hashing".into();
        j.done = 0;
        j.total = todo.len() as u64;
    });

    let results: Vec<(i64, String)> = todo
        .par_iter()
        .filter_map(|(id, p)| {
            let mut h = blake3::Hasher::new();
            let mut f = std::fs::File::open(p).ok()?;
            std::io::copy(&mut f, &mut h).ok()?;
            {
                let mut j = app.job.write();
                j.done += 1;
            }
            Some((*id, h.finalize().to_hex().to_string()))
        })
        .collect();

    let tx = conn.unchecked_transaction()?;
    for (id, h) in &results {
        tx.execute("UPDATE media SET hash=?2 WHERE id=?1", params![id, h])?;
    }
    tx.commit()?;

    set(&app.job, |j| {
        j.running = false;
        j.phase = "idle".into();
    });
    Ok(())
}

/// Where a binned file goes: a hidden folder at the root of its own library, so
/// the move is a same-volume rename — instant, and reversible for free.
pub fn bin_dir(lib_root: &str) -> PathBuf {
    Path::new(lib_root).join(BIN_DIRNAME)
}

pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let mut p = dir.join(name);
    let mut n = 1;
    while p.exists() {
        let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = Path::new(name).extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}
