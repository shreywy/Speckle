//! Local photo understanding: CLIP for search and auto-tagging, InsightFace for
//! people. Everything runs on this machine — models are fetched once and then
//! the network is never touched again, and no image ever leaves the disk it is
//! stored on.
//!
//! Both features are opt-in. The models are large, and a photo viewer that
//! silently downloads 400 MB the first time you open it would be rude.

use anyhow::{anyhow, bail, Context, Result};
use image::{DynamicImage, GenericImageView};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use parking_lot::RwLock;
use rayon::prelude::*;
use rusqlite::params;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use crate::db;
use crate::scan;
use crate::App;

pub const CLIP_DIM: usize = 512;
pub const FACE_DIM: usize = 512;

// ------------------------------------------------------------- registry ----

struct Download {
    url: &'static str,
    file: &'static str,
    approx: u64,
}

const CLIP_FILES: &[Download] = &[
    Download {
        url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model_quantized.onnx",
        file: "clip_vision.onnx",
        approx: 89_117_001,
    },
    Download {
        url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/text_model_quantized.onnx",
        file: "clip_text.onnx",
        approx: 64_504_507,
    },
    Download {
        url: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/tokenizer.json",
        file: "clip_tokenizer.json",
        approx: 2_224_119,
    },
];

const BUFFALO_URL: &str =
    "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip";

/// ONNX Runtime itself, loaded at run time. Shipping it as a DLL rather than
/// linking it in keeps the base executable small and means the build does not
/// depend on which Visual Studio toolset happens to be installed.
const ORT_URL: &str =
    "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip";
const ORT_DLL: &str = "onnxruntime.dll";

pub fn models_dir(app: &App) -> PathBuf {
    app.data_dir.join("models")
}

pub fn runtime_installed(app: &App) -> bool {
    models_dir(app).join(ORT_DLL).exists()
}

pub fn clip_installed(app: &App) -> bool {
    let d = models_dir(app);
    runtime_installed(app) && CLIP_FILES.iter().all(|f| d.join(f.file).exists())
}
pub fn faces_installed(app: &App) -> bool {
    let d = models_dir(app);
    runtime_installed(app) && d.join("det_10g.onnx").exists() && d.join("w600k_r50.onnx").exists()
}

static ORT_INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();

/// Point ONNX Runtime at the DLL we downloaded and initialise it exactly once.
fn init_runtime(app: &App) -> Result<()> {
    let dll = models_dir(app).join(ORT_DLL);
    let r = ORT_INIT.get_or_init(|| {
        if !dll.exists() {
            return Err("the ONNX runtime has not been downloaded yet".into());
        }
        ort::init_from(dll.to_string_lossy().to_string())
            .commit()
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    r.as_ref().map(|_| ()).map_err(|e| anyhow!("{e}"))
}

/// Fetch the runtime DLL. Shared by both features, so whichever the user turns
/// on first pays for it and the second is just the model weights.
fn ensure_runtime(app: &Arc<App>) -> Result<()> {
    let dir = models_dir(app);
    if dir.join(ORT_DLL).exists() {
        return Ok(());
    }
    let zip_path = dir.join("onnxruntime.zip");
    fetch(app, ORT_URL, &zip_path, 72_368_545, "Runtime")?;
    app.job.write().label = "Unpacking runtime…".into();
    extract_needed(&zip_path, &dir, &[ORT_DLL])?;
    let _ = std::fs::remove_file(&zip_path);
    Ok(())
}

fn fetch(app: &Arc<App>, url: &str, dest: &Path, approx: u64, label: &str) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let tmp = dest.with_extension("part");

    let resp = ureq::get(url).call().with_context(|| format!("fetching {url}"))?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(approx);

    let mut reader = resp.into_body().into_reader();
    let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    let mut buf = vec![0u8; 256 * 1024];
    let mut got: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])?;
        got += n as u64;
        let mut j = app.job.write();
        j.done = got / 1024;
        j.total = total.max(got) / 1024;
        j.label = format!("{label} — {:.0} MB of {:.0} MB", got as f64 / 1e6, total as f64 / 1e6);
    }
    std::io::Write::flush(&mut out)?;
    drop(out);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Download whichever model set the user asked for, then kick off the pass that
/// uses it. Runs on its own thread; progress shows in the sidebar.
pub fn spawn_install(app: Arc<App>, what: String) {
    std::thread::spawn(move || {
        {
            let mut j = app.job.write();
            if j.running {
                return;
            }
            *j = scan::Job { running: true, phase: "models".into(), ..Default::default() };
        }
        let res = (|| -> Result<()> {
            let dir = models_dir(&app);
            ensure_runtime(&app)?;
            if what == "clip" {
                for f in CLIP_FILES {
                    fetch(&app, f.url, &dir.join(f.file), f.approx, "Vision model")?;
                }
            } else {
                let zip_path = dir.join("buffalo_l.zip");
                fetch(&app, BUFFALO_URL, &zip_path, 288_621_354, "Face model")?;
                app.job.write().label = "Unpacking…".into();
                extract_needed(&zip_path, &dir, &["det_10g.onnx", "w600k_r50.onnx"])?;
                let _ = std::fs::remove_file(&zip_path);
            }
            Ok(())
        })();

        {
            let mut j = app.job.write();
            j.running = false;
            j.phase = "idle".into();
            if let Err(e) = &res {
                j.label = format!("model download failed: {e}");
            } else {
                j.label.clear();
            }
        }
        if res.is_err() {
            eprintln!("[ml] install failed: {res:?}");
            return;
        }
        {
            // Scoped so the pooled connection is back before the pass starts.
            if let Ok(c) = app.index.get() {
                let _ = db::set_setting(&c, if what == "clip" { "clip_on" } else { "faces_on" }, "true");
            }
        }
        if what == "clip" {
            spawn_clip_pass(app);
        } else {
            spawn_face_pass(app);
        }
    });
}

/// buffalo_l.zip carries five models; only two are wanted, and the archive is
/// stored so extraction is a straight copy.
fn extract_needed(zip_path: &Path, dir: &Path, wanted: &[&str]) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let name = Path::new(e.name())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !wanted.contains(&name.as_str()) {
            continue;
        }
        let mut out = std::fs::File::create(dir.join(&name))?;
        std::io::copy(&mut e, &mut out)?;
    }
    for w in wanted {
        if !dir.join(w).exists() {
            bail!("{w} was not in the archive");
        }
    }
    Ok(())
}

fn session(path: &Path) -> Result<Session> {
    Ok(Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(num_cpus::get().clamp(1, 8))?
        .commit_from_file(path)?)
}

// ----------------------------------------------------------------- CLIP ----

pub struct Clip {
    vision: RwLock<Session>,
    text: RwLock<Session>,
    tok: tokenizers::Tokenizer,
    /// Concept vocabulary, embedded once and reused for every photo.
    vocab: Vec<(&'static str, Vec<f32>)>,
}

impl Clip {
    fn load(app: &App) -> Result<Self> {
        let d = models_dir(app);
        let tok = tokenizers::Tokenizer::from_file(d.join("clip_tokenizer.json"))
            .map_err(|e| anyhow!("tokenizer: {e}"))?;
        let c = Clip {
            vision: RwLock::new(session(&d.join("clip_vision.onnx"))?),
            text: RwLock::new(session(&d.join("clip_text.onnx"))?),
            tok,
            vocab: Vec::new(),
        };
        Ok(c)
    }

    /// CLIP's own preprocessing: shortest side to 224 with a centre crop, then
    /// the published channel statistics. Getting these wrong degrades results
    /// quietly rather than loudly, so they are spelled out.
    fn preprocess(img: &DynamicImage) -> Vec<f32> {
        const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
        const STD: [f32; 3] = [0.268_629_54, 0.261_302_58, 0.275_777_11];
        let (w, h) = img.dimensions();
        let s = 224.0 / (w.min(h) as f32);
        let nw = ((w as f32 * s).round() as u32).max(224);
        let nh = ((h as f32 * s).round() as u32).max(224);
        let resized = img.resize_exact(nw, nh, image::imageops::FilterType::CatmullRom);
        let cropped = resized.crop_imm((nw - 224) / 2, (nh - 224) / 2, 224, 224).to_rgb8();

        let mut out = vec![0f32; 3 * 224 * 224];
        for (i, px) in cropped.pixels().enumerate() {
            for c in 0..3 {
                out[c * 224 * 224 + i] = (px.0[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
        out
    }

    pub fn embed_image(&self, img: &DynamicImage) -> Result<Vec<f32>> {
        let data = Self::preprocess(img);
        let input = Tensor::from_array(([1usize, 3, 224, 224], data))?;
        let mut sess = self.vision.write();
        let out = sess.run(ort::inputs!["pixel_values" => input])?;
        let (_, v) = out["image_embeds"].try_extract_tensor::<f32>()?;
        Ok(normalize(v.to_vec()))
    }

    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let enc = self.tok.encode(text, true).map_err(|e| anyhow!("tokenize: {e}"))?;
        // CLIP is trained at a fixed 77-token context and pools at the end-of-text
        // token, which it locates as the highest id present — so zero padding
        // after it is correct, and no attention mask is needed or accepted.
        let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        ids.truncate(77);
        ids.resize(77, 0);
        let t_ids = Tensor::from_array(([1usize, 77], ids))?;
        let mut sess = self.text.write();
        let out = sess.run(ort::inputs!["input_ids" => t_ids])?;
        let (_, v) = out["text_embeds"].try_extract_tensor::<f32>()?;
        Ok(normalize(v.to_vec()))
    }

    fn build_vocab(&mut self) -> Result<()> {
        if !self.vocab.is_empty() {
            return Ok(());
        }
        let mut v = Vec::with_capacity(VOCAB.len());
        for term in VOCAB {
            let e = self.embed_text(&format!("a photo of {term}"))?;
            v.push((*term, e));
        }
        self.vocab = v;
        Ok(())
    }

    /// The handful of concepts this picture matches most strongly. Only terms
    /// clearly above the field are kept — CLIP always ranks something first,
    /// and a confident-looking wrong tag is worse than no tag.
    fn tags_for(&self, vec: &[f32]) -> Vec<(&'static str, f32)> {
        let mut scored: Vec<(&str, f32)> =
            self.vocab.iter().map(|(t, e)| (*t, dot(vec, e))).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let best = scored.first().map(|s| s.1).unwrap_or(0.0);
        scored
            .into_iter()
            .take(6)
            .filter(|(_, s)| *s > 0.21 && *s > best - 0.035)
            .collect()
    }
}

pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    v.iter_mut().for_each(|x| *x /= n);
    v
}
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

// --------------------------------------------------------------- engines ---

pub fn clip(app: &Arc<App>) -> Result<Arc<Clip>> {
    if let Some(c) = app.clip.read().as_ref() {
        return Ok(c.clone());
    }
    if !clip_installed(app) {
        bail!("the vision model is not installed");
    }
    init_runtime(app)?;
    let mut c = Clip::load(app)?;
    c.build_vocab()?;
    let arc = Arc::new(c);
    *app.clip.write() = Some(arc.clone());
    Ok(arc)
}

pub fn faces(app: &Arc<App>) -> Result<Arc<FaceNet>> {
    if let Some(f) = app.faces.read().as_ref() {
        return Ok(f.clone());
    }
    if !faces_installed(app) {
        bail!("the face model is not installed");
    }
    init_runtime(app)?;
    let f = Arc::new(FaceNet::load(app)?);
    *app.faces.write() = Some(f.clone());
    Ok(f)
}

// ------------------------------------------------------------ CLIP pass ----

pub fn spawn_clip_pass(app: Arc<App>) {
    std::thread::spawn(move || {
        if let Err(e) = clip_pass(&app) {
            eprintln!("[ml] clip pass: {e:#}");
            app.job.write().label = format!("failed: {e}");
        }
        let mut j = app.job.write();
        j.running = false;
        j.phase = "idle".into();
    });
}

fn clip_pass(app: &Arc<App>) -> Result<()> {
    {
        let mut j = app.job.write();
        if j.running {
            return Ok(());
        }
        *j = scan::Job { running: true, phase: "tagging".into(), label: "loading model".into(), ..Default::default() };
    }
    let engine = clip(app)?;
    let conn = app.index.standalone()?;

    let todo: Vec<(i64, String, i64, String)> = {
        let mut st = conn.prepare(
            "SELECT id, path, kind, ext FROM media
             WHERE missing=0 AND deleted IS NULL AND state=1
               AND id NOT IN (SELECT media FROM clip)
             ORDER BY id DESC",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?
    };
    let total = todo.len() as u64;
    if total == 0 {
        return Ok(());
    }
    {
        let mut j = app.job.write();
        j.total = total;
        j.done = 0;
    }

    let (tx, rx) = mpsc::sync_channel::<(i64, Vec<f32>, Vec<(&'static str, f32)>)>(64);
    let job = app.job.clone();
    let writer = {
        let conn2 = app.index.standalone()?;
        std::thread::spawn(move || -> Result<()> {
            let start = Instant::now();
            let mut done = 0u64;
            let mut batch = Vec::with_capacity(32);
            let flush = |c: &rusqlite::Connection, b: &mut Vec<(i64, Vec<f32>, Vec<(&str, f32)>)>| -> Result<()> {
                if b.is_empty() {
                    return Ok(());
                }
                let tx = c.unchecked_transaction()?;
                {
                    let mut ins = tx.prepare_cached(
                        "INSERT INTO clip(media, vec) VALUES(?1,?2)
                         ON CONFLICT(media) DO UPDATE SET vec=excluded.vec",
                    )?;
                    let mut mktag = tx.prepare_cached("INSERT OR IGNORE INTO tags(name) VALUES(?1)")?;
                    let mut gettag = tx.prepare_cached("SELECT id FROM tags WHERE name=?1")?;
                    let mut clear = tx.prepare_cached("DELETE FROM media_tags WHERE media_id=?1")?;
                    let mut link = tx.prepare_cached(
                        "INSERT OR REPLACE INTO media_tags(media_id, tag_id, score) VALUES(?1,?2,?3)",
                    )?;
                    for (id, vec, tags) in b.iter() {
                        ins.execute(params![id, to_blob(vec)])?;
                        clear.execute([id])?;
                        for (name, score) in tags {
                            mktag.execute([name])?;
                            let tid: i64 = gettag.query_row([name], |r| r.get(0))?;
                            link.execute(params![id, tid, score])?;
                        }
                    }
                }
                tx.commit()?;
                b.clear();
                Ok(())
            };
            while let Ok(item) = rx.recv() {
                batch.push(item);
                done += 1;
                if batch.len() >= 32 {
                    flush(&conn2, &mut batch)?;
                }
                if done % 8 == 0 {
                    let secs = start.elapsed().as_secs_f64().max(0.001);
                    let rate = done as f64 / secs;
                    let mut j = job.write();
                    j.done = done;
                    j.rate = rate;
                    j.eta = if rate > 0.0 { (total - done) as f64 / rate } else { 0.0 };
                }
            }
            flush(&conn2, &mut batch)?;
            Ok(())
        })
    };

    let label = app.job.clone();
    todo.par_iter().for_each_with(tx, |tx, (id, path, kind, ext)| {
        // A 512-pixel decode is ample: CLIP only ever sees 224 square.
        let Ok(img) = crate::decode::load(Path::new(path), *kind, ext, 512) else { return };
        let Ok(vec) = engine.embed_image(&img) else { return };
        let tags = engine.tags_for(&vec);
        {
            let mut j = label.write();
            if j.done % 16 == 0 {
                j.label = Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            }
        }
        let _ = tx.send((*id, vec, tags));
    });

    writer.join().map_err(|_| anyhow!("clip writer panicked"))??;
    let _ = conn;
    Ok(())
}

/// Rank every embedded photo against a phrase. A brute-force scan is the right
/// tool here: 50k dot products over 512 floats is a few milliseconds, and an
/// approximate index would add a dependency and a staleness problem for no
/// perceptible gain at this scale.
pub fn search(app: &Arc<App>, q: &str, limit: usize) -> Result<Vec<(i64, f32)>> {
    let engine = clip(app)?;
    let qv = engine.embed_text(&format!("a photo of {q}"))?;
    let conn = app.index.get()?;
    let mut st = conn.prepare(
        "SELECT c.media, c.vec FROM clip c JOIN media m ON m.id = c.media
         WHERE m.deleted IS NULL AND m.missing = 0",
    )?;
    let mut scored: Vec<(i64, f32)> = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?
        .filter_map(Result::ok)
        .map(|(id, blob)| (id, dot(&qv, &from_blob(&blob))))
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // CLIP similarities are squeezed into a narrow band — almost every photo in
    // a library scores 0.20-0.30 against almost any phrase — so an absolute
    // floor keeps everything and is useless as a filter. What actually
    // separates a match from a near-miss is the drop-off from the best score,
    // so the cut is relative to it, with a floor to catch queries that match
    // nothing at all.
    let best = scored.first().map(|s| s.1).unwrap_or(0.0);
    if best < 0.20 {
        return Ok(Vec::new());
    }
    scored.retain(|(_, s)| *s >= best - 0.030 && *s >= 0.195);
    scored.truncate(limit);
    Ok(scored)
}

// ---------------------------------------------------------------- faces ----

pub struct FaceNet {
    det: RwLock<Session>,
    rec: RwLock<Session>,
}

pub struct Detection {
    pub bbox: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub score: f32,
}

impl FaceNet {
    fn load(app: &App) -> Result<Self> {
        let d = models_dir(app);
        Ok(FaceNet {
            det: RwLock::new(session(&d.join("det_10g.onnx"))?),
            rec: RwLock::new(session(&d.join("w600k_r50.onnx"))?),
        })
    }

    /// SCRFD at 640×640. The image is letterboxed rather than squashed so faces
    /// keep their proportions, and the scale is undone on the way out.
    pub fn detect(&self, img: &DynamicImage) -> Result<Vec<Detection>> {
        const SIZE: u32 = 640;
        let (w, h) = img.dimensions();
        let scale = (SIZE as f32 / w as f32).min(SIZE as f32 / h as f32);
        let nw = (w as f32 * scale).round() as u32;
        let nh = (h as f32 * scale).round() as u32;
        let small = img.resize_exact(nw.max(1), nh.max(1), image::imageops::FilterType::Triangle).to_rgb8();

        let mut data = vec![0f32; 3 * (SIZE * SIZE) as usize];
        for y in 0..nh.min(SIZE) {
            for x in 0..nw.min(SIZE) {
                let px = small.get_pixel(x, y);
                for c in 0..3 {
                    data[c * (SIZE * SIZE) as usize + (y * SIZE + x) as usize] =
                        (px.0[c] as f32 - 127.5) / 128.0;
                }
            }
        }

        let input = Tensor::from_array(([1usize, 3, SIZE as usize, SIZE as usize], data))?;

        // Nine outputs: score / bbox / keypoint deltas for strides 8, 16 and 32.
        // Copied out inside this scope so the session lock is released before
        // the (much longer) decode below.
        let raw: Vec<Vec<f32>> = {
            let mut sess = self.det.write();
            let out = sess.run(ort::inputs!["input.1" => input])?;
            let mut v = Vec::with_capacity(9);
            for i in 0..9 {
                let (_, t) = out[i].try_extract_tensor::<f32>()?;
                v.push(t.to_vec());
            }
            v
        };

        let mut dets: Vec<Detection> = Vec::new();
        for (i, stride) in [8usize, 16, 32].into_iter().enumerate() {
            let scores = &raw[i];
            let bboxes = &raw[i + 3];
            let kpss = &raw[i + 6];
            let gw = SIZE as usize / stride;
            let gh = SIZE as usize / stride;
            const ANCHORS: usize = 2;

            for idx in 0..scores.len() {
                let s = scores[idx];
                if s < 0.5 {
                    continue;
                }
                let cell = idx / ANCHORS;
                if cell >= gw * gh {
                    continue;
                }
                let cx = ((cell % gw) * stride) as f32;
                let cy = ((cell / gw) * stride) as f32;
                let b = &bboxes[idx * 4..idx * 4 + 4];
                // Predictions are distances from the anchor centre, in strides.
                let x1 = cx - b[0] * stride as f32;
                let y1 = cy - b[1] * stride as f32;
                let x2 = cx + b[2] * stride as f32;
                let y2 = cy + b[3] * stride as f32;

                let mut kps = [[0f32; 2]; 5];
                for k in 0..5 {
                    kps[k][0] = (cx + kpss[idx * 10 + k * 2] * stride as f32) / scale;
                    kps[k][1] = (cy + kpss[idx * 10 + k * 2 + 1] * stride as f32) / scale;
                }
                dets.push(Detection {
                    bbox: [x1 / scale, y1 / scale, x2 / scale, y2 / scale],
                    kps,
                    score: s,
                });
            }
        }

        dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(nms(dets, 0.4))
    }

    /// ArcFace wants the face aligned to a fixed 112×112 template, so a
    /// similarity transform is fitted from the five landmarks and the crop is
    /// sampled through it. Feeding an unaligned crop roughly halves accuracy.
    pub fn embed(&self, img: &DynamicImage, d: &Detection) -> Result<Vec<f32>> {
        const TEMPLATE: [[f32; 2]; 5] = [
            [38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366],
            [41.5493, 92.3655], [70.7299, 92.2041],
        ];
        let (a, b, tx, ty) = similarity_transform(&d.kps, &TEMPLATE);
        // Invert the 2x2 [[a,-b],[b,a]] so destination pixels can be sampled.
        let det = a * a + b * b;
        if det.abs() < 1e-9 {
            bail!("degenerate face alignment");
        }
        let rgb = img.to_rgb8();
        let (iw, ih) = (rgb.width() as f32, rgb.height() as f32);
        let mut data = vec![0f32; 3 * 112 * 112];
        for y in 0..112usize {
            for x in 0..112usize {
                let dx = x as f32 - tx;
                let dy = y as f32 - ty;
                let sx = (a * dx + b * dy) / det;
                let sy = (-b * dx + a * dy) / det;
                let px = sample_bilinear(&rgb, sx.clamp(0.0, iw - 1.0), sy.clamp(0.0, ih - 1.0));
                for c in 0..3 {
                    data[c * 112 * 112 + y * 112 + x] = (px[c] - 127.5) / 127.5;
                }
            }
        }
        let input = Tensor::from_array(([1usize, 3, 112, 112], data))?;
        let mut sess = self.rec.write();
        let out = sess.run(ort::inputs!["input.1" => input])?;
        let (_, v) = out[0].try_extract_tensor::<f32>()?;
        Ok(normalize(v.to_vec()))
    }
}

fn sample_bilinear(img: &image::RgbImage, x: f32, y: f32) -> [f32; 3] {
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(img.width() - 1);
    let y1 = (y0 + 1).min(img.height() - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let mut out = [0f32; 3];
    for c in 0..3 {
        let p00 = img.get_pixel(x0, y0).0[c] as f32;
        let p10 = img.get_pixel(x1, y0).0[c] as f32;
        let p01 = img.get_pixel(x0, y1).0[c] as f32;
        let p11 = img.get_pixel(x1, y1).0[c] as f32;
        out[c] = p00 * (1.0 - fx) * (1.0 - fy) + p10 * fx * (1.0 - fy)
               + p01 * (1.0 - fx) * fy + p11 * fx * fy;
    }
    out
}

/// Least-squares similarity transform (scale + rotation + translation) mapping
/// `from` onto `to`, expressed as the matrix [[a,-b],[b,a]] plus a translation.
fn similarity_transform(from: &[[f32; 2]; 5], to: &[[f32; 2]; 5]) -> (f32, f32, f32, f32) {
    let n = 5.0f32;
    let (mut fx, mut fy, mut tx, mut ty) = (0.0, 0.0, 0.0, 0.0);
    for i in 0..5 {
        fx += from[i][0]; fy += from[i][1];
        tx += to[i][0];   ty += to[i][1];
    }
    fx /= n; fy /= n; tx /= n; ty /= n;

    let (mut sxx, mut sxy, mut var) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..5 {
        let (dx, dy) = (from[i][0] - fx, from[i][1] - fy);
        let (ex, ey) = (to[i][0] - tx, to[i][1] - ty);
        sxx += dx * ex + dy * ey;
        sxy += dx * ey - dy * ex;
        var += dx * dx + dy * dy;
    }
    let var = var.max(1e-9);
    let a = sxx / var;
    let b = sxy / var;
    (a, b, tx - (a * fx - b * fy), ty - (b * fx + a * fy))
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = a[2].min(b[2]);
    let y2 = a[3].min(b[3]);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    inter / (area_a + area_b - inter).max(1e-6)
}

fn nms(dets: Vec<Detection>, thresh: f32) -> Vec<Detection> {
    let mut keep: Vec<Detection> = Vec::new();
    for d in dets {
        if keep.iter().any(|k| iou(&k.bbox, &d.bbox) > thresh) {
            continue;
        }
        keep.push(d);
    }
    keep
}

// ------------------------------------------------------------ face pass ----

pub fn spawn_face_pass(app: Arc<App>) {
    std::thread::spawn(move || {
        if let Err(e) = face_pass(&app) {
            eprintln!("[ml] face pass: {e:#}");
            app.job.write().label = format!("failed: {e}");
        }
        {
            let mut j = app.job.write();
            j.running = false;
            j.phase = "idle".into();
        }
        if let Err(e) = cluster(&app) {
            eprintln!("[ml] clustering: {e:#}");
        }
    });
}

fn face_pass(app: &Arc<App>) -> Result<()> {
    {
        let mut j = app.job.write();
        if j.running {
            return Ok(());
        }
        *j = scan::Job { running: true, phase: "faces".into(), label: "loading model".into(), ..Default::default() };
    }
    let engine = faces(app)?;
    let conn = app.index.standalone()?;

    // Only photos: a face in a video frame is not worth the decode cost here.
    let todo: Vec<(i64, String, i64, String)> = {
        let mut st = conn.prepare(
            "SELECT id, path, kind, ext FROM media
             WHERE missing=0 AND deleted IS NULL AND state=1 AND kind <> 1 AND facedone=0
             ORDER BY id DESC",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?
    };
    let total = todo.len() as u64;
    if total == 0 {
        return Ok(());
    }
    {
        let mut j = app.job.write();
        j.total = total;
        j.done = 0;
    }

    let (tx, rx) = mpsc::sync_channel::<(i64, Vec<(Detection, Vec<f32>)>)>(32);
    let job = app.job.clone();
    let writer = {
        let conn2 = app.index.standalone()?;
        std::thread::spawn(move || -> Result<()> {
            let start = Instant::now();
            let mut done = 0u64;
            while let Ok((id, found)) = rx.recv() {
                let tx = conn2.unchecked_transaction()?;
                {
                    tx.execute("UPDATE media SET facedone=1 WHERE id=?1", [id])?;
                    // Replace, never append: re-scanning a photo otherwise
                    // duplicates every face it contains.
                    tx.execute("DELETE FROM faces WHERE media=?1", [id])?;
                    let mut ins = tx.prepare_cached(
                        "INSERT INTO faces(media,x,y,w,h,score,vec) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    )?;
                    for (d, v) in &found {
                        ins.execute(params![
                            id,
                            d.bbox[0],
                            d.bbox[1],
                            d.bbox[2] - d.bbox[0],
                            d.bbox[3] - d.bbox[1],
                            d.score,
                            to_blob(v)
                        ])?;
                    }
                }
                tx.commit()?;
                done += 1;
                if done % 8 == 0 {
                    let secs = start.elapsed().as_secs_f64().max(0.001);
                    let rate = done as f64 / secs;
                    let mut j = job.write();
                    j.done = done;
                    j.rate = rate;
                    j.eta = if rate > 0.0 { (total - done) as f64 / rate } else { 0.0 };
                }
            }
            Ok(())
        })
    };

    let label = app.job.clone();
    todo.par_iter().for_each_with(tx, |tx, (id, path, kind, ext)| {
        let Ok(img) = crate::decode::load(Path::new(path), *kind, ext, 1280) else { return };
        let Ok(dets) = engine.detect(&img) else { return };
        let mut found = Vec::new();
        for d in dets {
            // Tiny faces embed badly and pollute the clusters.
            if (d.bbox[2] - d.bbox[0]) < 32.0 {
                continue;
            }
            if let Ok(v) = engine.embed(&img, &d) {
                found.push((d, v));
            }
        }
        {
            let mut j = label.write();
            if j.done % 8 == 0 {
                j.label = Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            }
        }
        let _ = tx.send((*id, found));
    });

    writer.join().map_err(|_| anyhow!("face writer panicked"))??;
    Ok(())
}

/// Greedy agglomerative clustering on cosine similarity. ArcFace embeddings for
/// the same person sit around 0.5-0.8 apart and different people below 0.3, so
/// a single threshold is enough and avoids tuning a density parameter.
pub fn cluster(app: &Arc<App>) -> Result<()> {
    const THRESH: f32 = 0.42;
    let conn = app.index.standalone()?;

    let rows: Vec<(i64, i64, Vec<f32>)> = {
        let mut st = conn.prepare("SELECT id, media, vec FROM faces ORDER BY id")?;
        st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, from_blob(&r.get::<_, Vec<u8>>(2)?)))
        })?
        .collect::<Result<_, _>>()?
    };
    if rows.is_empty() {
        return Ok(());
    }

    let mut centroids: Vec<Vec<f32>> = Vec::new();
    let mut counts: Vec<f32> = Vec::new();
    let mut assign: Vec<(i64, usize)> = Vec::with_capacity(rows.len());

    for (fid, _, v) in &rows {
        let mut best = (usize::MAX, THRESH);
        for (i, c) in centroids.iter().enumerate() {
            let s = dot(v, c);
            if s > best.1 {
                best = (i, s);
            }
        }
        if best.0 == usize::MAX {
            centroids.push(v.clone());
            counts.push(1.0);
            assign.push((*fid, centroids.len() - 1));
        } else {
            let i = best.0;
            let n = counts[i];
            for k in 0..centroids[i].len() {
                centroids[i][k] = (centroids[i][k] * n + v[k]) / (n + 1.0);
            }
            centroids[i] = normalize(std::mem::take(&mut centroids[i]));
            counts[i] = n + 1.0;
            assign.push((*fid, i));
        }
    }

    // A cluster of one is usually a false positive or a passer-by; keep it out
    // of the People list rather than filling the page with strangers.
    let keep: std::collections::HashSet<usize> =
        (0..counts.len()).filter(|i| counts[*i] >= 2.0).collect();

    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE faces SET cluster = NULL", [])?;
    // Names already given are matched back by their most representative face.
    let existing: Vec<(i64, String)> = {
        let mut st = tx.prepare("SELECT id, name FROM people WHERE name <> ''")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
    };
    let old_names: std::collections::HashMap<i64, String> = existing.into_iter().collect();
    tx.execute("DELETE FROM people", [])?;

    let mut cluster_to_person = std::collections::HashMap::new();
    for i in keep.iter().copied() {
        let pid = i as i64 + 1;
        let name = old_names.get(&pid).cloned().unwrap_or_default();
        tx.execute("INSERT INTO people(id, name) VALUES(?1, ?2)", params![pid, name])?;
        cluster_to_person.insert(i, pid);
    }
    {
        let mut up = tx.prepare_cached("UPDATE faces SET cluster=?2 WHERE id=?1")?;
        for (fid, ci) in &assign {
            if let Some(pid) = cluster_to_person.get(ci) {
                up.execute(params![fid, pid])?;
            }
        }
    }
    // Give every person a cover: the highest-scoring face in the group.
    tx.execute(
        "UPDATE people SET cover = (
           SELECT f.id FROM faces f WHERE f.cluster = people.id ORDER BY f.score DESC LIMIT 1)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Crop a stored face out of its photo, for the People page.
pub fn face_thumb(app: &Arc<App>, face_id: i64) -> Result<Vec<u8>> {
    let (media, x, y, w, h): (i64, f64, f64, f64, f64) = {
        let c = app.index.get()?;
        c.query_row("SELECT media,x,y,w,h FROM faces WHERE id=?1", [face_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
    };
    let (path, kind, ext): (String, i64, String) = {
        let c = app.index.get()?;
        c.query_row("SELECT path, kind, ext FROM media WHERE id=?1", [media], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
    };
    let img = crate::decode::load(Path::new(&path), kind, &ext, 1280)?;
    // A little air around the crop reads much better than a tight box.
    let pad = (w.max(h) * 0.32) as i64;
    let x0 = ((x as i64 - pad).max(0)) as u32;
    let y0 = ((y as i64 - pad).max(0)) as u32;
    let x1 = (((x + w) as i64 + pad) as u32).min(img.width());
    let y1 = (((y + h) as i64 + pad) as u32).min(img.height());
    let crop = img.crop_imm(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1));
    let sq = crate::decode::square_thumb(&crop, 192);
    crate::decode::encode_jpeg(&sq, 82)
}

// ------------------------------------------------------------ vocabulary ---

/// Concepts the auto-tagger can assign. Deliberately concrete and everyday —
/// CLIP scores abstract words highly against almost anything, which produces
/// tags that look clever and help nobody find a picture.
pub const VOCAB: &[&str] = &[
    // places and scenes
    "a beach", "a lake", "a river", "the ocean", "a waterfall", "a forest", "a field",
    "a mountain", "a desert", "a city street", "a park", "a garden", "a backyard",
    "a kitchen", "a living room", "a bedroom", "an office", "a restaurant", "a bar",
    "a cafe", "a museum", "a church", "a stadium", "a concert", "a market", "a shop",
    "a hotel room", "a swimming pool", "a campsite", "a cabin", "a farm", "a bridge",
    "a road", "a parking lot", "an airport", "a train station", "a harbour",
    // water and boats
    "a boat", "boats", "a sailboat", "a canoe", "a kayak", "a pedal boat", "a ship",
    "a dock", "a pier", "a marina",
    // vehicles
    "a car", "a truck", "a motorcycle", "a bicycle", "a bus", "a train", "an airplane",
    "a helicopter",
    // people
    "a group of people", "a person", "a portrait of a person", "a child", "a baby",
    "a family", "a couple", "a crowd", "a wedding", "a birthday party", "a graduation",
    "people dancing", "people eating", "a selfie", "a person smiling",
    // animals
    "a dog", "a cat", "a bird", "a horse", "a cow", "a sheep", "a duck", "a swan",
    "a fish", "a deer", "a squirrel", "a butterfly", "an insect",
    // food
    "food", "a meal", "breakfast", "a cake", "pizza", "a sandwich", "a salad",
    "a barbecue", "a drink", "coffee", "beer", "wine", "a cocktail", "ice cream",
    // nature and weather
    "a sunset", "a sunrise", "the night sky", "stars", "the moon", "clouds", "fog",
    "rain", "snow", "ice", "autumn leaves", "flowers", "a tree", "grass", "sand",
    "rocks", "a rainbow", "lightning",
    // activities
    "hiking", "camping", "fishing", "swimming", "surfing", "skiing", "snowboarding",
    "running", "cycling", "playing football", "playing basketball", "playing tennis",
    "golf", "playing a video game", "playing a musical instrument", "reading a book",
    "cooking", "shopping", "working at a computer", "a meeting",
    // objects
    "a computer", "a phone", "a camera", "a television", "a book", "a painting",
    "a poster", "furniture", "a chair", "a table", "a bed", "a lamp", "clothing",
    "shoes", "a bag", "jewellery", "a watch", "a toy", "a plant", "a candle",
    "fireworks", "a christmas tree", "a birthday cake", "balloons", "a sign",
    // image kinds
    "a screenshot", "a document", "a receipt", "a whiteboard", "a map", "a chart",
    "text on a screen", "a meme", "a drawing", "a diagram", "a logo",
    "a black and white photo", "a close-up", "an aerial view", "a panorama",
    "a blurry photo", "a dark photo",
    // architecture
    "a house", "a building", "a skyscraper", "a bridge at night", "a window",
    "a door", "stairs", "a fence", "a roof", "an interior",
];


// ------------------------------------------------------------- worker ------

fn flag(app: &Arc<App>, key: &str) -> bool {
    app.index
        .get()
        .ok()
        .and_then(|c| db::get_setting(&c, key))
        .map(|v| v.trim_matches('"') == "true")
        .unwrap_or(false)
}

fn pending(app: &Arc<App>, sql: &str) -> i64 {
    app.index
        .get()
        .ok()
        .and_then(|c| c.query_row(sql, [], |r| r.get::<_, i64>(0)).ok())
        .unwrap_or(0)
}

const CLIP_PENDING: &str = "SELECT COUNT(*) FROM media
     WHERE missing=0 AND deleted IS NULL AND state=1 AND id NOT IN (SELECT media FROM clip)";
const FACE_PENDING: &str = "SELECT COUNT(*) FROM media
     WHERE missing=0 AND deleted IS NULL AND state=1 AND kind<>1 AND facedone=0";

/// Keep understanding current without anyone asking.
///
/// Whatever brings new photos in — adding a folder, a phone upload, a rescan
/// finding files that appeared on disk — this notices the backlog and works
/// through it. It only ever runs while nothing else is, so it never competes
/// with indexing or a recompression job for the disk.
pub fn spawn_worker(app: Arc<App>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(4));

        // Never start on top of a scan, a thumbnail pass or a bulk job.
        if app.job.read().running {
            continue;
        }
        if app.jobs.read().task.as_ref().map(|t| t.running).unwrap_or(false) {
            continue;
        }

        // A scan that was deferred while the disk was busy runs first: there is
        // no point understanding photos that have not been indexed yet.
        if app.rescan_pending.swap(false, std::sync::atomic::Ordering::Relaxed) {
            crate::scan::spawn(app.clone(), None, false);
            continue;
        }

        if flag(&app, "clip_on") && clip_installed(&app) && pending(&app, CLIP_PENDING) > 0 {
            if let Err(e) = clip_pass(&app) {
                eprintln!("[ml] background tagging: {e:#}");
                // Back off rather than retry a failing model in a tight loop.
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
            let mut j = app.job.write();
            j.running = false;
            j.phase = "idle".into();
            continue;
        }

        if flag(&app, "faces_on") && faces_installed(&app) && pending(&app, FACE_PENDING) > 0 {
            let r = face_pass(&app);
            {
                let mut j = app.job.write();
                j.running = false;
                j.phase = "idle".into();
            }
            match r {
                // New faces only belong to people once they have been grouped.
                Ok(()) => {
                    if let Err(e) = cluster(&app) {
                        eprintln!("[ml] background clustering: {e:#}");
                    }
                }
                Err(e) => {
                    eprintln!("[ml] background faces: {e:#}");
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
        }
    });
}
