//! Catalog: folder scan + SQLite index + JSON sidecars.
//! Folder is source of truth for per-file state (rating/label/flag/keywords/
//! recipes live in `.araware*.json` sidecars); the DB is a search/cache index
//! plus the home of library organization (stacks, collections).
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::decode;
use crate::recipe::{sidecar_path_for, sidecar_path_for_v, Sidecar};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetEntry {
    /// real file path on disk (variants share the master's path)
    pub path: String,
    pub name: String,
    /// "raw" | "raster"
    pub kind: String,
    pub size: u64,
    pub mtime: i64,
    pub rating: i32,
    pub label: String,
    /// -1 rejected, 0 none, 1 picked
    pub flag: i32,
    pub keywords: Vec<String>,
    /// virtual copy slot: 0 = master, n = <stem>.araware.v{n}.json
    pub vslot: u32,
    /// stack id (0 = unstacked) and position inside it
    pub stack: i64,
    pub stack_seq: i32,
    /// path of the sibling file (raw<->jpeg pair) if present
    pub pair: Option<String>,
    pub has_sidecar: bool,
    /// camera "make model" + lens from header/EXIF (empty when unknown)
    pub camera: String,
    pub lens: String,
    /// capture metadata
    pub ctime: i64,
    pub iso: f32,
    pub aperture: f32,
    pub focal: f32,
    pub shutter: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionInfo {
    pub id: i64,
    pub name: String,
    /// 0 = manual item list, 1 = rules-evaluated smart collection
    pub smart: i32,
    pub rules: String,
    pub count: u32,
}

pub struct Catalog {
    db: Connection,
}

fn db_path() -> PathBuf {
    let base = std::env::var_os("ARAWARE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".araware")
        });
    let _ = fs::create_dir_all(&base);
    base.join("catalog.db")
}

pub fn thumbs_dir() -> PathBuf {
    let base = db_path().parent().map(Path::to_path_buf).unwrap_or_default();
    let d = base.join("thumbs");
    let _ = fs::create_dir_all(&d);
    d
}

const BASE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS files(
       path TEXT PRIMARY KEY, name TEXT, folder TEXT, kind TEXT,
       size INTEGER, mtime INTEGER, rating INTEGER, label TEXT,
       pair TEXT, added INTEGER);
     CREATE TABLE IF NOT EXISTS collections(
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       name TEXT NOT NULL, smart INTEGER NOT NULL DEFAULT 0,
       rules TEXT NOT NULL DEFAULT '');
     CREATE TABLE IF NOT EXISTS collection_items(
       coll_id INTEGER NOT NULL, ref TEXT NOT NULL,
       UNIQUE(coll_id, ref));
     CREATE INDEX IF NOT EXISTS idx_files_folder ON files(folder);
     CREATE INDEX IF NOT EXISTS idx_files_rating ON files(rating);
     CREATE INDEX IF NOT EXISTS idx_items_coll ON collection_items(coll_id);";

/// columns added after v1 — ALTER ADD COLUMN errors are ignored when the
/// column already exists
const MIGRATED_COLS: &[&str] = &[
    "ALTER TABLE files ADD COLUMN flag INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN keywords TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE files ADD COLUMN stack INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN stack_seq INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN ctime INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN iso REAL NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN aperture REAL NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN focal REAL NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN shutter REAL NOT NULL DEFAULT 0",
    "ALTER TABLE files ADD COLUMN camera TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE files ADD COLUMN lens TEXT NOT NULL DEFAULT ''",
];

impl Catalog {
    pub fn open() -> Result<Catalog> {
        let db = Connection::open(db_path()).context("open catalog db")?;
        db.execute_batch(BASE_SCHEMA)?;
        for m in MIGRATED_COLS {
            let _ = db.execute_batch(m);
        }
        Ok(Catalog { db })
    }

    /// in-memory catalog (tests)
    #[cfg(test)]
    pub fn open_mem() -> Result<Catalog> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE files(path TEXT PRIMARY KEY, name TEXT, folder TEXT, kind TEXT,
             size INTEGER NOT NULL DEFAULT 0, mtime INTEGER NOT NULL DEFAULT 0,
             rating INTEGER NOT NULL DEFAULT 0, label TEXT NOT NULL DEFAULT '',
             flag INTEGER NOT NULL DEFAULT 0, keywords TEXT NOT NULL DEFAULT '[]',
             stack INTEGER NOT NULL DEFAULT 0, stack_seq INTEGER NOT NULL DEFAULT 0,
             ctime INTEGER NOT NULL DEFAULT 0,
             iso REAL NOT NULL DEFAULT 0, aperture REAL NOT NULL DEFAULT 0,
             focal REAL NOT NULL DEFAULT 0, shutter REAL NOT NULL DEFAULT 0,
             camera TEXT NOT NULL DEFAULT '', lens TEXT NOT NULL DEFAULT '',
             pair TEXT, added INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE collections(id INTEGER PRIMARY KEY AUTOINCREMENT,
               name TEXT, smart INTEGER, rules TEXT);
             CREATE TABLE collection_items(coll_id INTEGER, ref TEXT,
               UNIQUE(coll_id, ref));",
        )?;
        Ok(Catalog { db })
    }

    /// Scan one directory level for supported assets, pairing RAW+JPEG by
    /// stem and expanding virtual copies (stem.araware.v*.json sidecars).
    pub fn scan(&self, folder: &Path) -> Result<Vec<AssetEntry>> {
        let mut raws: Vec<PathBuf> = Vec::new();
        let mut rasters: Vec<PathBuf> = Vec::new();
        for e in fs::read_dir(folder).with_context(|| format!("read {}", folder.display()))? {
            let p = e?.path();
            if !p.is_file() {
                continue;
            }
            if decode::is_raw(&p) {
                raws.push(p);
            } else if decode::is_raster(&p) {
                rasters.push(p);
            }
        }
        let mut entries: Vec<AssetEntry> = Vec::new();
        let raster_stems: HashMap<String, PathBuf> = rasters
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(|s| (s.to_string(), p.clone())))
            .collect();
        let raw_stems: HashSet<String> = raws
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
            .collect();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // stack ids assigned by the catalog (survive rescans)
        let mut stacks: HashMap<String, (i64, i32)> = HashMap::new();
        {
            let mut st = self.db.prepare("SELECT path, stack, stack_seq FROM files")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            for r in rows.flatten() {
                stacks.insert(r.0, (r.1, r.2));
            }
        }

        let mut push = |p: &PathBuf, kind: &str, pair: Option<String>| {
            let meta = fs::metadata(p).ok();
            let sc_path = sidecar_path_for(p);
            let sc = read_sidecar(&sc_path).unwrap_or_default();
            let has_sc = sc_path.exists();
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let ci = decode::probe(p);
            let path_s = p.to_string_lossy().into_owned();
            let (stack, stack_seq) = stacks.get(&path_s).copied().unwrap_or((0, 0));
            let entry = AssetEntry {
                path: path_s,
                name,
                kind: kind.to_string(),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                mtime: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                rating: sc.rating,
                label: sc.label.clone(),
                flag: sc.flag,
                keywords: sc.keywords.clone(),
                vslot: 0,
                stack,
                stack_seq,
                pair,
                has_sidecar: has_sc,
                camera: ci
                    .as_ref()
                    .map(|i| format!("{} {}", i.make, i.model).trim().to_string())
                    .unwrap_or_default(),
                lens: ci.as_ref().map(|i| i.lens.clone()).unwrap_or_default(),
                ctime: ci.as_ref().map(|i| i.timestamp).unwrap_or(0),
                iso: ci.as_ref().map(|i| i.iso).unwrap_or(0.0),
                aperture: ci.as_ref().map(|i| i.aperture).unwrap_or(0.0),
                focal: ci.as_ref().map(|i| i.focal).unwrap_or(0.0),
                shutter: ci.as_ref().map(|i| i.shutter).unwrap_or(0.0),
            };
            let _ = self.db.execute(
                "INSERT INTO files(path,name,folder,kind,size,mtime,rating,label,flag,
                    keywords,stack,stack_seq,ctime,iso,aperture,focal,shutter,
                    camera,lens,pair,added)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
                 ON CONFLICT(path) DO UPDATE SET
                   name=excluded.name,size=excluded.size,mtime=excluded.mtime,
                   rating=excluded.rating,label=excluded.label,flag=excluded.flag,
                   keywords=excluded.keywords,ctime=excluded.ctime,iso=excluded.iso,
                   aperture=excluded.aperture,focal=excluded.focal,
                   shutter=excluded.shutter,camera=excluded.camera,lens=excluded.lens,
                   pair=excluded.pair",
                params![
                    entry.path,
                    entry.name,
                    folder.to_string_lossy(),
                    entry.kind,
                    entry.size as i64,
                    entry.mtime,
                    entry.rating,
                    entry.label,
                    entry.flag,
                    serde_json::to_string(&entry.keywords).unwrap_or_default(),
                    stack,
                    stack_seq,
                    entry.ctime,
                    entry.iso,
                    entry.aperture,
                    entry.focal,
                    entry.shutter,
                    entry.camera,
                    entry.lens,
                    entry.pair,
                    now
                ],
            );
            entries.push(entry);
        };

        for p in &raws {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let pair = raster_stems.get(&stem).map(|q| q.to_string_lossy().into_owned());
            push(p, "raw", pair);
        }
        for p in &rasters {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if raw_stems.contains(&stem) {
                continue; // shown via raw entry
            }
            push(p, "raster", None);
        }
        expand_variants(&mut entries);
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// DB snapshot of a folder without rescanning (fast open / collections).
    /// Stack ids come from the files table; virtual copies are probed from
    /// their sidecars exactly like scan() does, so the result mirrors scan.
    pub fn assets(&self, folder: &Path) -> Result<Vec<AssetEntry>> {
        let mut st = self.db.prepare(
            "SELECT path,name,kind,size,mtime,rating,label,flag,keywords,
                    stack,stack_seq,pair,ctime,iso,aperture,focal,shutter,camera,lens
             FROM files WHERE folder=?1 ORDER BY name",
        )?;
        let f = folder.to_string_lossy();
        let rows = st.query_map(params![f], |r| {
            let kw: String = r.get(8).unwrap_or_default();
            Ok(AssetEntry {
                path: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime: r.get(4)?,
                rating: r.get(5)?,
                label: r.get(6)?,
                flag: r.get(7)?,
                keywords: serde_json::from_str(&kw).unwrap_or_default(),
                vslot: 0,
                stack: r.get(9)?,
                stack_seq: r.get(10)?,
                pair: r.get(11)?,
                has_sidecar: true,
                ctime: r.get(12)?,
                iso: r.get(13)?,
                aperture: r.get(14)?,
                focal: r.get(15)?,
                shutter: r.get(16)?,
                camera: r.get(17)?,
                lens: r.get(18)?,
            })
        })?;
        let mut out: Vec<AssetEntry> = rows.flatten().collect();
        // variants live only in sidecars, not the files table — probe for
        // them here too so snapshots match what scan() would return
        expand_variants(&mut out);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// every folder ever scanned (for the library sidebar)
    pub fn folders(&self) -> Result<Vec<String>> {
        let mut st = self.db.prepare("SELECT DISTINCT folder FROM files ORDER BY folder")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.flatten().collect())
    }

    fn touch_row(&self, asset: &Path, set: &str, v: &dyn rusqlite::ToSql) {
        // keep the DB mirror in sync when a sidecar field changes
        let sql = format!("UPDATE files SET {set}=?1 WHERE path=?2");
        let p = asset.to_string_lossy().into_owned();
        let _ = self.db.execute(&sql, rusqlite::params_from_iter(
            [v as &dyn rusqlite::ToSql, &p as &dyn rusqlite::ToSql]));
    }

    pub fn set_rating(&self, asset: &Path, rating: i32) -> Result<()> {
        let sp = sidecar_path_for(asset);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.rating = rating.clamp(0, 5);
        write_sidecar(&sp, &sc)?;
        self.touch_row(asset, "rating", &sc.rating);
        Ok(())
    }

    pub fn set_label(&self, asset: &Path, label: &str) -> Result<()> {
        let sp = sidecar_path_for(asset);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.label = label.to_string();
        write_sidecar(&sp, &sc)?;
        self.touch_row(asset, "label", &sc.label);
        Ok(())
    }

    pub fn set_flag(&self, asset: &Path, flag: i32) -> Result<()> {
        let sp = sidecar_path_for(asset);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.flag = flag.clamp(-1, 1);
        write_sidecar(&sp, &sc)?;
        self.touch_row(asset, "flag", &sc.flag);
        Ok(())
    }

    pub fn set_keywords(&self, asset: &Path, keywords: &[String]) -> Result<()> {
        let sp = sidecar_path_for(asset);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.keywords = keywords.to_vec();
        write_sidecar(&sp, &sc)?;
        self.touch_row(asset, "keywords", &serde_json::to_string(&sc.keywords).unwrap_or_default());
        Ok(())
    }

    /// per-variant sidecar state (rating/label/flag/keywords + recipe)
    pub fn set_flag_v(&self, asset: &Path, vslot: u32, flag: i32) -> Result<()> {
        let sp = sidecar_path_for_v(asset, vslot);
        let mut sc = read_sidecar(&sp).unwrap_or_default();
        sc.flag = flag.clamp(-1, 1);
        write_sidecar(&sp, &sc)
    }

    // ---- stacks ------------------------------------------------------

    /// group paths into a new stack; returns the stack id
    pub fn stack_group(&self, paths: &[String]) -> Result<i64> {
        let next: i64 = self
            .db
            .query_row("SELECT COALESCE(MAX(stack),0)+1 FROM files", [], |r| r.get(0))
            .unwrap_or(1);
        for (i, p) in paths.iter().enumerate() {
            let _ = self.db.execute(
                "UPDATE files SET stack=?1, stack_seq=?2 WHERE path=?3",
                params![next, i as i32, p],
            );
        }
        Ok(next)
    }

    pub fn stack_ungroup(&self, stack: i64) -> Result<()> {
        self.db.execute(
            "UPDATE files SET stack=0, stack_seq=0 WHERE stack=?1",
            params![stack],
        )?;
        Ok(())
    }

    /// move `path` to the front (cover) of its stack
    pub fn stack_cover(&self, path: &str) -> Result<()> {
        let _ = self.db.execute(
            "UPDATE files SET stack_seq=stack_seq+1 WHERE stack=(SELECT stack FROM files WHERE path=?1)",
            params![path],
        );
        self.db.execute(
            "UPDATE files SET stack_seq=0 WHERE path=?1",
            params![path],
        )?;
        Ok(())
    }

    // ---- virtual copies ----------------------------------------------

    /// create `<stem>.araware.v{n}.json` copying `src_vslot`'s sidecar;
    /// returns the new slot number
    pub fn variant_create(&self, asset: &Path, src_vslot: u32) -> Result<u32> {
        let mut n = 1u32;
        while sidecar_path_for_v(asset, n).exists() {
            n += 1;
            if n > 64 {
                anyhow::bail!("too many virtual copies");
            }
        }
        let src = read_sidecar(&sidecar_path_for_v(asset, src_vslot)).unwrap_or_default();
        write_sidecar(&sidecar_path_for_v(asset, n), &src)?;
        Ok(n)
    }

    pub fn variant_delete(&self, asset: &Path, vslot: u32) -> Result<()> {
        if vslot == 0 {
            anyhow::bail!("cannot delete master");
        }
        let p = sidecar_path_for_v(asset, vslot);
        if p.exists() {
            fs::remove_file(&p)?;
        }
        Ok(())
    }

    /// copy a variant's recipe into the master sidecar (keep master's
    /// rating/flag/keywords)
    pub fn variant_promote(&self, asset: &Path, vslot: u32) -> Result<()> {
        let vsc = read_sidecar(&sidecar_path_for_v(asset, vslot))?;
        let mp = sidecar_path_for(asset);
        let mut msc = read_sidecar(&mp).unwrap_or_default();
        msc.recipe = vsc.recipe;
        msc.versions = vsc.versions;
        write_sidecar(&mp, &msc)
    }

    // ---- collections ---------------------------------------------------

    pub fn collections(&self) -> Result<Vec<CollectionInfo>> {
        let mut st = self.db.prepare(
            "SELECT c.id, c.name, c.smart, c.rules,
                    (SELECT COUNT(*) FROM collection_items i WHERE i.coll_id=c.id)
             FROM collections c ORDER BY c.name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(CollectionInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                smart: r.get::<_, i32>(2)?,
                rules: r.get(3)?,
                count: r.get::<_, i64>(4)? as u32,
            })
        })?;
        let mut out: Vec<CollectionInfo> = rows.flatten().collect();
        // smart collections report their live match count
        for c in out.iter_mut() {
            if c.smart == 1 {
                c.count = self.smart_eval(&c.rules).map(|v| v.len() as u32).unwrap_or(0);
            }
        }
        Ok(out)
    }

    pub fn collection_add(&self, name: &str, smart: bool, rules: &str) -> Result<i64> {
        self.db.execute(
            "INSERT INTO collections(name,smart,rules) VALUES(?1,?2,?3)",
            params![name, if smart { 1 } else { 0 }, rules],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn collection_rename(&self, id: i64, name: &str) -> Result<()> {
        self.db
            .execute("UPDATE collections SET name=?1 WHERE id=?2", params![name, id])?;
        Ok(())
    }

    pub fn collection_set_rules(&self, id: i64, rules: &str) -> Result<()> {
        self.db
            .execute("UPDATE collections SET rules=?1 WHERE id=?2", params![rules, id])?;
        Ok(())
    }

    pub fn collection_delete(&self, id: i64) -> Result<()> {
        self.db
            .execute("DELETE FROM collection_items WHERE coll_id=?1", params![id])?;
        self.db.execute("DELETE FROM collections WHERE id=?1", params![id])?;
        Ok(())
    }

    /// item refs: "path" or "path#v<slot>"
    pub fn collection_set_items(&self, id: i64, refs: &[String]) -> Result<()> {
        self.db
            .execute("DELETE FROM collection_items WHERE coll_id=?1", params![id])?;
        for r in refs {
            let _ = self.db.execute(
                "INSERT OR IGNORE INTO collection_items(coll_id,ref) VALUES(?1,?2)",
                params![id, r],
            );
        }
        Ok(())
    }

    pub fn collection_add_items(&self, id: i64, refs: &[String]) -> Result<()> {
        for r in refs {
            let _ = self.db.execute(
                "INSERT OR IGNORE INTO collection_items(coll_id,ref) VALUES(?1,?2)",
                params![id, r],
            );
        }
        Ok(())
    }

    pub fn collection_remove_items(&self, id: i64, refs: &[String]) -> Result<()> {
        for r in refs {
            let _ = self.db.execute(
                "DELETE FROM collection_items WHERE coll_id=?1 AND ref=?2",
                params![id, r],
            );
        }
        Ok(())
    }

    pub fn collection_items(&self, id: i64) -> Result<Vec<String>> {
        let mut st = self
            .db
            .prepare("SELECT ref FROM collection_items WHERE coll_id=?1")?;
        let rows = st.query_map(params![id], |r| r.get(0))?;
        Ok(rows.flatten().collect())
    }

    /// evaluate smart-collection rules against the files table.
    /// rules JSON: {rating_min, flag:[-1,0,1], label:"", camera_contains:"",
    ///              lens_contains:"", keyword:"", name_contains:"", edited:true}
    pub fn smart_eval(&self, rules: &str) -> Result<Vec<AssetEntry>> {
        let r: Value = serde_json::from_str(rules).unwrap_or(json!({}));
        // two phases: SQL pre-filters masters on columns variants share
        // (camera/lens/name), then per-entry rules (rating/flag/label/
        // keyword/edited) run in Rust so virtual copies can match even
        // when their master does not.
        let mut sql = String::from(
            "SELECT path,name,kind,size,mtime,rating,label,flag,keywords,
                    stack,stack_seq,pair,ctime,iso,aperture,focal,shutter,camera,lens,
                    folder
             FROM files WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for (key, col) in [("camera_contains", "camera"), ("lens_contains", "lens"), ("name_contains", "name")] {
            if let Some(s) = r.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    sql.push_str(&format!(" AND {col} LIKE ?"));
                    args.push(Box::new(format!("%{s}%")));
                }
            }
        }
        sql.push_str(" ORDER BY name");
        let mut st = self.db.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let rows = st.query_map(&*arg_refs, |r| {
            let kw: String = r.get(8).unwrap_or_default();
            Ok(AssetEntry {
                path: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime: r.get(4)?,
                rating: r.get(5)?,
                label: r.get(6)?,
                flag: r.get(7)?,
                keywords: serde_json::from_str(&kw).unwrap_or_default(),
                vslot: 0,
                stack: r.get(9)?,
                stack_seq: r.get(10)?,
                pair: r.get(11)?,
                has_sidecar: true,
                ctime: r.get(12)?,
                iso: r.get(13)?,
                aperture: r.get(14)?,
                focal: r.get(15)?,
                shutter: r.get(16)?,
                camera: r.get(17)?,
                lens: r.get(18)?,
            })
        })?;
        let mut entries: Vec<AssetEntry> = rows.flatten().collect();
        expand_variants(&mut entries);
        let rating_min = r.get("rating_min").and_then(|v| v.as_i64()).unwrap_or(0);
        let rating_eq = r.get("rating_eq").and_then(|v| v.as_i64());
        let flag_eq = r.get("flag").and_then(|v| v.as_i64());
        let label_eq = r.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let keyword = r.get("keyword").and_then(|v| v.as_str()).unwrap_or("");
        let edited = r.get("edited").and_then(|v| v.as_bool()).unwrap_or(false);
        entries.retain(|e| {
            if rating_min > 0 && (e.rating as i64) < rating_min {
                return false;
            }
            if let Some(v) = rating_eq {
                if e.rating as i64 != v {
                    return false;
                }
            }
            if let Some(v) = flag_eq {
                if e.flag as i64 != v {
                    return false;
                }
            }
            if !label_eq.is_empty() && e.label != label_eq {
                return false;
            }
            if !keyword.is_empty() && !e.keywords.iter().any(|k| k == keyword) {
                return false;
            }
            if edited && e.rating == 0 && e.label.is_empty() && e.flag == 0
                && e.keywords.is_empty()
            {
                return false;
            }
            true
        });
        Ok(entries)
    }
}

/// Append virtual-copy rows for `<stem>.araware.v{n}.json` siblings (v1..v64).
/// Variants are never stored in the files table — they exist as sidecars on
/// disk, so both scan() and assets() probe for them identically.
fn expand_variants(entries: &mut Vec<AssetEntry>) {
    let mut variants: Vec<AssetEntry> = Vec::new();
    for e in entries.iter() {
        for n in 1..=64u32 {
            let vp = sidecar_path_for_v(Path::new(&e.path), n);
            if !vp.exists() {
                break;
            }
            let sc = read_sidecar(&vp).unwrap_or_default();
            variants.push(AssetEntry {
                path: e.path.clone(),
                name: format!("{} v{}", e.name, n),
                kind: e.kind.clone(),
                size: e.size,
                mtime: e.mtime,
                rating: sc.rating,
                label: sc.label.clone(),
                flag: sc.flag,
                keywords: sc.keywords.clone(),
                vslot: n,
                stack: 0,
                stack_seq: 0,
                pair: e.pair.clone(),
                has_sidecar: true,
                camera: e.camera.clone(),
                lens: e.lens.clone(),
                ctime: e.ctime,
                iso: e.iso,
                aperture: e.aperture,
                focal: e.focal,
                shutter: e.shutter,
            });
        }
    }
    entries.extend(variants);
}

pub fn read_sidecar(sc_path: &Path) -> Result<Sidecar> {
    let s = fs::read_to_string(sc_path)?;
    serde_json::from_str(&s).context("parse sidecar")
}

pub fn write_sidecar(sc_path: &Path, sc: &Sidecar) -> Result<()> {
    let tmp = sc_path.with_extension("araware.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(sc)?)?;
    fs::rename(&tmp, sc_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ara_cat_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn flag_keywords_sidecar_roundtrip() {
        let dir = tmpdir();
        let raw = dir.join("a.nef");
        fs::File::create(&raw).unwrap().write_all(b"x").unwrap();
        let c = Catalog::open_mem().unwrap();
        c.set_flag(&raw, 1).unwrap();
        c.set_keywords(&raw, &["sunset".into(), "test".into()]).unwrap();
        let sc = read_sidecar(&sidecar_path_for(&raw)).unwrap();
        assert_eq!(sc.flag, 1);
        assert_eq!(sc.keywords, vec!["sunset", "test"]);
        c.set_flag(&raw, -1).unwrap();
        let sc = read_sidecar(&sidecar_path_for(&raw)).unwrap();
        assert_eq!(sc.flag, -1);
    }

    #[test]
    fn stacks_group_cover_ungroup() {
        let c = Catalog::open_mem().unwrap();
        for p in ["a", "b", "c"] {
            c.db.execute(
                "INSERT INTO files(path,name,folder,kind,stack,stack_seq) VALUES(?1,?1,'f','raw',0,0)",
                params![p],
            ).unwrap();
        }
        let id = c.stack_group(&["a".into(), "b".into()]).unwrap();
        assert!(id > 0);
        c.stack_cover("b").unwrap();
        let seq: i32 = c.db
            .query_row("SELECT stack_seq FROM files WHERE path='b'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seq, 0);
        c.stack_ungroup(id).unwrap();
        let s: i64 = c.db
            .query_row("SELECT stack FROM files WHERE path='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(s, 0);
    }

    #[test]
    fn variants_create_promote_delete() {
        let dir = tmpdir();
        let raw = dir.join("b.cr2");
        fs::File::create(&raw).unwrap().write_all(b"x").unwrap();
        let c = Catalog::open_mem().unwrap();
        // master gets a rating; the variant inherits it
        c.set_rating(&raw, 4).unwrap();
        let n = c.variant_create(&raw, 0).unwrap();
        assert_eq!(n, 1);
        assert!(sidecar_path_for_v(&raw, 1).exists());
        let n2 = c.variant_create(&raw, 0).unwrap();
        assert_eq!(n2, 2);
        // promote v1: recipe from variant flows into the master sidecar
        let mut vsc = read_sidecar(&sidecar_path_for_v(&raw, 1)).unwrap();
        vsc.recipe.exposure = 1.5;
        write_sidecar(&sidecar_path_for_v(&raw, 1), &vsc).unwrap();
        c.variant_promote(&raw, 1).unwrap();
        let m = read_sidecar(&sidecar_path_for(&raw)).unwrap();
        assert!((m.recipe.exposure - 1.5).abs() < 1e-6);
        assert_eq!(m.rating, 4); // master rating preserved
        c.variant_delete(&raw, 1).unwrap();
        assert!(!sidecar_path_for_v(&raw, 1).exists());
        assert!(c.variant_delete(&raw, 0).is_err()); // master is protected
    }

    #[test]
    fn collections_and_smart() {
        let c = Catalog::open_mem().unwrap();
        c.db.execute_batch(
            "INSERT INTO files(path,name,folder,kind,rating,label,flag,keywords,camera,lens)
             VALUES('a','a','f','raw',5,'red',1,'[\"sun\"]','Canon R5','RF50'),
                    ('b','b','f','raw',1,'',0,'[]','Nikon Z9','Z24'),
                    ('c','c','f','raw',0,'blue',-1,'[\"sun\"]','Canon R5','RF85');",
        ).unwrap();
        let id = c.collection_add("favs", false, "").unwrap();
        c.collection_add_items(id, &["a".into(), "c".into()]).unwrap();
        assert_eq!(c.collection_items(id).unwrap().len(), 2);
        c.collection_remove_items(id, &["c".into()]).unwrap();
        assert_eq!(c.collection_items(id).unwrap(), vec!["a"]);
        let cols = c.collections().unwrap();
        assert_eq!(cols[0].count, 1);

        let hits = c.smart_eval(r#"{"rating_min":1}"#).unwrap();
        assert_eq!(hits.len(), 2); // rating>=1 matches a(5) and b(1)
        let hits = c.smart_eval(r#"{"rating_eq":5}"#).unwrap();
        assert_eq!(hits.len(), 1);
        let hits = c.smart_eval(r#"{"keyword":"sun"}"#).unwrap();
        assert_eq!(hits.len(), 2);
        let hits = c.smart_eval(r#"{"flag":-1}"#).unwrap();
        assert_eq!(hits[0].name, "c");
        let hits = c.smart_eval(r#"{"camera_contains":"canon","label":"blue"}"#).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn db_snapshot_roundtrip() {
        let c = Catalog::open_mem().unwrap();
        c.db.execute_batch(
            "INSERT INTO files(path,name,folder,kind,rating,flag,keywords,stack,ctime,iso)
             VALUES('a','a','/f','raw',3,1,'[\"k\"]',7,100,800);",
        ).unwrap();
        let a = c.assets(Path::new("/f")).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].rating, 3);
        assert_eq!(a[0].stack, 7);
        assert_eq!(a[0].iso, 800.0);
        assert_eq!(a[0].keywords, vec!["k"]);
        assert_eq!(c.folders().unwrap(), vec!["/f"]);
    }
}
