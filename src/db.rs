//! SQLite storage. Two databases: `index.db` holds metadata, `thumbs.db` holds
//! thumbnail blobs. They are separate so that rebuilding the (large) thumbnail
//! store never risks the (small, precious) metadata — ratings, favourites and
//! bin state live in index.db and survive a cache wipe.

use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Media class. Drives which decoder the thumbnailer reaches for.
pub const KIND_PHOTO: i64 = 0;
pub const KIND_VIDEO: i64 = 1;
pub const KIND_RAW: i64 = 2;
pub const KIND_LAYERED: i64 = 3;

/// Not yet thumbnailed / thumbnailed / gave up.
pub const ST_NEW: i64 = 0;
pub const ST_DONE: i64 = 1;
pub const ST_ERR: i64 = 2;

/// A tiny connection pool. SQLite connections are cheap but not free, and
/// rusqlite's `Connection` is `!Sync`, so handlers borrow one for the duration
/// of a blocking call and hand it back on drop.
pub struct Pool {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
    init: &'static str,
}

pub struct Handle<'a> {
    pool: &'a Pool,
    conn: Option<Connection>,
}

impl std::ops::Deref for Handle<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl Drop for Handle<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            let mut idle = self.pool.idle.lock();
            if idle.len() < 8 {
                idle.push(c);
            }
        }
    }
}

impl Pool {
    pub fn new(path: PathBuf, init: &'static str) -> Result<Self> {
        let p = Pool { path, idle: Mutex::new(Vec::new()), init };
        // Create the file and apply the schema once up front.
        let c = p.open()?;
        c.execute_batch(init)?;
        p.idle.lock().push(c);
        Ok(p)
    }

    fn open(&self) -> Result<Connection> {
        let c = Connection::open(&self.path)?;
        // Migrations for databases created by an earlier version. SQLite has no
        // "ADD COLUMN IF NOT EXISTS", so a failure here just means it is present.
        let _ = c.execute("ALTER TABLE media ADD COLUMN facedone INTEGER NOT NULL DEFAULT 0", []);
        let _ = c.execute("ALTER TABLE people ADD COLUMN centroid BLOB", []);
        let _ = c.execute("ALTER TABLE libraries ADD COLUMN excludes TEXT NOT NULL DEFAULT ''", []);
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA temp_store=MEMORY;
             PRAGMA cache_size=-32000;
             PRAGMA busy_timeout=15000;",
        )?;
        Ok(c)
    }

    pub fn get(&self) -> Result<Handle<'_>> {
        let existing = self.idle.lock().pop();
        let conn = match existing {
            Some(c) => c,
            None => self.open()?,
        };
        Ok(Handle { pool: self, conn: Some(conn) })
    }

    /// A standalone connection for a long-running background job, so the job
    /// never holds a pooled connection hostage while requests are waiting.
    pub fn standalone(&self) -> Result<Connection> {
        let c = self.open()?;
        c.execute_batch(self.init)?;
        Ok(c)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub const INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS libraries (
  id       INTEGER PRIMARY KEY,
  path     TEXT NOT NULL UNIQUE,
  name     TEXT NOT NULL,
  color    TEXT NOT NULL DEFAULT '#4B79E4',
  added_at INTEGER NOT NULL,
  -- Sub-folders to skip, newline separated and relative to the library root.
  -- Kept as text rather than a table because it is read on every walk and
  -- edited as a whole.
  excludes TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS media (
  id       INTEGER PRIMARY KEY,
  lib      INTEGER NOT NULL,
  path     TEXT NOT NULL UNIQUE,
  rel      TEXT NOT NULL,
  sub      TEXT NOT NULL DEFAULT '',
  name     TEXT NOT NULL,
  ext      TEXT NOT NULL,
  kind     INTEGER NOT NULL,
  bytes    INTEGER NOT NULL,
  mtime    INTEGER NOT NULL,
  taken    INTEGER,
  w        INTEGER,
  h        INTEGER,
  orient   INTEGER NOT NULL DEFAULT 1,
  camera   TEXT,
  lens     TEXT,
  iso      INTEGER,
  fnum     REAL,
  expo     TEXT,
  focal    REAL,
  dur      REAL,
  vcodec   TEXT,
  acodec   TEXT,
  rating   INTEGER NOT NULL DEFAULT 0,
  fav      INTEGER NOT NULL DEFAULT 0,
  deleted  INTEGER,
  binpath  TEXT,
  hash     TEXT,
  dhash    INTEGER,
  state    INTEGER NOT NULL DEFAULT 0,
  err      TEXT,
  added_at INTEGER NOT NULL,
  missing  INTEGER NOT NULL DEFAULT 0,
  -- Set once a photo has been through face detection. Without this, every
  -- photo containing no faces looks unprocessed forever and is re-scanned on
  -- each restart.
  facedone INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS ix_media_lib     ON media(lib);
CREATE INDEX IF NOT EXISTS ix_media_state   ON media(state);
CREATE INDEX IF NOT EXISTS ix_media_deleted ON media(deleted);
CREATE INDEX IF NOT EXISTS ix_media_taken   ON media(taken DESC);
CREATE INDEX IF NOT EXISTS ix_media_kind    ON media(kind);
CREATE INDEX IF NOT EXISTS ix_media_hash    ON media(hash);
CREATE INDEX IF NOT EXISTS ix_media_dhash   ON media(dhash);
CREATE INDEX IF NOT EXISTS ix_media_bytes   ON media(bytes DESC);
CREATE INDEX IF NOT EXISTS ix_media_sub     ON media(lib, sub);

CREATE TABLE IF NOT EXISTS settings (
  k TEXT PRIMARY KEY,
  v TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tags (
  id   INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS media_tags (
  media_id INTEGER NOT NULL,
  tag_id   INTEGER NOT NULL,
  score    REAL NOT NULL DEFAULT 1.0,
  PRIMARY KEY (media_id, tag_id)
);
CREATE INDEX IF NOT EXISTS ix_mt_tag ON media_tags(tag_id);

-- Curated sets, like a playlist. Membership is explicit and ordered; nothing
-- here touches the filesystem until the set is exported.
CREATE TABLE IF NOT EXISTS collections (
  id      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL UNIQUE,
  note    TEXT NOT NULL DEFAULT '',
  created INTEGER NOT NULL,
  cover   INTEGER
);
CREATE TABLE IF NOT EXISTS collection_items (
  coll  INTEGER NOT NULL,
  media INTEGER NOT NULL,
  ord   INTEGER NOT NULL DEFAULT 0,
  added INTEGER NOT NULL,
  PRIMARY KEY (coll, media)
);
CREATE INDEX IF NOT EXISTS ix_ci_media ON collection_items(media);

-- Faces found in photos. `cluster` groups them into a person; `person` is the
-- name the user gave that cluster, if any.
CREATE TABLE IF NOT EXISTS faces (
  id      INTEGER PRIMARY KEY,
  media   INTEGER NOT NULL,
  x       REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
  score   REAL NOT NULL,
  cluster INTEGER,
  vec     BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_faces_media   ON faces(media);
CREATE INDEX IF NOT EXISTS ix_faces_cluster ON faces(cluster);

CREATE TABLE IF NOT EXISTS people (
  id      INTEGER PRIMARY KEY,
  name    TEXT NOT NULL DEFAULT '',
  cover   INTEGER,
  hidden  INTEGER NOT NULL DEFAULT 0,
  -- Mean of this person's face embeddings, normalised. Kept so that new faces
  -- can be matched against existing people without reloading every vector,
  -- and so that grouping never has to start from scratch and throw away names
  -- or manual merges.
  centroid BLOB
);

-- CLIP image embedding, one row per photo, stored as raw little-endian f32.
CREATE TABLE IF NOT EXISTS clip (
  media INTEGER PRIMARY KEY,
  vec   BLOB NOT NULL
);
"#;

pub const THUMB_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS t (
  id   INTEGER NOT NULL,
  tier INTEGER NOT NULL,
  data BLOB NOT NULL,
  PRIMARY KEY (id, tier)
) WITHOUT ROWID;
"#;

/// Thumbnail tiers. Grid tier is a square centre-crop so every cache entry is
/// the same shape and the grid can be laid out by arithmetic; preview tier
/// keeps the original aspect and is what the viewer shows while the full-size
/// original is still being decoded.
pub const TIER_GRID: i64 = 0;
pub const TIER_PREVIEW: i64 = 1;

pub fn get_setting(c: &Connection, k: &str) -> Option<String> {
    c.query_row("SELECT v FROM settings WHERE k=?1", [k], |r| r.get(0)).ok()
}

pub fn set_setting(c: &Connection, k: &str, v: &str) -> Result<()> {
    c.execute(
        "INSERT INTO settings(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        rusqlite::params![k, v],
    )?;
    Ok(())
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
