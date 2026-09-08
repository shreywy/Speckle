//! Collections, export, and where uploads land.
//!
//! A collection is a curated set — like a playlist. Membership is a database
//! row, never a file move, so a photo can sit in five collections at once and
//! the folders on disk stay exactly as the user arranged them. Exporting is the
//! only operation that writes anything, and it writes somewhere new.

use anyhow::{bail, Context, Result};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::db;
use crate::App;

// -------------------------------------------------------------- collections --

pub fn list(app: &Arc<App>) -> Result<Value> {
    let c = app.index.get()?;
    let mut st = c.prepare(
        "SELECT c.id, c.name, c.note, c.created,
                (SELECT COUNT(*) FROM collection_items i WHERE i.coll = c.id),
                COALESCE(c.cover, (SELECT i.media FROM collection_items i
                                   WHERE i.coll = c.id ORDER BY i.ord, i.added LIMIT 1)),
                (SELECT COALESCE(SUM(m.bytes),0) FROM collection_items i
                 JOIN media m ON m.id = i.media WHERE i.coll = c.id)
         FROM collections c ORDER BY c.created DESC",
    )?;
    let rows: Vec<Value> = st
        .query_map([], |r| {
            Ok(json!({
              "id": r.get::<_, i64>(0)?,
              "name": r.get::<_, String>(1)?,
              "note": r.get::<_, String>(2)?,
              "created": r.get::<_, i64>(3)?,
              "count": r.get::<_, i64>(4)?,
              "cover": r.get::<_, Option<i64>>(5)?,
              "bytes": r.get::<_, i64>(6)?
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(json!({ "collections": rows }))
}

pub fn create(app: &Arc<App>, name: &str) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a collection needs a name");
    }
    let c = app.index.get()?;
    c.execute(
        "INSERT INTO collections(name, created) VALUES(?1, ?2)
         ON CONFLICT(name) DO NOTHING",
        params![name, db::now()],
    )?;
    let id: i64 = c.query_row("SELECT id FROM collections WHERE name=?1", [name], |r| r.get(0))?;
    Ok(json!({ "id": id, "name": name }))
}

pub fn rename(app: &Arc<App>, id: i64, name: &str, note: Option<&str>) -> Result<()> {
    let c = app.index.get()?;
    if !name.trim().is_empty() {
        c.execute("UPDATE collections SET name=?2 WHERE id=?1", params![id, name.trim()])?;
    }
    if let Some(n) = note {
        c.execute("UPDATE collections SET note=?2 WHERE id=?1", params![id, n])?;
    }
    Ok(())
}

pub fn remove(app: &Arc<App>, id: i64) -> Result<()> {
    let c = app.index.get()?;
    c.execute("DELETE FROM collection_items WHERE coll=?1", [id])?;
    c.execute("DELETE FROM collections WHERE id=?1", [id])?;
    Ok(())
}

#[derive(Deserialize)]
pub struct ItemsReq {
    pub ids: Vec<i64>,
    #[serde(default)]
    pub remove: bool,
}

pub fn set_items(app: &Arc<App>, coll: i64, req: &ItemsReq) -> Result<usize> {
    let c = app.index.get()?;
    let tx = c.unchecked_transaction()?;
    let n = if req.remove {
        let mut st = tx.prepare("DELETE FROM collection_items WHERE coll=?1 AND media=?2")?;
        req.ids.iter().map(|m| st.execute(params![coll, m]).unwrap_or(0)).sum()
    } else {
        let next: i64 = tx
            .query_row("SELECT COALESCE(MAX(ord),0) FROM collection_items WHERE coll=?1", [coll], |r| r.get(0))
            .unwrap_or(0);
        let mut st = tx.prepare(
            "INSERT INTO collection_items(coll, media, ord, added) VALUES(?1,?2,?3,?4)
             ON CONFLICT(coll, media) DO NOTHING",
        )?;
        req.ids
            .iter()
            .enumerate()
            .map(|(i, m)| st.execute(params![coll, m, next + i as i64 + 1, db::now()]).unwrap_or(0))
            .sum()
    };
    tx.commit()?;
    Ok(n)
}

/// Which collections a given photo belongs to — shown in the info panel so the
/// answer to "is this already saved somewhere?" is visible.
pub fn memberships(app: &Arc<App>, media: i64) -> Result<Vec<Value>> {
    let c = app.index.get()?;
    let mut st = c.prepare(
        "SELECT c.id, c.name FROM collections c
         JOIN collection_items i ON i.coll = c.id WHERE i.media = ?1 ORDER BY c.name",
    )?;
    Ok(st
        .query_map([media], |r| Ok(json!({"id": r.get::<_,i64>(0)?, "name": r.get::<_,String>(1)?})))?
        .collect::<Result<_, _>>()?)
}

// ------------------------------------------------------------------ export --

/// Zip a collection to a temporary file and return its path. Built on disk
/// rather than in memory because a holiday album is measured in gigabytes.
pub fn export_zip(app: &Arc<App>, coll: i64, originals: bool) -> Result<(PathBuf, String)> {
    let (name, items): (String, Vec<(i64, String, String)>) = {
        let c = app.index.get()?;
        let name: String = c.query_row("SELECT name FROM collections WHERE id=?1", [coll], |r| r.get(0))?;
        let mut st = c.prepare(
            "SELECT m.id, m.path, m.name FROM collection_items i
             JOIN media m ON m.id = i.media
             WHERE i.coll = ?1 AND m.deleted IS NULL AND m.missing = 0
             ORDER BY i.ord, i.added",
        )?;
        let items = st
            .query_map([coll], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        (name, items)
    };
    if items.is_empty() {
        bail!("that collection is empty");
    }

    let safe: String = name
        .chars()
        .map(|ch| if ch.is_alphanumeric() || ch == ' ' || ch == '-' || ch == '_' { ch } else { '_' })
        .collect();
    let out = app.data_dir.join(format!("export-{coll}-{}.zip", db::now()));
    let file = std::fs::File::create(&out)?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    // Photos are already compressed; deflating them again costs minutes and
    // saves almost nothing, so the archive just stores them.
    let opts: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (id, path, fname) in &items {
        let mut entry = fname.clone();
        if !used.insert(entry.clone()) {
            // Two folders can hold the same filename; keep both.
            let stem = Path::new(fname).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let ext = Path::new(fname).extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
            entry = format!("{stem} ({id}){ext}");
            used.insert(entry.clone());
        }
        zip.start_file(&entry, opts)?;
        if originals {
            let mut f = std::fs::File::open(path).with_context(|| format!("reading {path}"))?;
            std::io::copy(&mut f, &mut zip)?;
        } else {
            let data = crate::scan::ensure_preview(app, *id)?;
            zip.write_all(&data)?;
        }
    }
    zip.finish()?;
    Ok((out, format!("{safe}.zip")))
}

// ------------------------------------------------------------- upload dest --

/// Where uploads land. Defaults to a dedicated folder so a phone dumping photos
/// never mixes them into a curated library by accident.
pub fn inbox_root(c: &rusqlite::Connection) -> String {
    db::get_setting(c, "inbox")
        .map(|s| s.trim_matches('"').replace('\\', "/"))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "E:/Photos/Speckle".to_string())
}

pub fn inbox_organize(c: &rusqlite::Connection) -> bool {
    db::get_setting(c, "inbox_organize")
        .map(|s| s.trim_matches('"') != "false")
        .unwrap_or(true)
}

/// Sort an upload into `YYYY-MM` under the inbox, using the date the photo was
/// actually taken where the file carries one. Falling back to "now" would file
/// a 2019 holiday under this month, which is exactly the kind of quiet wrongness
/// that makes an archive untrustworthy.
pub fn dated_dir(root: &str, taken: Option<i64>, organize: bool) -> PathBuf {
    let base = PathBuf::from(root);
    if !organize {
        return base;
    }
    let ts = taken.unwrap_or_else(db::now);
    let dt = chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);
    use chrono::Datelike;
    base.join(format!("{:04}-{:02}", dt.year(), dt.month()))
}

/// Pull a capture date out of bytes we have in memory, before they are written,
/// so the file goes straight to the right folder instead of being moved later.
pub fn taken_from_bytes(data: &[u8], filename: &str) -> Option<i64> {
    let mut cur = std::io::Cursor::new(data);
    if let Ok(exif) = exif::Reader::new().continue_on_error(true).read_from_container(&mut cur) {
        if let Some(t) = crate::decode::exif_taken(&exif) {
            return Some(t);
        }
    } else {
        let mut cur = std::io::Cursor::new(data);
        if let Err(exif::Error::PartialResult(p)) =
            exif::Reader::new().continue_on_error(true).read_from_container(&mut cur)
        {
            if let Some(t) = crate::decode::exif_taken(&p.into_inner().0) {
                return Some(t);
            }
        }
    }
    crate::decode::date_from_name(filename)
}

// ----------------------------------------------------------------- reveal ---

/// Open the host's file manager with the file selected. Only meaningful when
/// the app is being used at the machine itself; the UI hides it otherwise.
pub fn reveal(path: &str) -> Result<()> {
    let native = path.replace('/', "\\");
    if !Path::new(path).exists() {
        bail!("that file is no longer at {path}");
    }
    #[cfg(windows)]
    {
        // `explorer` returns a non-zero exit code even on success, so the status
        // is deliberately not checked.
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{native}"))
            .spawn()?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(Path::new(path).parent().unwrap_or(Path::new(".")))
            .spawn()?;
        Ok(())
    }
}
