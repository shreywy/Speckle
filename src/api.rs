//! The HTTP surface. This is the whole application interface — the desktop
//! window is just a WebView pointed at it, so there is exactly one code path
//! whether you are at the machine or on a phone across the tailnet.

use anyhow::Result;
use axum::extract::{Multipart, Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::db::{self, TIER_GRID};
use crate::decode;
use crate::ml;
use crate::scan;
use crate::sets;
use crate::stream;
use crate::tools;
use crate::App;

#[derive(rust_embed::Embed)]
#[folder = "assets/"]
struct Assets;

type S = State<Arc<App>>;

/// Anything blocking (SQLite, image decode, filesystem) has to leave the async
/// worker threads alone.
async fn blocking<T, F>(f: F) -> Result<T, Response>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}"))),
        Err(e) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task panicked: {e}"))),
    }
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

pub async fn serve(app: Arc<App>) -> Result<()> {
    use tower_http::compression::CompressionLayer;
    use tower_http::cors::{Any, CorsLayer};

    let r = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/state", get(state))
        .route("/api/settings", get(get_settings).post(post_settings))
        .route("/api/browse", get(browse))
        .route("/api/libraries", post(add_library))
        .route("/api/libraries/{id}", delete(remove_library))
        .route("/api/rescan", post(rescan))
        .route("/api/media", get(media_list))
        .route("/api/media/{id}", get(media_one))
        .route("/api/media/{id}/meta", post(media_meta))
        .route("/api/thumb/{id}", get(thumb))
        .route("/api/preview/{id}", get(preview))
        .route("/api/full/{id}", get(full))
        .route("/api/download/{id}", get(download))
        .route("/api/video/{id}", get(video))
        .route("/api/transcode/{id}", get(transcode))
        .route("/api/bin", post(do_bin))
        .route("/api/restore", post(do_restore))
        .route("/api/purge", post(do_purge))
        .route("/api/storage", get(storage))
        .route("/api/duplicates", get(duplicates))
        .route("/api/hash", post(start_hash))
        .route("/api/compress", post(start_compress))
        .route("/api/compress/preview", post(preview_compress))
        .route("/api/upload", post(upload))
        .route("/api/edit/{id}", post(save_edit))
        .route("/api/reveal", post(reveal))
        .route("/api/search", get(semantic_search))
        .route("/api/people", get(people_list))
        .route("/api/people/{id}", post(people_rename))
        .route("/api/people/merge", post(people_merge))
        .route("/api/face/{id}", get(face_thumb))
        .route("/api/ml/enable", post(ml_enable))
        .route("/api/ml/disable", post(ml_disable))
        .route("/api/ml/reindex", post(ml_reindex))
        .route("/api/ml/recluster", post(ml_recluster))
        .route("/api/collections", get(colls_list).post(colls_create))
        .route("/api/collections/{id}", post(colls_update).delete(colls_delete))
        .route("/api/collections/{id}/items", post(colls_items))
        .route("/api/collections/{id}/export", get(colls_export))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        // A 4K JPEG posted back from the editor is comfortably over the default.
        .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024))
        .with_state(app.clone());

    // Bound to every interface: the user chose open access on their tailnet.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", app.port)).await?;
    axum::serve(listener, r).await?;
    Ok(())
}

// ------------------------------------------------------------- static -------

async fn index() -> Response {
    match Assets::get("index.html") {
        Some(f) => Html(String::from_utf8_lossy(&f.data).to_string()).into_response(),
        None => err(StatusCode::NOT_FOUND, "index.html missing from the build"),
    }
}

async fn asset(AxPath(path): AxPath<String>) -> Response {
    match Assets::get(&path) {
        Some(f) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            let mut h = HeaderMap::new();
            h.insert(header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
            h.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            );
            (StatusCode::OK, h, f.data.to_vec()).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not found"),
    }
}

// -------------------------------------------------------------- state -------

async fn state(State(app): S) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        let mut st = c.prepare(
            "SELECT l.id, l.path, l.name, l.color,
                    (SELECT COUNT(*) FROM media m WHERE m.lib=l.id AND m.deleted IS NULL AND m.missing=0),
                    (SELECT COALESCE(SUM(bytes),0) FROM media m WHERE m.lib=l.id AND m.deleted IS NULL AND m.missing=0)
             FROM libraries l ORDER BY l.id",
        )?;
        let libs: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                  "id": r.get::<_,i64>(0)?, "path": r.get::<_,String>(1)?,
                  "name": r.get::<_,String>(2)?, "color": r.get::<_,String>(3)?,
                  "count": r.get::<_,i64>(4)?, "bytes": r.get::<_,i64>(5)?
                }))
            })?
            .collect::<Result<_, _>>()?;

        let counts = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        let total = counts("SELECT COUNT(*) FROM media WHERE deleted IS NULL AND missing=0");
        let bytes = counts("SELECT COALESCE(SUM(bytes),0) FROM media WHERE deleted IS NULL AND missing=0");
        let favs = counts("SELECT COUNT(*) FROM media WHERE fav=1 AND deleted IS NULL AND missing=0");
        let videos = counts("SELECT COUNT(*) FROM media WHERE kind=1 AND deleted IS NULL AND missing=0");
        let raws = counts("SELECT COUNT(*) FROM media WHERE kind IN (2,3) AND deleted IS NULL AND missing=0");
        let binned = counts("SELECT COUNT(*) FROM media WHERE deleted IS NOT NULL");
        let missing = counts("SELECT COUNT(*) FROM media WHERE missing=1");
        let errors = counts("SELECT COUNT(*) FROM media WHERE state=2");

        // Sub-folders inside each library, so the sidebar can show that a
        // library actually contains dump/ edits/ selection/.
        let mut fs = c.prepare(
            "SELECT lib, sub, COUNT(*) FROM media
             WHERE deleted IS NULL AND missing=0 AND sub <> ''
             GROUP BY lib, sub ORDER BY sub",
        )?;
        let folders: Vec<Value> = fs
            .query_map([], |r| {
                Ok(json!({"lib": r.get::<_,i64>(0)?, "sub": r.get::<_,String>(1)?, "count": r.get::<_,i64>(2)?}))
            })?
            .collect::<Result<_, _>>()?;

        let mut settings = serde_json::Map::new();
        let mut ss = c.prepare("SELECT k, v FROM settings")?;
        for row in ss.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (k, v) = row?;
            settings.insert(k, serde_json::from_str(&v).unwrap_or(Value::String(v)));
        }

        let clip_on = db::get_setting(&c, "clip_on").map(|v| v.trim_matches('"') == "true").unwrap_or(false);
        let faces_on = db::get_setting(&c, "faces_on").map(|v| v.trim_matches('"') == "true").unwrap_or(false);
        let clip_ready = clip_on && ml::clip_installed(&a);
        let faces_ready = faces_on && ml::faces_installed(&a);
        let tagged = counts("SELECT COUNT(*) FROM clip");
        let face_n = counts("SELECT COUNT(*) FROM faces");
        let people_n = counts("SELECT COUNT(*) FROM people");
        let clip_pending = counts(
            "SELECT COUNT(*) FROM media WHERE missing=0 AND deleted IS NULL AND state=1
             AND id NOT IN (SELECT media FROM clip)",
        );
        let faces_pending = counts(
            "SELECT COUNT(*) FROM media WHERE missing=0 AND deleted IS NULL AND state=1
             AND kind<>1 AND facedone=0",
        );

        Ok(json!({
          "ml": {
            "ready": clip_ready, "faces_ready": faces_ready,
            "tagged": tagged, "pending": clip_pending,
            "faces": face_n, "people": people_n, "faces_pending": faces_pending
          },
          "libraries": libs,
          "folders": folders,
          "counts": {"total": total, "bytes": bytes, "fav": favs, "video": videos,
                     "raw": raws, "bin": binned, "missing": missing, "errors": errors},
          "job": a.job.read().clone(),
          "task": a.jobs.read().task.clone(),
          "ffmpeg": a.ffmpeg,
          "port": a.port,
          "addrs": a.addrs,
          "data_dir": a.data_dir.to_string_lossy().replace('\\', "/"),
          "settings": settings,
          "uptime": a.started.elapsed().as_secs(),
        }))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn get_settings(State(app): S) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        let mut m = serde_json::Map::new();
        let mut st = c.prepare("SELECT k, v FROM settings")?;
        for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (k, v) = row?;
            m.insert(k, serde_json::from_str(&v).unwrap_or(Value::String(v)));
        }
        Ok(Value::Object(m))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn post_settings(State(app): S, Json(body): Json<Value>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        if let Some(obj) = body.as_object() {
            for (k, v) in obj {
                db::set_setting(&c, k, &v.to_string())?;
            }
        }
        Ok(json!({"ok": true}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn browse(Query(q): Query<HashMap<String, String>>) -> Response {
    let p = q.get("path").cloned();
    match blocking(move || tools::browse(p.as_deref())).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

// ---------------------------------------------------------- libraries -------

#[derive(Deserialize)]
struct AddLib {
    path: String,
    #[serde(default)]
    name: Option<String>,
}

const LIB_COLORS: &[&str] = &["#4B79E4", "#E8A33D", "#B48CE0", "#3FA98A", "#5AA9E6", "#E86A9C"];

async fn add_library(State(app): S, Json(body): Json<AddLib>) -> Response {
    let a = app.clone();
    let path = body.path.replace('\\', "/").trim_end_matches('/').to_string();
    let name = body
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| {
            std::path::Path::new(&path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone())
        });

    let a2 = a.clone();
    let res = blocking(move || {
        if !std::path::Path::new(&path).is_dir() {
            anyhow::bail!("{path} is not a folder");
        }
        let c = a2.index.get()?;
        let n: i64 = c.query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get(0))?;
        let color = LIB_COLORS[(n as usize) % LIB_COLORS.len()];
        c.execute(
            "INSERT INTO libraries(path,name,color,added_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(path) DO UPDATE SET name=excluded.name",
            params![path, name, color, db::now()],
        )?;
        let id: i64 = c.query_row("SELECT id FROM libraries WHERE path=?1", [&path], |r| r.get(0))?;
        Ok(json!({"id": id, "path": path, "name": name, "color": color}))
    })
    .await;

    match res {
        Ok(v) => {
            scan::spawn(app, None, false);
            Json(v).into_response()
        }
        Err(e) => e,
    }
}

async fn remove_library(State(app): S, AxPath(id): AxPath<i64>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        let ids: Vec<i64> = {
            let mut st = c.prepare("SELECT id FROM media WHERE lib=?1")?;
            st.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        c.execute("DELETE FROM media WHERE lib=?1", [id])?;
        c.execute("DELETE FROM libraries WHERE id=?1", [id])?;
        tools::prune_orphans(&c)?;
        drop(c);
        let t = a.thumbs.get()?;
        for chunk in ids.chunks(400) {
            let list = chunk.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            t.execute(&format!("DELETE FROM t WHERE id IN ({list})"), [])?;
        }
        // Files on disk are never touched by removing a library.
        Ok(json!({"ok": true, "removed": ids.len()}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

#[derive(Deserialize, Default)]
struct RescanReq {
    #[serde(default)]
    libs: Option<Vec<i64>>,
    #[serde(default)]
    rethumb: bool,
}

async fn rescan(State(app): S, body: Option<Json<RescanReq>>) -> Response {
    let Json(b) = body.unwrap_or(Json(RescanReq::default()));
    scan::spawn(app, b.libs, b.rethumb);
    Json(json!({"ok": true})).into_response()
}

// -------------------------------------------------------------- media -------

#[derive(Deserialize, Default)]
struct ListQ {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    lib: Option<i64>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    bin: Option<i64>,
    #[serde(default)]
    coll: Option<i64>,
    #[serde(default)]
    person: Option<i64>,
}

/// The whole ordered list, column-oriented and stripped to what the grid draws.
/// One request buys instant virtualised scrolling with correct total height and
/// correct date-group boundaries; fetching pages as you scroll cannot do either
/// without guessing.
async fn media_list(State(app): S, Query(q): Query<ListQ>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        let mut where_parts: Vec<String> = vec!["missing=0".into()];
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if q.bin.unwrap_or(0) == 1 {
            where_parts.push("deleted IS NOT NULL".into());
        } else {
            where_parts.push("deleted IS NULL".into());
        }
        if let Some(l) = q.lib {
            where_parts.push("lib = ?".into());
            args.push(Box::new(l));
        }
        if let Some(cid) = q.coll {
            where_parts.push("id IN (SELECT media FROM collection_items WHERE coll = ?)".into());
            args.push(Box::new(cid));
        }
        if let Some(pid) = q.person {
            where_parts.push("id IN (SELECT media FROM faces WHERE cluster = ?)".into());
            args.push(Box::new(pid));
        }
        if let Some(s) = q.sub.as_ref().filter(|s| !s.is_empty()) {
            where_parts.push("(sub = ? OR sub LIKE ? || '/%')".into());
            args.push(Box::new(s.clone()));
            args.push(Box::new(s.clone()));
        }
        match q.filter.as_deref() {
            Some("fav") => where_parts.push("fav = 1".into()),
            Some("video") => where_parts.push("kind = 1".into()),
            Some("photo") => where_parts.push("kind = 0".into()),
            Some("raw") => where_parts.push("kind IN (2,3)".into()),
            Some("unrated") => where_parts.push("rating = 0".into()),
            Some("rated") => where_parts.push("rating >= 4".into()),
            Some("error") => where_parts.push("state = 2".into()),
            _ => {}
        }
        if let Some(text) = q.q.as_ref().filter(|s| !s.trim().is_empty()) {
            // Auto-generated tags are searched alongside names, which is what
            // makes a plain-text query like "boats" find boats.
            where_parts.push(
                "(name LIKE ? OR rel LIKE ? OR camera LIKE ? OR id IN (
                    SELECT mt.media_id FROM media_tags mt JOIN tags t ON t.id = mt.tag_id
                    WHERE t.name LIKE ?))"
                    .into(),
            );
            let pat = format!("%{}%", text.trim());
            for _ in 0..4 {
                args.push(Box::new(pat.clone()));
            }
        }

        let order = match q.sort.as_deref() {
            Some("oldest") => "COALESCE(taken, mtime) ASC, id ASC",
            Some("name") => "name COLLATE NOCASE ASC",
            Some("name_desc") => "name COLLATE NOCASE DESC",
            Some("size") => "bytes DESC",
            Some("size_asc") => "bytes ASC",
            Some("rating") => "rating DESC, COALESCE(taken, mtime) DESC",
            Some("added") => "added_at DESC, id DESC",
            Some("random") => "id * 2654435761 % 1000003",
            _ => "COALESCE(taken, mtime) DESC, id DESC",
        };

        let sql = format!(
            "SELECT id, COALESCE(taken, mtime), kind, fav, rating, state, dur, ext, vcodec, acodec, w, h
             FROM media WHERE {} ORDER BY {} LIMIT 400000",
            where_parts.join(" AND "),
            order
        );

        let mut st = c.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = st.query_map(refs.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<f64>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<i64>>(11)?,
            ))
        })?;

        let mut ids = Vec::new();
        let mut ts = Vec::new();
        let mut fl = Vec::new();
        let mut dur = Vec::new();
        // Dimensions ride along so the viewer can size its frame before the
        // picture arrives. Without them every navigation reflows.
        let mut w = Vec::new();
        let mut h = Vec::new();

        for row in rows.flatten() {
            ids.push(row.0);
            ts.push(row.1);
            // kind:2 | fav:1 | rating:3 | error:1 | native-video:1
            let native =
                decode::video_plays_natively(&row.7, row.8.as_deref(), row.9.as_deref()) as i64;
            fl.push(
                (row.2 & 3)
                    | (row.3 & 1) << 2
                    | (row.4 & 7) << 3
                    | ((row.5 == db::ST_ERR) as i64) << 6
                    | native << 7,
            );
            dur.push(row.6.unwrap_or(0.0).round() as i64);
            w.push(row.10.unwrap_or(0));
            h.push(row.11.unwrap_or(0));
        }

        Ok(json!({"total": ids.len(), "ids": ids, "ts": ts, "fl": fl,
                  "dur": dur, "w": w, "h": h}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn media_one(State(app): S, AxPath(id): AxPath<i64>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        let v = c.query_row(
            "SELECT m.id, m.name, m.path, m.rel, m.sub, m.ext, m.kind, m.bytes, m.mtime, m.taken,
                    m.w, m.h, m.camera, m.lens, m.iso, m.fnum, m.expo, m.focal, m.dur,
                    m.vcodec, m.acodec, m.rating, m.fav, m.deleted, m.state, m.err, l.name, l.id
             FROM media m JOIN libraries l ON l.id = m.lib WHERE m.id = ?1",
            [id],
            |r| {
                Ok(json!({
                  "id": r.get::<_,i64>(0)?, "name": r.get::<_,String>(1)?,
                  "path": r.get::<_,String>(2)?, "rel": r.get::<_,String>(3)?,
                  "sub": r.get::<_,String>(4)?, "ext": r.get::<_,String>(5)?,
                  "kind": r.get::<_,i64>(6)?, "bytes": r.get::<_,i64>(7)?,
                  "mtime": r.get::<_,i64>(8)?, "taken": r.get::<_,Option<i64>>(9)?,
                  "w": r.get::<_,Option<i64>>(10)?, "h": r.get::<_,Option<i64>>(11)?,
                  "camera": r.get::<_,Option<String>>(12)?, "lens": r.get::<_,Option<String>>(13)?,
                  "iso": r.get::<_,Option<i64>>(14)?, "fnum": r.get::<_,Option<f64>>(15)?,
                  "expo": r.get::<_,Option<String>>(16)?, "focal": r.get::<_,Option<f64>>(17)?,
                  "dur": r.get::<_,Option<f64>>(18)?,
                  "vcodec": r.get::<_,Option<String>>(19)?, "acodec": r.get::<_,Option<String>>(20)?,
                  "rating": r.get::<_,i64>(21)?, "fav": r.get::<_,i64>(22)?,
                  "deleted": r.get::<_,Option<i64>>(23)?, "state": r.get::<_,i64>(24)?,
                  "err": r.get::<_,Option<String>>(25)?,
                  "library": r.get::<_,String>(26)?, "lib": r.get::<_,i64>(27)?
                }))
            },
        )?;
        Ok(v)
    })
    .await
    {
        Ok(mut v) => {
            let ext = v["ext"].as_str().unwrap_or("").to_string();
            let native = decode::video_plays_natively(
                &ext,
                v["vcodec"].as_str(),
                v["acodec"].as_str(),
            );
            v["native"] = json!(native);
            v["browser_image"] =
                json!(matches!(ext.as_str(), "jpg" | "jpeg" | "jfif" | "png" | "webp" | "gif" | "bmp" | "avif"));
            let a2 = app.clone();
            if let Ok(extra) = blocking(move || {
                let colls = sets::memberships(&a2, id)?;
                let c = a2.index.get()?;
                let mut st = c.prepare(
                    "SELECT f.id, f.x, f.y, f.w, f.h, f.cluster, COALESCE(p.name,'')
                     FROM faces f LEFT JOIN people p ON p.id = f.cluster WHERE f.media = ?1",
                )?;
                let faces: Vec<Value> = st
                    .query_map([id], |r| {
                        Ok(json!({"id": r.get::<_,i64>(0)?, "x": r.get::<_,f64>(1)?,
                                  "y": r.get::<_,f64>(2)?, "w": r.get::<_,f64>(3)?,
                                  "h": r.get::<_,f64>(4)?, "cluster": r.get::<_,Option<i64>>(5)?,
                                  "name": r.get::<_,String>(6)?}))
                    })?
                    .collect::<Result<_, _>>()?;
                let mut tg = c.prepare(
                    "SELECT t.name, mt.score FROM media_tags mt JOIN tags t ON t.id = mt.tag_id
                     WHERE mt.media_id = ?1 ORDER BY mt.score DESC LIMIT 12",
                )?;
                let tags: Vec<Value> = tg
                    .query_map([id], |r| {
                        Ok(json!({"name": r.get::<_,String>(0)?, "score": r.get::<_,f64>(1)?}))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(json!({"collections": colls, "faces": faces, "tags": tags}))
            })
            .await
            {
                v["collections"] = extra["collections"].clone();
                v["faces"] = extra["faces"].clone();
                v["tags"] = extra["tags"].clone();
            }
            Json(v).into_response()
        }
        Err(e) => e,
    }
}

#[derive(Deserialize)]
struct MetaReq {
    #[serde(default)]
    fav: Option<bool>,
    #[serde(default)]
    rating: Option<i64>,
}

async fn media_meta(State(app): S, AxPath(id): AxPath<i64>, Json(b): Json<MetaReq>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        if let Some(f) = b.fav {
            c.execute("UPDATE media SET fav=?2 WHERE id=?1", params![id, f as i64])?;
        }
        if let Some(r) = b.rating {
            c.execute("UPDATE media SET rating=?2 WHERE id=?1", params![id, r.clamp(0, 5)])?;
        }
        Ok(json!({"ok": true}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

// --------------------------------------------------------------- bytes ------

async fn thumb(State(app): S, AxPath(id): AxPath<i64>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let t = a.thumbs.get()?;
        let d: Vec<u8> = t.query_row(
            "SELECT data FROM t WHERE id=?1 AND tier=?2",
            params![id, TIER_GRID],
            |r| r.get(0),
        )?;
        Ok(d)
    })
    .await
    {
        Ok(d) => stream::bytes_response(d, "image/jpeg", true),
        // A miss is normal while indexing is still catching up.
        Err(_) => (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-store")], "no thumbnail yet")
            .into_response(),
    }
}

async fn preview(State(app): S, AxPath(id): AxPath<i64>) -> Response {
    let a = app.clone();
    match blocking(move || scan::ensure_preview(&a, id)).await {
        Ok(d) => stream::bytes_response(d, "image/jpeg", true),
        Err(e) => e,
    }
}

fn row_paths(app: &App, id: i64) -> Result<(String, i64, String, String)> {
    let c = app.index.get()?;
    Ok(c.query_row(
        "SELECT COALESCE(binpath, path), kind, ext, name FROM media WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?)
}

/// Full resolution. Formats the browser understands are served straight off
/// disk; everything else is converted on the way out.
async fn full(State(app): S, AxPath(id): AxPath<i64>, headers: HeaderMap) -> Response {
    let a = app.clone();
    let (path, kind, ext, _name) = match blocking(move || row_paths(&a, id)).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let browser_native =
        matches!(ext.as_str(), "jpg" | "jpeg" | "jfif" | "png" | "webp" | "gif" | "bmp" | "avif");
    if browser_native {
        let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
        return stream::file_range(std::path::Path::new(&path), &headers, &mime, None).await;
    }

    match blocking(move || {
        let img = decode::load(std::path::Path::new(&path), kind, &ext, 4096)?;
        let fit = decode::fit_thumb(&img, 4096);
        decode::encode_jpeg(&fit, 90)
    })
    .await
    {
        Ok(d) => stream::bytes_response(d, "image/jpeg", true),
        Err(e) => e,
    }
}

async fn download(State(app): S, AxPath(id): AxPath<i64>, headers: HeaderMap) -> Response {
    let a = app.clone();
    match blocking(move || row_paths(&a, id)).await {
        Ok((path, _, _, name)) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
            stream::file_range(std::path::Path::new(&path), &headers, &mime, Some(&name)).await
        }
        Err(e) => e,
    }
}

async fn video(State(app): S, AxPath(id): AxPath<i64>, headers: HeaderMap) -> Response {
    let a = app.clone();
    let row = {
        let a2 = a.clone();
        blocking(move || {
            let c = a2.index.get()?;
            Ok(c.query_row(
                "SELECT COALESCE(binpath, path), ext, vcodec, acodec FROM media WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )?)
        })
        .await
    };
    let Ok((path, ext, vc, ac)) = row else { return err(StatusCode::NOT_FOUND, "not found") };

    if decode::video_plays_natively(&ext, vc.as_deref(), ac.as_deref()) {
        let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
        stream::file_range(std::path::Path::new(&path), &headers, &mime, None).await
    } else if app.ffmpeg {
        stream::transcode(std::path::Path::new(&path), 0.0, 1080)
    } else {
        err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "this clip needs ffmpeg to play, and ffmpeg was not found")
    }
}

async fn transcode(
    State(app): S,
    AxPath(id): AxPath<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !app.ffmpeg {
        return err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "ffmpeg not found");
    }
    let a = app.clone();
    let path = match blocking(move || row_paths(&a, id)).await {
        Ok((p, _, _, _)) => p,
        Err(e) => return e,
    };
    let t: f64 = q.get("t").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let h: u32 = q.get("h").and_then(|s| s.parse().ok()).unwrap_or(1080);
    stream::transcode(std::path::Path::new(&path), t, h)
}

// ----------------------------------------------------------- bin & bulk -----

#[derive(Deserialize, Default)]
struct IdsReq {
    #[serde(default)]
    ids: Vec<i64>,
    #[serde(default)]
    all: bool,
}

async fn do_bin(State(app): S, Json(b): Json<IdsReq>) -> Response {
    let a = app.clone();
    match blocking(move || Ok(tools::bin(&a, &b.ids)?)).await {
        Ok(n) => Json(json!({"ok": true, "moved": n})).into_response(),
        Err(e) => e,
    }
}

async fn do_restore(State(app): S, Json(b): Json<IdsReq>) -> Response {
    let a = app.clone();
    match blocking(move || Ok(tools::restore(&a, &b.ids)?)).await {
        Ok(n) => Json(json!({"ok": true, "restored": n})).into_response(),
        Err(e) => e,
    }
}

async fn do_purge(State(app): S, Json(b): Json<IdsReq>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let ids = if b.all { None } else { Some(b.ids.clone()) };
        Ok(tools::purge(&a, ids.as_deref())?)
    })
    .await
    {
        Ok((n, freed)) => Json(json!({"ok": true, "purged": n, "freed": freed})).into_response(),
        Err(e) => e,
    }
}

// ------------------------------------------------------------ storage -------

async fn storage(State(app): S) -> Response {
    let a = app.clone();
    match blocking(move || tools::storage(&a)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn duplicates(State(app): S, Query(q): Query<HashMap<String, String>>) -> Response {
    let exact = q.get("exact").map(|v| v == "1").unwrap_or(false);
    let a = app.clone();
    match blocking(move || tools::duplicates(&a, exact)).await {
        Ok(g) => Json(json!({"groups": g})).into_response(),
        Err(e) => e,
    }
}

async fn start_hash(State(app): S) -> Response {
    let a = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = scan::ensure_hashes(&a) {
            eprintln!("[hash] {e:#}");
        }
    });
    Json(json!({"ok": true})).into_response()
}

async fn start_compress(State(app): S, Json(req): Json<tools::CompressReq>) -> Response {
    if app.jobs.read().task.as_ref().map(|t| t.running).unwrap_or(false) {
        return err(StatusCode::CONFLICT, "another job is already running");
    }
    tools::spawn_compress(app, req);
    Json(json!({"ok": true})).into_response()
}

async fn preview_compress(State(app): S, Json(req): Json<tools::CompressReq>) -> Response {
    let a = app.clone();
    match blocking(move || tools::compress_preview(&a, &req)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

// ------------------------------------------------------------- upload -------

/// Take files from a phone or a browser and file them into the upload folder,
/// grouped by the month the photo was actually taken. The upload folder is
/// registered as a library on first use, so uploaded photos appear in the grid
/// without the user having to add anything.
async fn upload(State(app): S, mut mp: Multipart) -> Response {
    let (root, organize) = {
        let a = app.clone();
        match blocking(move || {
            let c = a.index.get()?;
            let root = sets::inbox_root(&c);
            let organize = sets::inbox_organize(&c);
            std::fs::create_dir_all(&root)?;
            Ok((root, organize))
        })
        .await
        {
            Ok(v) => v,
            Err(e) => return e,
        }
    };

    let mut saved = Vec::new();
    let mut bytes_total = 0usize;
    let mut failed = 0usize;

    while let Ok(Some(field)) = mp.next_field().await {
        let name = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("upload-{}.bin", db::now()));
        let safe = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        let Ok(data) = field.bytes().await else {
            failed += 1;
            continue;
        };
        if data.is_empty() {
            continue;
        }

        // Read the capture date before writing, so the file goes straight to the
        // right month instead of landing wrong and being moved afterwards.
        let taken = sets::taken_from_bytes(&data, &safe);
        let dir = sets::dated_dir(&root, taken, organize);
        if tokio::fs::create_dir_all(&dir).await.is_err() {
            failed += 1;
            continue;
        }
        let dest = scan::unique_dest(&dir, &safe);
        if tokio::fs::write(&dest, &data).await.is_ok() {
            bytes_total += data.len();
            saved.push(dest.to_string_lossy().replace('\\', "/"));
        } else {
            failed += 1;
        }
    }

    if !saved.is_empty() {
        let a = app.clone();
        let root2 = root.clone();
        let _ = blocking(move || {
            let c = a.index.get()?;
            // Make sure the destination is actually a library, otherwise the
            // uploads would sit on disk and never show up.
            let known: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM libraries WHERE ?1 = path OR ?1 LIKE path || '/%'",
                    [&root2],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if known == 0 {
                let n: i64 = c.query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get(0))?;
                let label = std::path::Path::new(&root2)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Uploads".into());
                c.execute(
                    "INSERT INTO libraries(path,name,color,added_at) VALUES(?1,?2,?3,?4)
                     ON CONFLICT(path) DO NOTHING",
                    rusqlite::params![
                        root2,
                        label,
                        LIB_COLORS[(n as usize) % LIB_COLORS.len()],
                        db::now()
                    ],
                )?;
            }
            Ok(())
        })
        .await;
        scan::spawn(app, None, false);
    }

    Json(json!({
        "ok": true, "saved": saved.len(), "failed": failed,
        "bytes": bytes_total, "inbox": root
    }))
    .into_response()
}

// --------------------------------------------------------------- edit -------

async fn save_edit(
    State(app): S,
    AxPath(id): AxPath<i64>,
    Query(q): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    let overwrite = q.get("mode").map(|m| m == "overwrite").unwrap_or(false);
    if body.is_empty() {
        return err(StatusCode::BAD_REQUEST, "empty image");
    }
    let a = app.clone();
    let data = body.to_vec();
    match blocking(move || tools::save_edit(&a, id, &data, overwrite)).await {
        Ok(new_id) => {
            scan::spawn(app, None, false);
            Json(json!({"ok": true, "id": new_id})).into_response()
        }
        Err(e) => e,
    }
}

// --------------------------------------------------------- collections -----

async fn colls_list(State(app): S) -> Response {
    let a = app.clone();
    match blocking(move || sets::list(&a)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

#[derive(Deserialize)]
struct NameReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    note: Option<String>,
}

async fn colls_create(State(app): S, Json(b): Json<NameReq>) -> Response {
    let a = app.clone();
    match blocking(move || sets::create(&a, &b.name)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn colls_update(State(app): S, AxPath(id): AxPath<i64>, Json(b): Json<NameReq>) -> Response {
    let a = app.clone();
    match blocking(move || {
        sets::rename(&a, id, &b.name, b.note.as_deref())?;
        Ok(json!({"ok": true}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn colls_delete(State(app): S, AxPath(id): AxPath<i64>) -> Response {
    let a = app.clone();
    match blocking(move || {
        sets::remove(&a, id)?;
        Ok(json!({"ok": true}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn colls_items(
    State(app): S,
    AxPath(id): AxPath<i64>,
    Json(b): Json<sets::ItemsReq>,
) -> Response {
    let a = app.clone();
    match blocking(move || Ok(sets::set_items(&a, id, &b)?)).await {
        Ok(n) => Json(json!({"ok": true, "changed": n})).into_response(),
        Err(e) => e,
    }
}

/// Build the archive on disk, hand it over, then bin it. Stale archives are
/// swept on the way in so a forgotten export never quietly eats the drive.
async fn colls_export(
    State(app): S,
    AxPath(id): AxPath<i64>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let originals = q.get("originals").map(|v| v != "0").unwrap_or(true);
    let a = app.clone();
    let built = blocking(move || {
        sweep_exports(&a.data_dir);
        sets::export_zip(&a, id, originals)
    })
    .await;

    match built {
        Ok((path, name)) => {
            let resp = stream::file_range(&path, &headers, "application/zip", Some(&name)).await;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(900)).await;
                let _ = tokio::fs::remove_file(path).await;
            });
            resp
        }
        Err(e) => e,
    }
}

fn sweep_exports(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("export-") || !name.ends_with(".zip") {
            continue;
        }
        if e.metadata().and_then(|m| m.modified()).map(|m| m < cutoff).unwrap_or(false) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// -------------------------------------------------------------- reveal -----

#[derive(Deserialize)]
struct RevealReq {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    path: Option<String>,
}

async fn reveal(State(app): S, Json(b): Json<RevealReq>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let path = match (b.id, b.path) {
            (Some(id), _) => {
                let c = a.index.get()?;
                c.query_row("SELECT COALESCE(binpath, path) FROM media WHERE id=?1", [id], |r| {
                    r.get::<_, String>(0)
                })?
            }
            (_, Some(p)) => p,
            _ => anyhow::bail!("nothing to reveal"),
        };
        sets::reveal(&path)?;
        Ok(json!({"ok": true, "path": path}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

// ------------------------------------------------------ machine learning ---

#[derive(Deserialize)]
struct WhatReq {
    #[serde(default)]
    what: String,
}

async fn ml_enable(State(app): S, Json(b): Json<WhatReq>) -> Response {
    let what = if b.what == "faces" { "faces" } else { "clip" };
    if app.job.read().running {
        return err(StatusCode::CONFLICT, "something else is already running — try again when it finishes");
    }
    let installed = if what == "faces" { ml::faces_installed(&app) } else { ml::clip_installed(&app) };
    if installed {
        let a = app.clone();
        let key = if what == "faces" { "faces_on" } else { "clip_on" };
        let _ = blocking(move || {
            let c = a.index.get()?;
            db::set_setting(&c, key, "true")?;
            Ok(())
        })
        .await;
        if what == "faces" { ml::spawn_face_pass(app) } else { ml::spawn_clip_pass(app) }
    } else {
        ml::spawn_install(app, what.to_string());
    }
    Json(json!({"ok": true})).into_response()
}

async fn ml_disable(State(app): S, Json(b): Json<WhatReq>) -> Response {
    let key = if b.what == "faces" { "faces_on" } else { "clip_on" };
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        db::set_setting(&c, key, "false")?;
        Ok(json!({"ok": true}))
    })
    .await
    {
        // The downloaded weights are left in place: turning the feature back on
        // should not mean downloading 300 MB again.
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn ml_reindex(State(app): S, Json(b): Json<WhatReq>) -> Response {
    let faces = b.what == "faces";
    let a = app.clone();
    let cleared = blocking(move || {
        let c = a.index.get()?;
        if faces {
            c.execute("DELETE FROM faces", [])?;
            c.execute("DELETE FROM people", [])?;
            c.execute("UPDATE media SET facedone = 0", [])?;
        } else {
            c.execute("DELETE FROM clip", [])?;
            c.execute("DELETE FROM media_tags", [])?;
        }
        Ok(())
    })
    .await;
    if let Err(e) = cleared {
        return e;
    }
    if faces { ml::spawn_face_pass(app) } else { ml::spawn_clip_pass(app) }
    Json(json!({"ok": true})).into_response()
}

async fn people_merge(State(app): S, Json(b): Json<MergeReq>) -> Response {
    let a = app.clone();
    let into = b.into.unwrap_or_else(|| *b.ids.first().unwrap_or(&0));
    match blocking(move || Ok(ml::merge_people(&a, &b.ids, into)?)).await {
        Ok(n) => Json(json!({"ok": true, "moved": n, "into": into})).into_response(),
        Err(e) => e,
    }
}

#[derive(Deserialize)]
struct MergeReq {
    #[serde(default)]
    ids: Vec<i64>,
    #[serde(default)]
    into: Option<i64>,
}

async fn ml_recluster(State(app): S) -> Response {
    let a = app.clone();
    match blocking(move || ml::recluster_from_scratch(&a)).await {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(e) => e,
    }
}

/// Search by what the picture shows. Returns the same columnar shape as the
/// ordinary list endpoint so the grid does not need a second code path.
async fn semantic_search(State(app): S, Query(q): Query<HashMap<String, String>>) -> Response {
    let text = q.get("q").cloned().unwrap_or_default();
    if text.trim().is_empty() {
        return Json(json!({"total": 0, "ids": [], "ts": [], "fl": [], "dur": [], "w": [], "h": []}))
            .into_response();
    }
    let a = app.clone();
    match blocking(move || {
        let hits = ml::search(&a, text.trim(), 800)?;
        let c = a.index.get()?;
        let mut ids = Vec::new();
        let (mut ts, mut fl, mut dur, mut w, mut h) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut st = c.prepare(
            "SELECT COALESCE(taken, mtime), kind, fav, rating, state, dur, ext, vcodec, acodec, w, h
             FROM media WHERE id = ?1",
        )?;
        for (id, _score) in hits {
            let row = st.query_row([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, Option<f64>>(5)?,
                    r.get::<_, String>(6)?, r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?, r.get::<_, Option<i64>>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                ))
            });
            let Ok(r) = row else { continue };
            let native = decode::video_plays_natively(&r.6, r.7.as_deref(), r.8.as_deref()) as i64;
            ids.push(id);
            ts.push(r.0);
            fl.push((r.1 & 3) | (r.2 & 1) << 2 | (r.3 & 7) << 3
                    | ((r.4 == db::ST_ERR) as i64) << 6 | native << 7);
            dur.push(r.5.unwrap_or(0.0).round() as i64);
            w.push(r.9.unwrap_or(0));
            h.push(r.10.unwrap_or(0));
        }
        Ok(json!({"total": ids.len(), "ids": ids, "ts": ts, "fl": fl,
                  "dur": dur, "w": w, "h": h}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn people_list(State(app): S) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        let mut st = c.prepare(
            "SELECT p.id, p.name, p.cover,
                    (SELECT COUNT(DISTINCT f.media) FROM faces f WHERE f.cluster = p.id)
             FROM people p WHERE p.hidden = 0
             ORDER BY (p.name = '') ASC, 4 DESC",
        )?;
        let people: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                  "id": r.get::<_,i64>(0)?, "name": r.get::<_,String>(1)?,
                  "cover": r.get::<_,Option<i64>>(2)?, "count": r.get::<_,i64>(3)?
                }))
            })?
            .collect::<Result<_, _>>()?;
        Ok(json!({ "people": people }))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn people_rename(State(app): S, AxPath(id): AxPath<i64>, Json(b): Json<NameReq>) -> Response {
    let a = app.clone();
    match blocking(move || {
        let c = a.index.get()?;
        c.execute("UPDATE people SET name=?2 WHERE id=?1", rusqlite::params![id, b.name.trim()])?;
        Ok(json!({"ok": true}))
    })
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e,
    }
}

async fn face_thumb(State(app): S, AxPath(id): AxPath<i64>) -> Response {
    let a = app.clone();
    match blocking(move || ml::face_thumb(&a, id)).await {
        Ok(d) => stream::bytes_response(d, "image/jpeg", true),
        Err(e) => e,
    }
}
