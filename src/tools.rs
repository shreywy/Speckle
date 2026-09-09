//! Everything that changes files: the bin, duplicate detection, storage
//! accounting, bulk recompression and saving edits.
//!
//! The rule throughout: originals are never destroyed by a bulk operation. They
//! go to the bin, which is a same-drive rename, and the user has to empty it
//! deliberately. That is what makes it safe to offer a button labelled
//! "reclaim 183 GB".

use anyhow::{bail, Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::db::{self, KIND_PHOTO, KIND_RAW, KIND_VIDEO};
use crate::decode;
use crate::scan;
use crate::App;

/// Sizes in whichever unit keeps the number readable — "0.0 GB" after a real
/// saving reads like the job did nothing.
fn human_bytes(b: i64) -> String {
    let f = b as f64;
    if f >= 1_073_741_824.0 {
        format!("{:.2} GB", f / 1_073_741_824.0)
    } else if f >= 1_048_576.0 {
        format!("{:.1} MB", f / 1_048_576.0)
    } else {
        format!("{:.0} KB", f / 1024.0)
    }
}

/// Drop rows that point at media which no longer exists. Called after anything
/// that removes photos, so counts stay honest and a rebuilt library does not
/// inherit a previous one's tags.
pub fn prune_orphans(conn: &rusqlite::Connection) -> Result<()> {
    for sql in [
        "DELETE FROM clip WHERE media NOT IN (SELECT id FROM media)",
        "DELETE FROM faces WHERE media NOT IN (SELECT id FROM media)",
        "DELETE FROM media_tags WHERE media_id NOT IN (SELECT id FROM media)",
        "DELETE FROM collection_items WHERE media NOT IN (SELECT id FROM media)",
        "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM media_tags)",
        "DELETE FROM people WHERE id NOT IN (SELECT DISTINCT cluster FROM faces WHERE cluster IS NOT NULL)",
    ] {
        conn.execute(sql, [])?;
    }
    Ok(())
}

// ------------------------------------------------------------- job board ----

#[derive(Clone, Debug, Serialize, Default)]
pub struct TaskInfo {
    pub kind: String,
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub running: bool,
    pub message: String,
    pub freed: i64,
    pub failed: u64,
}

#[derive(Default, Serialize, Clone)]
pub struct JobBoard {
    pub task: Option<TaskInfo>,
}

fn task_set(app: &App, f: impl FnOnce(&mut TaskInfo)) {
    let mut b = app.jobs.write();
    let t = b.task.get_or_insert_with(TaskInfo::default);
    f(t);
}

// ------------------------------------------------------------------ bin -----

fn lib_root(conn: &rusqlite::Connection, lib: i64) -> Result<String> {
    Ok(conn.query_row("SELECT path FROM libraries WHERE id=?1", [lib], |r| r.get(0))?)
}

/// Move files into their library's hidden bin. Same volume, so this is a rename
/// and costs nothing regardless of file size.
pub fn bin(app: &App, ids: &[i64]) -> Result<usize> {
    let conn = app.index.get()?;
    let mut moved = 0;
    for id in ids {
        let row: Result<(i64, String, String), _> = conn.query_row(
            "SELECT lib, path, name FROM media WHERE id=?1 AND deleted IS NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        let Ok((lib, path, name)) = row else { continue };
        let root = lib_root(&conn, lib)?;
        let dir = scan::bin_dir(&root);
        std::fs::create_dir_all(&dir)?;

        let dest = scan::unique_dest(&dir, &format!("{id}__{name}"));
        match std::fs::rename(&path, &dest) {
            Ok(_) => {}
            Err(_) => {
                // Crossing a volume boundary (a junction, say) needs a real copy.
                std::fs::copy(&path, &dest)?;
                std::fs::remove_file(&path)?;
            }
        }
        conn.execute(
            "UPDATE media SET deleted=?2, binpath=?3 WHERE id=?1",
            params![id, db::now(), dest.to_string_lossy().replace('\\', "/")],
        )?;
        moved += 1;
    }
    Ok(moved)
}

pub fn restore(app: &App, ids: &[i64]) -> Result<usize> {
    let conn = app.index.get()?;
    let mut back = 0;
    for id in ids {
        let row: Result<(String, String), _> = conn.query_row(
            "SELECT path, binpath FROM media WHERE id=?1 AND deleted IS NOT NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        let Ok((path, binpath)) = row else { continue };
        if let Some(parent) = Path::new(&path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // If something has since taken the original name, restore alongside it
        // rather than clobbering whatever is there now.
        let dest = if Path::new(&path).exists() {
            let p = Path::new(&path);
            scan::unique_dest(p.parent().unwrap_or(Path::new(".")), &p.file_name().unwrap_or_default().to_string_lossy())
        } else {
            PathBuf::from(&path)
        };
        if std::fs::rename(&binpath, &dest).is_err() {
            std::fs::copy(&binpath, &dest)?;
            std::fs::remove_file(&binpath).ok();
        }
        conn.execute(
            "UPDATE media SET deleted=NULL, binpath=NULL, path=?2 WHERE id=?1",
            params![id, dest.to_string_lossy().replace('\\', "/")],
        )?;
        back += 1;
    }
    Ok(back)
}

/// Irreversible. Only ever reached from an explicit confirmation.
pub fn purge(app: &App, ids: Option<&[i64]>) -> Result<(usize, i64)> {
    let conn = app.index.get()?;
    let targets: Vec<(i64, String, i64)> = match ids {
        Some(list) => {
            let mut v = Vec::new();
            for id in list {
                if let Ok(r) = conn.query_row(
                    "SELECT id, binpath, bytes FROM media WHERE id=?1 AND deleted IS NOT NULL",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                ) {
                    v.push(r);
                }
            }
            v
        }
        None => {
            let mut st = conn
                .prepare("SELECT id, binpath, bytes FROM media WHERE deleted IS NOT NULL")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?
        }
    };

    let mut freed = 0i64;
    let thumbs = app.thumbs.get()?;
    for (id, binpath, bytes) in &targets {
        if std::fs::remove_file(binpath).is_ok() {
            freed += bytes;
        }
        conn.execute("DELETE FROM media WHERE id=?1", [id])?;
        thumbs.execute("DELETE FROM t WHERE id=?1", [id]).ok();
    }
    prune_orphans(&conn)?;
    Ok((targets.len(), freed))
}

/// Anything sitting in the bin past its retention window goes for good. Called
/// on startup and after every bin operation.
pub fn auto_purge(app: &App, days: i64) -> Result<usize> {
    if days <= 0 {
        return Ok(0);
    }
    let cutoff = db::now() - days * 86_400;
    let conn = app.index.get()?;
    let ids: Vec<i64> = {
        let mut st =
            conn.prepare("SELECT id FROM media WHERE deleted IS NOT NULL AND deleted < ?1")?;
        st.query_map([cutoff], |r| r.get(0))?.collect::<Result<_, _>>()?
    };
    drop(conn);
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(purge(app, Some(&ids))?.0)
}

// -------------------------------------------------------------- storage -----

fn dir_size(p: &Path) -> u64 {
    walkdir::WalkDir::new(p)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Is this photo carrying far more bytes than its pixel count justifies? A
/// well-encoded JPEG lands near 0.35 MB per megapixel; past three times that,
/// recompression is nearly free visually.
fn oversized(bytes: i64, w: Option<i64>, h: Option<i64>) -> bool {
    let (Some(w), Some(h)) = (w, h) else { return bytes > 12_000_000 };
    let mp = (w as f64 * h as f64) / 1_000_000.0;
    if mp < 0.5 {
        return false;
    }
    bytes as f64 > (mp * 1_100_000.0).max(2_500_000.0)
}

/// What a re-encode would plausibly return, by how efficient the current codec
/// is. Deliberately conservative — an estimate that overpromises is worse than
/// no estimate.
fn video_saving(vcodec: Option<&str>, bytes: i64) -> i64 {
    let f = match vcodec.unwrap_or("") {
        "mjpeg" | "rawvideo" | "msmpeg4v3" | "msmpeg4v2" | "mpeg4" | "wmv3" | "wmv2" | "mpeg2video" | "mpeg1video" => 0.80,
        "h264" | "vp8" => 0.42,
        _ => 0.0,
    };
    (bytes as f64 * f) as i64
}

pub fn storage(app: &Arc<App>) -> Result<serde_json::Value> {
    let conn = app.index.get()?;

    let by_kind = |kind: i64| -> (i64, i64) {
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(bytes),0) FROM media WHERE kind=?1 AND deleted IS NULL AND missing=0",
            [kind],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0))
    };
    let (n_photo, b_photo) = by_kind(KIND_PHOTO);
    let (n_video, b_video) = by_kind(KIND_VIDEO);
    let (n_raw, b_raw) = by_kind(KIND_RAW);
    let (n_lay, b_lay) = by_kind(db::KIND_LAYERED);
    let (n_bin, b_bin): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(bytes),0) FROM media WHERE deleted IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));

    // The data directory also holds the ML weights, which are not a thumbnail
    // cache and would otherwise show as 350 MB of "cache" for a small library.
    let models = dir_size(&app.data_dir.join("models")) as i64;
    let cache = dir_size(&app.data_dir) as i64 - models;

    // Oversized photo candidates.
    let mut big_n = 0i64;
    let mut big_saving = 0i64;
    {
        let mut st = conn.prepare(
            "SELECT bytes, w, h FROM media
             WHERE kind IN (0) AND deleted IS NULL AND missing=0 AND bytes > 2500000",
        )?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?))
        })?;
        for row in rows.flatten() {
            if oversized(row.0, row.1, row.2) {
                big_n += 1;
                let mp = row.1.unwrap_or(4000) as f64 * row.2.unwrap_or(3000) as f64 / 1e6;
                let target = (mp * 420_000.0) as i64;
                big_saving += (row.0 - target).max(0);
            }
        }
    }

    // Video re-encode candidates.
    let mut vid_n = 0i64;
    let mut vid_saving = 0i64;
    {
        let mut st = conn.prepare(
            "SELECT bytes, vcodec FROM media WHERE kind=1 AND deleted IS NULL AND missing=0",
        )?;
        let rows =
            st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))?;
        for (bytes, vc) in rows.flatten() {
            let s = video_saving(vc.as_deref(), bytes);
            if s > 4_000_000 {
                vid_n += 1;
                vid_saving += s;
            }
        }
    }

    // Duplicates, as far as we can tell without a full hash pass.
    let (dup_groups, dup_saving): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(extra),0) FROM (
               SELECT COUNT(*)-1 AS n, SUM(bytes)-MAX(bytes) AS extra
               FROM media WHERE deleted IS NULL AND missing=0 AND dhash IS NOT NULL AND dhash<>0
               GROUP BY dhash HAVING COUNT(*)>1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));

    let total_photos = n_photo + n_raw + n_lay;
    let avg_mb = if total_photos > 0 {
        (b_photo + b_raw + b_lay) as f64 / total_photos as f64 / 1_048_576.0
    } else {
        0.0
    };

    let free = free_space_of_libraries(&conn);

    Ok(json!({
      "breakdown": [
        {"label":"Photos",          "bytes": b_photo, "count": n_photo, "color":"#4B79E4"},
        {"label":"Video",           "bytes": b_video, "count": n_video, "color":"#5AA9E6"},
        {"label":"RAW",             "bytes": b_raw,   "count": n_raw,   "color":"#B48CE0"},
        {"label":"Layered (PSD)",   "bytes": b_lay,   "count": n_lay,   "color":"#E8A33D"},
        {"label":"Thumbnail cache", "bytes": cache,   "count": 0,       "color":"#64C08A"},
        {"label":"Models",          "bytes": models,  "count": 0,       "color":"#8A93A6"},
        {"label":"Bin",             "bytes": b_bin,   "count": n_bin,   "color":"#F2564D"}
      ],
      "free": free,
      "cache": cache,
      "avg_photo_mb": avg_mb,
      "oversized": avg_mb > 4.5 && big_n > 20,
      "reclaim": [
        {"id":"duplicates", "label":"Duplicates",      "bytes": dup_saving, "count": dup_groups, "detail": format!("{dup_groups} groups of visually identical files"), "color":"#B48CE0"},
        {"id":"oversized",  "label":"Oversized photos","bytes": big_saving, "count": big_n,      "detail": format!("{big_n} files well above {:.1} MB for their resolution", 2.5), "color":"#E8A33D"},
        {"id":"video",      "label":"Video re-encode", "bytes": vid_saving, "count": vid_n,      "detail": format!("{vid_n} clips in an inefficient codec"), "color":"#5AA9E6"},
        {"id":"bin",        "label":"Bin",             "bytes": b_bin,      "count": n_bin,      "detail": format!("{n_bin} files awaiting purge"), "color":"#F2564D"}
      ]
    }))
}

/// Free bytes on the volume holding the first library. Uses the Win32 call
/// directly rather than dragging in a crate for one function.
fn free_space_of_libraries(conn: &rusqlite::Connection) -> i64 {
    let root: Option<String> =
        conn.query_row("SELECT path FROM libraries ORDER BY id LIMIT 1", [], |r| r.get(0)).ok();
    let Some(root) = root else { return 0 };
    free_space(Path::new(&root))
}

#[cfg(windows)]
pub fn free_space(p: &Path) -> i64 {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut avail = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok != 0 {
        avail as i64
    } else {
        0
    }
}

#[cfg(not(windows))]
pub fn free_space(_p: &Path) -> i64 {
    0
}

// ----------------------------------------------------------- duplicates -----

#[derive(Serialize)]
pub struct DupItem {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub bytes: i64,
    pub w: Option<i64>,
    pub h: Option<i64>,
    pub mtime: i64,
    pub keep: bool,
}

#[derive(Serialize)]
pub struct DupGroup {
    pub key: String,
    pub exact: bool,
    pub freed: i64,
    pub items: Vec<DupItem>,
}

/// Exact matches come from a content hash; near matches from the perceptual
/// hash, which is what catches the same photo saved twice at different sizes.
/// Within a group the largest file wins, breaking ties on the newer one.
pub fn duplicates(app: &Arc<App>, exact_only: bool) -> Result<Vec<DupGroup>> {
    let conn = app.index.get()?;
    let col = if exact_only { "hash" } else { "dhash" };
    let sql = format!(
        "SELECT {col}, id, name, path, bytes, w, h, mtime FROM media
         WHERE deleted IS NULL AND missing=0 AND {col} IS NOT NULL AND {col} <> 0
           AND {col} IN (SELECT {col} FROM media
                         WHERE deleted IS NULL AND missing=0 AND {col} IS NOT NULL AND {col} <> 0
                         GROUP BY {col} HAVING COUNT(*) > 1)
         ORDER BY {col}, bytes DESC, mtime DESC"
    );

    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, rusqlite::types::Value>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    let mut groups: Vec<DupGroup> = Vec::new();
    let mut current_key = String::new();

    for row in rows.flatten() {
        let key = match &row.0 {
            rusqlite::types::Value::Text(s) => s.clone(),
            rusqlite::types::Value::Integer(i) => i.to_string(),
            _ => continue,
        };
        if key != current_key {
            current_key = key.clone();
            groups.push(DupGroup { key, exact: exact_only, freed: 0, items: Vec::new() });
        }
        let g = groups.last_mut().unwrap();
        let keep = g.items.is_empty(); // rows arrive largest-first
        if !keep {
            g.freed += row.4;
        }
        g.items.push(DupItem {
            id: row.1,
            name: row.2,
            path: row.3,
            bytes: row.4,
            w: row.5,
            h: row.6,
            mtime: row.7,
            keep,
        });
    }

    groups.retain(|g| g.items.len() > 1);
    groups.sort_by_key(|g| std::cmp::Reverse(g.freed));
    Ok(groups)
}

// ---------------------------------------------------------- compression -----

#[derive(Deserialize, Debug, Clone)]
pub struct CompressReq {
    #[serde(default)]
    pub ids: Vec<i64>,
    /// "oversized" | "video" | "selection"
    #[serde(default)]
    pub scope: String,
    #[serde(default = "d_quality")]
    pub quality: u8,
    #[serde(default = "d_max_edge")]
    pub max_edge: u32,
    #[serde(default = "d_true")]
    pub keep_originals: bool,
    #[serde(default)]
    pub skip_rated: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "d_limit")]
    pub limit: usize,
}
fn d_quality() -> u8 {
    82
}
fn d_max_edge() -> u32 {
    4096
}
fn d_true() -> bool {
    true
}
fn d_limit() -> usize {
    usize::MAX
}

pub fn compress_targets(app: &Arc<App>, req: &CompressReq) -> Result<Vec<(i64, String, i64, String, i64)>> {
    let conn = app.index.get()?;
    let mut out = Vec::new();

    match req.scope.as_str() {
        "video" => {
            let mut st = conn.prepare(
                "SELECT id, path, kind, ext, bytes, vcodec, rating FROM media
                 WHERE kind=1 AND deleted IS NULL AND missing=0 ORDER BY bytes DESC",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })?;
            for r in rows.flatten() {
                if req.skip_rated && r.6 >= 4 {
                    continue;
                }
                if video_saving(r.5.as_deref(), r.4) > 4_000_000 {
                    out.push((r.0, r.1, r.2, r.3, r.4));
                }
            }
        }
        "selection" => {
            for id in &req.ids {
                if let Ok(r) = conn.query_row(
                    "SELECT id, path, kind, ext, bytes FROM media WHERE id=?1 AND deleted IS NULL",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                ) {
                    out.push(r);
                }
            }
        }
        _ => {
            let mut st = conn.prepare(
                "SELECT id, path, kind, ext, bytes, w, h, rating FROM media
                 WHERE kind IN (0) AND deleted IS NULL AND missing=0 AND bytes > 2500000
                 ORDER BY bytes DESC",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for r in rows.flatten() {
                if req.skip_rated && r.7 >= 4 {
                    continue;
                }
                if oversized(r.4, r.5, r.6) {
                    out.push((r.0, r.1, r.2, r.3, r.4));
                }
            }
        }
    }
    out.truncate(req.limit);
    Ok(out)
}

/// Recompress in the background. Every original is binned first when
/// `keep_originals` is set, so the whole run is undoable until the bin is
/// emptied.
pub fn spawn_compress(app: Arc<App>, req: CompressReq) {
    std::thread::spawn(move || {
        let targets = match compress_targets(&app, &req) {
            Ok(t) => t,
            Err(e) => {
                task_set(&app, |t| {
                    t.running = false;
                    t.message = format!("could not build work list: {e}");
                });
                return;
            }
        };

        task_set(&app, |t| {
            *t = TaskInfo {
                kind: "compress".into(),
                label: format!("Recompressing {} files", targets.len()),
                total: targets.len() as u64,
                running: true,
                ..Default::default()
            }
        });

        let mut freed = 0i64;
        let mut failed = 0u64;
        for (i, (id, path, kind, ext, bytes)) in targets.iter().enumerate() {
            let r = if *kind == KIND_VIDEO {
                compress_video(&app, *id, path, &req)
            } else {
                compress_photo(&app, *id, path, ext, &req)
            };
            match r {
                Ok(new_bytes) => freed += (bytes - new_bytes).max(0),
                Err(e) => {
                    failed += 1;
                    eprintln!("[compress] {path}: {e:#}");
                }
            }
            task_set(&app, |t| {
                t.done = i as u64 + 1;
                t.freed = freed;
                t.failed = failed;
                t.label = Path::new(path)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
            });
        }

        task_set(&app, |t| {
            t.running = false;
            t.message = format!(
                "Recompressed {} files, reclaimed {}{}",
                targets.len() - failed as usize,
                human_bytes(freed),
                if failed > 0 { format!(", {failed} failed") } else { String::new() }
            );
        });

        // Tiles must be rebuilt from the new files.
        scan::spawn(app, None, false);
    });
}

fn replace_original(app: &App, id: i64, old: &Path, new_data: &[u8], new_ext: &str, keep: bool) -> Result<i64> {
    let stem = old.file_stem().context("no stem")?.to_string_lossy().to_string();
    let dir = old.parent().context("no parent")?;
    let same_ext = old
        .extension()
        .map(|e| e.to_string_lossy().eq_ignore_ascii_case(new_ext))
        .unwrap_or(false);

    // Write beside the original first; a half-written file must never be able
    // to take the original's place.
    let tmp = dir.join(format!(".{stem}.speckle-tmp"));
    std::fs::write(&tmp, new_data)?;

    if keep {
        bin(app, &[id])?;
    } else {
        std::fs::remove_file(old).ok();
    }

    let final_path = if same_ext { old.to_path_buf() } else { dir.join(format!("{stem}.{new_ext}")) };
    let final_path = if final_path.exists() && !same_ext {
        scan::unique_dest(dir, &format!("{stem}.{new_ext}"))
    } else {
        final_path
    };
    std::fs::rename(&tmp, &final_path)?;

    let conn = app.index.get()?;
    let p = final_path.to_string_lossy().replace('\\', "/");
    let name = final_path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let rel_row: Result<(i64, String), _> =
        conn.query_row("SELECT lib, rel FROM media WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)));

    if keep {
        // The original kept row `id` and is now in the bin, so the recompressed
        // file is a new row that the next scan will pick up. Insert it now so
        // the grid updates immediately.
        if let Ok((lib, _)) = rel_row {
            let root = lib_root(&conn, lib)?;
            let rel = final_path
                .strip_prefix(&root)
                .unwrap_or(&final_path)
                .to_string_lossy()
                .replace('\\', "/");
            let sub = rel.rfind('/').map(|i| rel[..i].to_string()).unwrap_or_default();
            conn.execute(
                "INSERT OR IGNORE INTO media (lib,path,rel,sub,name,ext,kind,bytes,mtime,state,added_at,missing)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,0)",
                params![lib, p, rel, sub, name, new_ext, KIND_PHOTO, new_data.len() as i64, db::now(), db::now()],
            )?;
        }
    } else {
        conn.execute(
            "UPDATE media SET path=?2, name=?3, ext=?4, bytes=?5, mtime=?6, state=0, err=NULL WHERE id=?1",
            params![id, p, name, new_ext, new_data.len() as i64, db::now()],
        )?;
        app.thumbs.get()?.execute("DELETE FROM t WHERE id=?1", [id]).ok();
    }
    Ok(new_data.len() as i64)
}

fn compress_photo(app: &App, id: i64, path: &str, ext: &str, req: &CompressReq) -> Result<i64> {
    let p = Path::new(path);
    let img = decode::load(p, KIND_PHOTO, ext, req.max_edge)?;
    let img = if req.max_edge > 0 { decode::fit_thumb(&img, req.max_edge) } else { img };
    let data = decode::encode_jpeg(&img, req.quality)?;

    let original_len = std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(i64::MAX);
    // Refuse to "save" space by making a file bigger.
    if data.len() as i64 >= original_len {
        bail!("recompression produced a larger file; left alone");
    }
    if req.dry_run {
        return Ok(data.len() as i64);
    }
    replace_original(app, id, p, &data, "jpg", req.keep_originals)
}

fn compress_video(app: &App, id: i64, path: &str, req: &CompressReq) -> Result<i64> {
    let p = Path::new(path);
    let dir = p.parent().context("no parent")?;
    let stem = p.file_stem().context("no stem")?.to_string_lossy().to_string();
    let tmp = dir.join(format!(".{stem}.speckle-tmp.mp4"));

    let crf = match req.quality {
        q if q >= 92 => "20",
        q if q >= 84 => "24",
        q if q >= 74 => "27",
        _ => "30",
    };

    let mut cmd = std::process::Command::new("ffmpeg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let status = cmd
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(p)
        .args(["-map", "0:v:0", "-map", "0:a:0?"])
        .args(["-c:v", "libx265", "-preset", "medium", "-crf", crf, "-tag:v", "hvc1"])
        .args(["-pix_fmt", "yuv420p"])
        .arg("-vf")
        .arg(format!("scale='trunc(min(1,{m}/max(iw,ih))*iw/2)*2':'trunc(min(1,{m}/max(iw,ih))*ih/2)*2'", m = req.max_edge.max(720)))
        .args(["-c:a", "aac", "-b:a", "128k"])
        .args(["-movflags", "+faststart"])
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .status()?;

    if !status.success() {
        std::fs::remove_file(&tmp).ok();
        bail!("ffmpeg exited with {status}");
    }
    let new_len = std::fs::metadata(&tmp)?.len() as i64;
    let old_len = std::fs::metadata(p)?.len() as i64;
    if new_len >= old_len {
        std::fs::remove_file(&tmp).ok();
        bail!("re-encode produced a larger file; left alone");
    }
    if req.dry_run {
        std::fs::remove_file(&tmp).ok();
        return Ok(new_len);
    }

    let data = std::fs::read(&tmp)?;
    std::fs::remove_file(&tmp).ok();
    replace_original(app, id, p, &data, "mp4", req.keep_originals)
}

/// A cheap sample so the UI can show a real before/after instead of a guess.
pub fn compress_preview(app: &Arc<App>, req: &CompressReq) -> Result<serde_json::Value> {
    let targets = compress_targets(app, req)?;
    let sample: Vec<_> = targets.iter().take(12).collect();
    let mut before = 0i64;
    let mut after = 0i64;
    let mut example = None;

    for (id, path, kind, ext, bytes) in &sample {
        if *kind == KIND_VIDEO {
            before += bytes;
            after += bytes - video_saving(None, *bytes);
            continue;
        }
        let Ok(img) = decode::load(Path::new(path), *kind, ext, req.max_edge) else { continue };
        let img = decode::fit_thumb(&img, req.max_edge);
        let Ok(data) = decode::encode_jpeg(&img, req.quality) else { continue };
        before += bytes;
        after += data.len() as i64;
        if example.is_none() {
            example = Some(json!({"id": id, "before": bytes, "after": data.len()}));
        }
    }

    let ratio = if before > 0 { after as f64 / before as f64 } else { 1.0 };
    let total_before: i64 = targets.iter().map(|t| t.4).sum();

    Ok(json!({
      "count": targets.len(),
      "total_before": total_before,
      "estimated_after": (total_before as f64 * ratio) as i64,
      "estimated_saving": (total_before as f64 * (1.0 - ratio)) as i64,
      "ratio": ratio,
      "sampled": sample.len(),
      "example": example
    }))
}

// -------------------------------------------------------------- editing -----

/// Save an edited image. The browser does the pixel work — the same WebGL
/// pipeline that drew the live preview produces the final JPEG — so what you
/// saw is exactly what lands on disk.
pub fn save_edit(app: &App, id: i64, data: &[u8], overwrite: bool) -> Result<i64> {
    let conn = app.index.get()?;
    let (lib, path, _name): (i64, String, String) =
        conn.query_row("SELECT lib, path, name FROM media WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let p = PathBuf::from(&path);
    let dir = p.parent().context("no parent")?.to_path_buf();
    let stem = p.file_stem().context("no stem")?.to_string_lossy().to_string();

    if overwrite {
        drop(conn);
        replace_original(app, id, &p, data, "jpg", true)?;
        return Ok(id);
    }

    let dest = scan::unique_dest(&dir, &format!("{stem}-edited.jpg"));
    std::fs::write(&dest, data)?;

    let root = lib_root(&conn, lib)?;
    let rel = dest.strip_prefix(&root).unwrap_or(&dest).to_string_lossy().replace('\\', "/");
    let sub = rel.rfind('/').map(|i| rel[..i].to_string()).unwrap_or_default();
    conn.execute(
        "INSERT OR IGNORE INTO media (lib,path,rel,sub,name,ext,kind,bytes,mtime,state,added_at,missing)
         VALUES (?1,?2,?3,?4,?5,'jpg',?6,?7,?8,0,?9,0)",
        params![
            lib,
            dest.to_string_lossy().replace('\\', "/"),
            rel,
            sub,
            dest.file_name().unwrap_or_default().to_string_lossy().to_string(),
            KIND_PHOTO,
            data.len() as i64,
            db::now(),
            db::now()
        ],
    )?;
    let new_id: i64 = conn.query_row(
        "SELECT id FROM media WHERE path=?1",
        [dest.to_string_lossy().replace('\\', "/")],
        |r| r.get(0),
    )?;
    Ok(new_id)
}

/// Directory listing for the in-app folder picker. Works identically from the
/// desktop window and from a phone, which a native file dialog could not.
pub fn browse(path: Option<&str>) -> Result<serde_json::Value> {
    let mut entries = Vec::new();

    let Some(path) = path.filter(|p| !p.is_empty()) else {
        // Top level: the drives that actually exist.
        #[cfg(windows)]
        for letter in b'A'..=b'Z' {
            let p = format!("{}:/", letter as char);
            if Path::new(&p).exists() {
                entries.push(json!({"name": format!("{}:", letter as char), "path": p, "dir": true}));
            }
        }
        return Ok(json!({"path": "", "parent": null, "entries": entries}));
    };

    let dir = Path::new(path);
    let mut names: Vec<(String, String)> = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {path}"))?.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('$') || name == scan::BIN_DIRNAME {
            continue;
        }
        names.push((name, e.path().to_string_lossy().replace('\\', "/")));
    }
    names.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    for (name, p) in names {
        entries.push(json!({"name": name, "path": p, "dir": true}));
    }

    let parent = dir.parent().map(|p| p.to_string_lossy().replace('\\', "/"));
    Ok(json!({"path": path.replace('\\', "/"), "parent": parent, "entries": entries}))
}


// ------------------------------------------------------------- ffmpeg ------

/// Current Windows build. FFmpeg only learned to read HEIC in 7.0, and the
/// build most people already have on PATH predates that — which quietly makes
/// every iPhone photo in a library unreadable.
const FFMPEG_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";

pub fn tools_dir(app: &App) -> PathBuf {
    app.data_dir.join("tools")
}

/// Fetch a current ffmpeg into Speckle's own directory and switch to it. Does
/// not touch the system, PATH, or whatever ffmpeg the user already has.
pub fn spawn_install_ffmpeg(app: Arc<App>) {
    std::thread::spawn(move || {
        // Downloading is network-bound, so it runs alongside whatever else is
        // happening and reports on the secondary progress channel.
        task_set(&app, |t| {
            *t = TaskInfo {
                kind: "download".into(),
                label: "ffmpeg".into(),
                running: true,
                ..Default::default()
            }
        });
        let res = (|| -> Result<()> {
            let dir = tools_dir(&app);
            std::fs::create_dir_all(&dir)?;
            let zip_path = dir.join("ffmpeg.zip");
            crate::ml::fetch_with(&app, FFMPEG_URL, &zip_path, 112_000_000, "ffmpeg", true)?;
            task_set(&app, |t| t.label = "Unpacking ffmpeg…".into());
            crate::ml::extract_needed(&zip_path, &dir, &["ffmpeg.exe", "ffprobe.exe"])?;
            let _ = std::fs::remove_file(&zip_path);
            crate::decode::set_tools_dir(dir);
            Ok(())
        })();

        task_set(&app, |t| {
            t.running = false;
            t.message = match &res {
                Err(e) => format!("ffmpeg download failed: {e}"),
                Ok(_) => "Installed a current ffmpeg".into(),
            };
        });
        match res {
            Ok(()) => {
                println!("[speckle] installed a current ffmpeg; retrying unreadable files");
                crate::scan::spawn_ex(app, None, false, true);
            }
            Err(e) => eprintln!("[speckle] ffmpeg install failed: {e:#}"),
        }
    });
}
