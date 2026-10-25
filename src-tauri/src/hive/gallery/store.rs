//! The gallery's index (docs/hive.md §10.2): one SQLite file per Hive, Rust's alone. Rows are
//! never deleted: emptying the trash keeps a tombstone, so lineage, receipts and the ledger hold.
//!
//! Migrations are additive only (`ALTER TABLE … ADD COLUMN`, errors ignored, as `scrollback_db`),
//! and every statement here names its columns, so an index a newer build stamped and widened
//! still opens in an older one (plan D5).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde_json::{Map, Value};

use super::model::*;
use super::{FILES, INDEX, THUMBS, TMP, TRASH};

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS assets (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL DEFAULT '',
        file TEXT NOT NULL DEFAULT '',
        mime TEXT NOT NULL DEFAULT '',
        bytes INTEGER NOT NULL DEFAULT 0,
        sha256 TEXT NOT NULL DEFAULT '',
        width INTEGER,
        height INTEGER,
        duration_ms INTEGER,
        title TEXT NOT NULL DEFAULT '',
        caption TEXT,
        made_by_kind TEXT NOT NULL DEFAULT '',
        made_by_tab TEXT NOT NULL DEFAULT '',
        made_by_role TEXT NOT NULL DEFAULT '',
        made_by_title TEXT NOT NULL DEFAULT '',
        made_by_badge INTEGER NOT NULL DEFAULT 0,
        task_id TEXT,
        source_path TEXT NOT NULL DEFAULT '',
        recipe TEXT,
        cost_micros INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT '',
        starred INTEGER NOT NULL DEFAULT 0,
        thumb TEXT NOT NULL DEFAULT 'pending',
        extras TEXT NOT NULL DEFAULT '{}',
        created_at TEXT NOT NULL DEFAULT '',
        created_ms INTEGER NOT NULL DEFAULT 0,
        deleted_at TEXT,
        purged INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS assets_created ON assets(created_ms);
    CREATE INDEX IF NOT EXISTS assets_task ON assets(task_id);
    CREATE INDEX IF NOT EXISTS assets_sha ON assets(sha256);
    CREATE TABLE IF NOT EXISTS asset_parents (
        asset_id TEXT NOT NULL, parent_id TEXT NOT NULL, pos INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (asset_id, parent_id)
    );
    CREATE INDEX IF NOT EXISTS asset_parents_parent ON asset_parents(parent_id);
    CREATE TABLE IF NOT EXISTS asset_tags (asset_id TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (asset_id, tag));
    CREATE INDEX IF NOT EXISTS asset_tags_tag ON asset_tags(tag);
    CREATE TABLE IF NOT EXISTS collections (
        id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE, cover TEXT, created_at TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE IF NOT EXISTS collection_items (
        collection_id TEXT NOT NULL, asset_id TEXT NOT NULL, pos INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (collection_id, asset_id)
    );
    CREATE INDEX IF NOT EXISTS collection_items_asset ON collection_items(asset_id);
    CREATE TABLE IF NOT EXISTS spends (
        id TEXT PRIMARY KEY,
        asset_id TEXT,
        task_id TEXT,
        made_by TEXT NOT NULL DEFAULT '{}',
        tab_id TEXT,
        cost_micros INTEGER NOT NULL DEFAULT 0,
        tool TEXT NOT NULL DEFAULT '',
        model TEXT NOT NULL DEFAULT '',
        at TEXT NOT NULL DEFAULT '',
        at_ms INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS spends_at ON spends(at_ms);
";

/// Every asset read goes through this one SELECT; `read_asset` takes its columns in order.
/// Children count only those out of the trash; tags are joined on a unit separator, which
/// `normalize_tag` keeps out of every tag.
const SELECT_ASSET: &str = "
    SELECT a.id, a.kind, a.file, a.mime, a.bytes, a.sha256, a.width, a.height, a.duration_ms, a.title, a.caption,
        a.made_by_kind, a.made_by_tab, a.made_by_role, a.made_by_title, a.made_by_badge, a.task_id, a.source_path,
        a.recipe, a.status, a.starred, a.thumb, a.extras, a.created_at, a.created_ms, a.deleted_at, a.purged,
        (SELECT group_concat(p.parent_id, ',' ORDER BY p.pos) FROM asset_parents p WHERE p.asset_id = a.id),
        (SELECT count(*) FROM asset_parents p JOIN assets c ON c.id = p.asset_id
            WHERE p.parent_id = a.id AND c.deleted_at IS NULL AND c.purged = 0),
        (SELECT group_concat(t.tag, char(31) ORDER BY t.tag) FROM asset_tags t WHERE t.asset_id = a.id),
        (SELECT group_concat(ci.collection_id, ',' ORDER BY c.rowid) FROM collection_items ci
            JOIN collections c ON c.id = ci.collection_id WHERE ci.asset_id = a.id)
    FROM assets a";

const ORDER_NEWEST: &str = " ORDER BY a.created_ms DESC, a.rowid DESC";

fn err(e: impl std::fmt::Display) -> String {
    format!("gallery index: {e}")
}

fn split(s: Option<String>, sep: char) -> Vec<String> {
    s.map(|s| s.split(sep).filter(|x| !x.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

fn read_asset(r: &Row) -> rusqlite::Result<Asset> {
    let recipe: Option<String> = r.get(18)?;
    let extras: String = r.get(22)?;
    Ok(Asset {
        id: r.get(0)?,
        kind: r.get(1)?,
        file: r.get(2)?,
        mime: r.get(3)?,
        bytes: r.get::<_, i64>(4)?.max(0) as u64,
        sha256: r.get(5)?,
        width: r.get::<_, Option<i64>>(6)?.and_then(|v| u32::try_from(v).ok()),
        height: r.get::<_, Option<i64>>(7)?.and_then(|v| u32::try_from(v).ok()),
        duration_ms: r.get::<_, Option<i64>>(8)?.and_then(|v| u64::try_from(v).ok()),
        title: r.get(9)?,
        caption: r.get(10)?,
        made_by: MadeBy {
            kind: r.get(11)?,
            tab_id: r.get(12)?,
            role: r.get(13)?,
            worker_title: r.get(14)?,
            badge: u32::try_from(r.get::<_, i64>(15)?).unwrap_or(0),
        },
        task_id: r.get(16)?,
        source_path: r.get(17)?,
        recipe: recipe.and_then(|s| serde_json::from_str(&s).ok()),
        status: r.get(19)?,
        starred: r.get::<_, i64>(20)? != 0,
        thumb: r.get(21)?,
        extras: serde_json::from_str::<Map<String, Value>>(&extras).unwrap_or_default(),
        created_at: r.get(23)?,
        created_ms: r.get(24)?,
        deleted_at: r.get(25)?,
        purged: r.get::<_, i64>(26)? != 0,
        parents: split(r.get(27)?, ','),
        children: u32::try_from(r.get::<_, i64>(28)?).unwrap_or(0),
        tags: split(r.get(29)?, '\u{1f}'),
        collections: split(r.get(30)?, ','),
    })
}

fn get_in(conn: &Connection, id: &str) -> Result<Option<Asset>, String> {
    conn.query_row(&format!("{SELECT_ASSET} WHERE a.id = ?1"), [id], read_asset).optional().map_err(err)
}

/// `(exists, in the trash, purged)` for one id.
fn state_of(conn: &Connection, id: &str) -> Result<Option<(bool, bool)>, String> {
    conn.query_row("SELECT deleted_at IS NOT NULL, purged != 0 FROM assets WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(err)
}

/// A LIKE pattern matching `q` anywhere, `\` escaping its wildcards.
fn like_anywhere(q: &str) -> String {
    let mut s = String::with_capacity(q.len() + 2);
    s.push('%');
    for c in q.chars() {
        if matches!(c, '\\' | '%' | '_') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

fn normalized_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.iter().map(|t| normalize_tag(t)).filter(|t| !t.is_empty()) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn dedup(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter().filter(|i| seen.insert(i.as_str())).cloned().collect()
}

fn now() -> (String, i64) {
    (crate::commands::workspace::iso_now(), crate::hive::fuses::now_ms())
}

/// Removes a file the store wrote read-only: Windows refuses to delete one otherwise.
fn remove_stored(p: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if let Ok(m) = fs::metadata(p) {
        let mut perm = m.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = fs::set_permissions(p, perm);
    }
    match fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub struct Gallery {
    root: PathBuf,
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Gallery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gallery").field("root", &self.root).finish_non_exhaustive()
    }
}

impl Gallery {
    /// Opens (and makes) the gallery at `gallery_dir`: its folders, the index and its schema. A
    /// purge a crash interrupted is settled here (`sweep_trash`).
    pub fn open(gallery_dir: &Path) -> Result<Self, String> {
        for d in [gallery_dir.to_path_buf(), gallery_dir.join(FILES), gallery_dir.join(THUMBS), gallery_dir.join(TRASH)] {
            fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let conn = Connection::open(gallery_dir.join(INDEX)).map_err(err)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA journal_size_limit=67108864;",
        )
        .map_err(err)?;
        conn.execute_batch(SCHEMA).map_err(err)?;
        // Additive migrations go here, each an `ALTER TABLE … ADD COLUMN` whose error is ignored.
        conn.execute("INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?1)", [SCHEMA_VERSION.to_string()]).map_err(err)?;
        let stamped: Option<String> = conn.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).optional().map_err(err)?;
        if let Some(v) = stamped.and_then(|v| v.parse::<i64>().ok()).filter(|v| *v > SCHEMA_VERSION) {
            log::info!("gallery {}: index from a newer build (schema {v}); reading it as schema {SCHEMA_VERSION}", gallery_dir.display());
        }
        let g = Self { root: gallery_dir.to_path_buf(), conn: Mutex::new(conn) };
        g.sweep_trash();
        Ok(g)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files_dir(&self) -> PathBuf {
        self.root.join(FILES)
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.root.join(THUMBS)
    }

    pub fn trash_dir(&self) -> PathBuf {
        self.root.join(TRASH)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP)
    }

    /// Where an asset's stored file is: the path an agent uses it by.
    pub fn file_path(&self, a: &Asset) -> PathBuf {
        self.files_dir().join(&a.file)
    }

    /// Files one asset in one transaction: its row, parents, tags and collection place. Every
    /// parent must exist and not be purged; the collection must exist.
    pub fn insert_asset(&self, a: NewAsset) -> Result<Asset, String> {
        if !is_asset_id(&a.id) {
            return Err(format!("\"{}\" isn't an asset id", a.id));
        }
        let parents = dedup(&a.parents);
        if parents.contains(&a.id) {
            return Err("An asset can't be its own parent.".into());
        }
        let recipe = a.recipe.as_ref().map(serde_json::to_string).transpose().map_err(err)?;
        let cost = a.recipe.as_ref().and_then(|r| r.cost_usd).map(usd_to_micros).unwrap_or(0);
        let thumb = if a.thumb.is_empty() { THUMB_PENDING } else { a.thumb.as_str() };
        let kind = if a.kind.is_empty() { KIND_OTHER } else { a.kind.as_str() };
        let (at, ms) = now();
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(err)?;
        for p in &parents {
            match state_of(&tx, p)? {
                None => return Err(format!("No asset {p} to derive from.")),
                Some((_, true)) => return Err(format!("{p} was emptied from the trash; it can't be a parent.")),
                Some(_) => {}
            }
        }
        tx.execute(
            "INSERT INTO assets (id, kind, file, mime, bytes, sha256, width, height, duration_ms, title, caption,
                made_by_kind, made_by_tab, made_by_role, made_by_title, made_by_badge, task_id, source_path, recipe,
                cost_micros, status, starred, thumb, extras, created_at, created_ms, deleted_at, purged)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, '', 0, ?21, '{}', ?22, ?23, NULL, 0)",
            params![
                a.id,
                kind,
                a.file,
                a.mime,
                a.bytes as i64,
                a.sha256,
                a.width,
                a.height,
                a.duration_ms.map(|d| d as i64),
                a.title,
                a.caption,
                a.made_by.kind,
                a.made_by.tab_id,
                a.made_by.role,
                a.made_by.worker_title,
                a.made_by.badge,
                a.task_id,
                a.source_path,
                recipe,
                cost,
                thumb,
                at,
                ms,
            ],
        )
        .map_err(err)?;
        for (pos, p) in parents.iter().enumerate() {
            tx.execute("INSERT INTO asset_parents (asset_id, parent_id, pos) VALUES (?1, ?2, ?3)", params![a.id, p, pos as i64]).map_err(err)?;
        }
        for t in normalized_tags(&a.tags) {
            tx.execute("INSERT OR IGNORE INTO asset_tags (asset_id, tag) VALUES (?1, ?2)", params![a.id, t]).map_err(err)?;
        }
        if let Some(c) = a.collection.as_deref() {
            let found: Option<String> = tx.query_row("SELECT id FROM collections WHERE id = ?1", [c], |r| r.get(0)).optional().map_err(err)?;
            if found.is_none() {
                return Err(format!("No collection {c}."));
            }
            tx.execute(
                "INSERT OR IGNORE INTO collection_items (collection_id, asset_id, pos)
                 VALUES (?1, ?2, (SELECT COALESCE(MAX(pos) + 1, 0) FROM collection_items WHERE collection_id = ?1))",
                params![c, a.id],
            )
            .map_err(err)?;
        }
        tx.commit().map_err(err)?;
        get_in(&conn, &a.id)?.ok_or_else(|| err("the asset just filed is missing"))
    }

    /// One ledger row. Never refused for being over a budget: the money is spent (plan D3).
    pub fn record_spend(&self, s: NewSpend) -> Result<Spend, String> {
        if s.cost_micros < 0 {
            return Err("A spend can't be negative.".into());
        }
        let conn = self.conn.lock();
        if let Some(a) = s.asset_id.as_deref() {
            if state_of(&conn, a)?.is_none() {
                return Err(format!("No asset {a} to charge."));
            }
        }
        let (at, at_ms) = now();
        let spend = Spend {
            id: mint(SPEND_PREFIX),
            asset_id: s.asset_id,
            task_id: s.task_id,
            tab_id: Some(s.made_by.tab_id.clone()).filter(|t| !t.is_empty()),
            made_by: s.made_by,
            cost_micros: s.cost_micros,
            tool: s.tool,
            model: s.model,
            at,
            at_ms,
        };
        conn.execute(
            "INSERT INTO spends (id, asset_id, task_id, made_by, tab_id, cost_micros, tool, model, at, at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                spend.id,
                spend.asset_id,
                spend.task_id,
                serde_json::to_string(&spend.made_by).map_err(err)?,
                spend.tab_id,
                spend.cost_micros,
                spend.tool,
                spend.model,
                spend.at,
                spend.at_ms,
            ],
        )
        .map_err(err)?;
        Ok(spend)
    }

    /// Any asset, trashed and purged ones included: a tombstone still answers.
    pub fn get(&self, id: &str) -> Result<Option<Asset>, String> {
        get_in(&self.conn.lock(), id)
    }

    /// Newest first. Purged assets never list; the trash only with `include_trash`.
    pub fn list(&self, f: &ListFilter) -> Result<Vec<AssetRow>, String> {
        let some = |o: &Option<String>| o.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let mut wh: Vec<&str> = vec!["a.purged = 0"];
        let mut args: Vec<SqlValue> = Vec::new();
        if !f.include_trash {
            wh.push("a.deleted_at IS NULL");
        }
        if let Some(k) = some(&f.kind) {
            wh.push("a.kind = ?");
            args.push(k.into());
        }
        if let Some(m) = some(&f.made_by) {
            wh.push("(a.made_by_kind = ? OR a.made_by_tab = ?)");
            args.push(m.clone().into());
            args.push(m.into());
        }
        if let Some(t) = some(&f.task_id) {
            wh.push("a.task_id = ?");
            args.push(t.into());
        }
        if let Some(c) = some(&f.collection) {
            wh.push(
                "EXISTS (SELECT 1 FROM collection_items ci JOIN collections c ON c.id = ci.collection_id
                    WHERE ci.asset_id = a.id AND (c.id = ? OR c.name = ? COLLATE NOCASE))",
            );
            args.push(c.clone().into());
            args.push(c.into());
        }
        if let Some(s) = f.status.as_deref() {
            wh.push("a.status = ?");
            args.push(s.trim().to_string().into());
        }
        if let Some(t) = some(&f.tag).map(|t| normalize_tag(&t)) {
            wh.push("EXISTS (SELECT 1 FROM asset_tags t WHERE t.asset_id = a.id AND t.tag = ?)");
            args.push(t.into());
        }
        if let Some(s) = f.starred {
            wh.push("a.starred = ?");
            args.push((s as i64).into());
        }
        if let Some(q) = some(&f.query) {
            wh.push(
                "(a.title LIKE ? ESCAPE '\\'
                  OR (CASE WHEN json_valid(a.recipe) THEN json_extract(a.recipe, '$.prompt') END) LIKE ? ESCAPE '\\'
                  OR EXISTS (SELECT 1 FROM asset_tags t WHERE t.asset_id = a.id AND t.tag LIKE ? ESCAPE '\\'))",
            );
            let pat = like_anywhere(&q);
            args.extend([pat.clone().into(), pat.clone().into(), pat.into()]);
        }
        let mut sql = format!("{SELECT_ASSET} WHERE {}{ORDER_NEWEST}", wh.join(" AND "));
        if let Some(n) = f.limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&sql).map_err(err)?;
        let rows = stmt.query_map(params_from_iter(args), read_asset).map_err(err)?;
        rows.map(|r| r.map(|a| AssetRow::from(&a)).map_err(err)).collect()
    }

    /// The live asset with these bytes filed for the same task, so re-filing answers it (plan
    /// D6). `None` for the task means the coordinator's own: a human's file is always new.
    pub fn by_sha(&self, task_id: Option<&str>, sha: &str) -> Result<Option<Asset>, String> {
        let conn = self.conn.lock();
        let live = "a.sha256 = ?1 AND a.deleted_at IS NULL AND a.purged = 0";
        let res = match task_id {
            Some(t) => conn.query_row(&format!("{SELECT_ASSET} WHERE {live} AND a.task_id = ?2 ORDER BY a.created_ms, a.rowid LIMIT 1"), params![sha, t], read_asset),
            None => conn.query_row(
                &format!("{SELECT_ASSET} WHERE {live} AND a.task_id IS NULL AND a.made_by_kind = '{BY_COORDINATOR}' ORDER BY a.created_ms, a.rowid LIMIT 1"),
                params![sha],
                read_asset,
            ),
        };
        res.optional().map_err(err)
    }

    /// Tags added and removed, status and star set, on every id or none: an unknown or purged
    /// id refuses the whole change. Who may set what is the caller's to decide (plan D12).
    pub fn tag(&self, ids: &[String], c: &TagChange) -> Result<Vec<Asset>, String> {
        let ids = dedup(ids);
        if ids.is_empty() {
            return Err("No assets named.".into());
        }
        if let Some(s) = c.status.as_deref() {
            if !STATUSES.contains(&s) {
                return Err(format!("\"{s}\" isn't a status: \"\", picked, final or rejected."));
            }
        }
        let (add, untag) = (normalized_tags(&c.tags), normalized_tags(&c.untag));
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(err)?;
        for id in &ids {
            match state_of(&tx, id)? {
                None => return Err(format!("No asset {id}.")),
                Some((_, true)) => return Err(format!("{id} was emptied from the trash.")),
                Some(_) => {}
            }
            for t in &add {
                tx.execute("INSERT OR IGNORE INTO asset_tags (asset_id, tag) VALUES (?1, ?2)", params![id, t]).map_err(err)?;
            }
            for t in &untag {
                tx.execute("DELETE FROM asset_tags WHERE asset_id = ?1 AND tag = ?2", params![id, t]).map_err(err)?;
            }
            if let Some(s) = c.status.as_deref() {
                tx.execute("UPDATE assets SET status = ?2 WHERE id = ?1", params![id, s]).map_err(err)?;
            }
            if let Some(s) = c.starred {
                tx.execute("UPDATE assets SET starred = ?2 WHERE id = ?1", params![id, s as i64]).map_err(err)?;
            }
        }
        tx.commit().map_err(err)?;
        ids.iter().map(|id| get_in(&conn, id)?.ok_or_else(|| err("an asset just tagged is missing"))).collect()
    }

    /// Builds one collection: found by id or name (made by name with `create`), then members
    /// added, removed, reordered and the cover set, in that order. An order must be a full
    /// permutation of the members out of the trash; trashed members keep their places after them.
    pub fn collect(&self, c: &CollectChange) -> Result<Collection, String> {
        let key = c.collection.trim();
        if key.is_empty() {
            return Err("Name a collection.".into());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(err)?;
        let found: Option<String> =
            tx.query_row("SELECT id FROM collections WHERE id = ?1 OR name = ?1 COLLATE NOCASE", [key], |r| r.get(0)).optional().map_err(err)?;
        let id = match found {
            Some(id) => id,
            None if c.create => {
                if key.chars().count() > 100 || key.chars().any(char::is_control) || is_collection_id(key) {
                    return Err(format!("\"{key}\" can't name a collection: at most 100 characters, one line, not an id."));
                }
                let id = mint(COLLECTION_PREFIX);
                tx.execute("INSERT INTO collections (id, name, cover, created_at) VALUES (?1, ?2, NULL, ?3)", params![id, key, now().0]).map_err(err)?;
                id
            }
            None => return Err(format!("No collection called \"{key}\".")),
        };
        let mut members: Vec<String> = {
            let mut stmt = tx.prepare("SELECT asset_id FROM collection_items WHERE collection_id = ?1 ORDER BY pos").map_err(err)?;
            let rows = stmt.query_map([&id], |r| r.get(0)).map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };
        for a in dedup(&c.add) {
            match state_of(&tx, &a)? {
                None => return Err(format!("No asset {a}.")),
                Some((true, _)) | Some((_, true)) => return Err(format!("{a} is in the trash.")),
                Some(_) => {}
            }
            if !members.contains(&a) {
                members.push(a);
            }
        }
        members.retain(|m| !c.remove.contains(m));
        if let Some(order) = &c.order {
            let mut live = Vec::new();
            let mut trashed = Vec::new();
            for m in &members {
                match state_of(&tx, m)? {
                    Some((false, false)) => live.push(m.clone()),
                    _ => trashed.push(m.clone()),
                }
            }
            let given: HashSet<&String> = order.iter().collect();
            if order.len() != live.len() || given.len() != order.len() || !live.iter().all(|m| given.contains(m)) {
                return Err(format!("An order must list each of the collection's {} assets once, and nothing else.", live.len()));
            }
            members = order.iter().cloned().chain(trashed).collect();
        }
        let cover_now: Option<String> = tx.query_row("SELECT cover FROM collections WHERE id = ?1", [&id], |r| r.get(0)).map_err(err)?;
        let cover = match c.cover.as_deref() {
            Some("") => None,
            Some(a) if members.iter().any(|m| m == a) => Some(a.to_string()),
            Some(a) => return Err(format!("{a} isn't in the collection, so it can't be its cover.")),
            None => cover_now.filter(|a| members.contains(a)),
        };
        tx.execute("DELETE FROM collection_items WHERE collection_id = ?1", [&id]).map_err(err)?;
        for (pos, m) in members.iter().enumerate() {
            tx.execute("INSERT INTO collection_items (collection_id, asset_id, pos) VALUES (?1, ?2, ?3)", params![id, m, pos as i64]).map_err(err)?;
        }
        tx.execute("UPDATE collections SET cover = ?2 WHERE id = ?1", params![id, cover]).map_err(err)?;
        tx.commit().map_err(err)?;
        collections_in(&conn)?.into_iter().find(|x| x.id == id).ok_or_else(|| err("the collection just built is missing"))
    }

    /// Every collection, oldest first, members in order.
    pub fn collections(&self) -> Result<Vec<Collection>, String> {
        collections_in(&self.conn.lock())
    }

    /// The thumbnailer's findings. Dimensions it couldn't read stay as they were.
    pub fn set_meta(&self, id: &str, m: &MediaMeta) -> Result<(), String> {
        let conn = self.conn.lock();
        let extras: Option<String> = conn.query_row("SELECT extras FROM assets WHERE id = ?1", [id], |r| r.get(0)).optional().map_err(err)?;
        let Some(extras) = extras else { return Err(format!("No asset {id}.")) };
        let mut extras: Map<String, Value> = serde_json::from_str(&extras).unwrap_or_default();
        extras.insert("strip".into(), Value::Bool(m.strip));
        extras.insert("wave".into(), Value::Bool(m.wave));
        conn.execute(
            "UPDATE assets SET width = COALESCE(?2, width), height = COALESCE(?3, height),
                duration_ms = COALESCE(?4, duration_ms), thumb = CASE WHEN ?5 = '' THEN thumb ELSE ?5 END, extras = ?6
             WHERE id = ?1",
            params![id, m.width, m.height, m.duration_ms.map(|d| d as i64), m.thumb, serde_json::to_string(&extras).map_err(err)?],
        )
        .map_err(err)?;
        Ok(())
    }

    /// Assets still waiting for a thumbnail, oldest first; the trash waits for nothing.
    pub fn pending_thumbs(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM assets WHERE thumb = ?1 AND deleted_at IS NULL AND purged = 0 ORDER BY created_ms, rowid")
            .map_err(err)?;
        let rows = stmt.query_map([THUMB_PENDING], |r| r.get(0)).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    /// Into the trash: hidden from lists, file kept. Answers how many moved.
    pub fn trash(&self, ids: &[String]) -> Result<usize, String> {
        let at = now().0;
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(err)?;
        let mut n = 0;
        for id in dedup(ids) {
            n += tx.execute("UPDATE assets SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL AND purged = 0", params![id, at]).map_err(err)?;
        }
        tx.commit().map_err(err)?;
        Ok(n)
    }

    /// Out of the trash, unless it was emptied. Answers how many came back.
    pub fn restore(&self, ids: &[String]) -> Result<usize, String> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(err)?;
        let mut n = 0;
        for id in dedup(ids) {
            n += tx.execute("UPDATE assets SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL AND purged = 0", [id]).map_err(err)?;
        }
        tx.commit().map_err(err)?;
        Ok(n)
    }

    /// Empties the trash (plan D11): each file moves to `trash/`, the rows become tombstones and
    /// leave their collections in one transaction, then the moved files and the thumbnails go. A
    /// crash between the move and the commit is settled by the next open's `sweep_trash`.
    pub fn purge_trash(&self) -> Result<usize, String> {
        let (files, trash) = (self.files_dir(), self.trash_dir());
        let mut conn = self.conn.lock();
        let victims: Vec<(String, String)> = {
            let mut stmt = conn.prepare("SELECT id, file FROM assets WHERE deleted_at IS NOT NULL AND purged = 0").map_err(err)?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(err)?;
            rows.collect::<Result<_, _>>().map_err(err)?
        };
        let mut moved: Vec<(String, String)> = Vec::new();
        for (id, file) in victims {
            let ok = file.is_empty()
                || match fs::rename(files.join(&file), trash.join(&file)) {
                    Ok(()) => true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Err(e) => {
                        log::warn!("gallery {}: {id} stays in the trash, its file didn't move: {e}", self.root.display());
                        false
                    }
                };
            if ok {
                moved.push((id, file));
            }
        }
        let committed = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            for (id, _) in &moved {
                tx.execute("UPDATE assets SET purged = 1 WHERE id = ?1", [id])?;
                tx.execute("DELETE FROM collection_items WHERE asset_id = ?1", [id])?;
                tx.execute("UPDATE collections SET cover = NULL WHERE cover = ?1", [id])?;
            }
            tx.commit()
        })();
        if let Err(e) = committed {
            for (_, file) in moved.iter().filter(|(_, f)| !f.is_empty()) {
                let _ = fs::rename(trash.join(file), files.join(file));
            }
            return Err(err(e));
        }
        drop(conn);
        for (id, file) in &moved {
            if !file.is_empty() {
                if let Err(e) = remove_stored(&trash.join(file)) {
                    log::warn!("gallery {}: {file} left in trash/: {e}", self.root.display());
                }
            }
            self.remove_thumbs(id);
        }
        Ok(moved.len())
    }

    fn remove_thumbs(&self, id: &str) {
        let prefix = format!("{id}.");
        for e in fs::read_dir(self.thumbs_dir()).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = remove_stored(&e.path());
            }
        }
        let _ = fs::remove_dir_all(self.tmp_dir().join(id));
    }

    /// Whatever a purge left in `trash/`: a file whose asset is still in the trash (the purge's
    /// commit never happened) goes back; anything else is gone for good.
    fn sweep_trash(&self) {
        let (files, trash) = (self.files_dir(), self.trash_dir());
        let conn = self.conn.lock();
        for e in fs::read_dir(&trash).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let back = conn
                .query_row("SELECT purged FROM assets WHERE file = ?1", [&name], |r| r.get::<_, i64>(0))
                .optional()
                .ok()
                .flatten()
                .is_some_and(|p| p == 0)
                && fs::symlink_metadata(files.join(&name)).is_err();
            let res = if back { fs::rename(e.path(), files.join(&name)) } else { remove_stored(&e.path()) };
            if let Err(err) = res {
                log::warn!("gallery {}: trash/{name} not settled: {err}", self.root.display());
            }
        }
    }

    /// The ledger's total, spend-only rows included: what was spent, not what was kept.
    pub fn spent_micros(&self) -> Result<i64, String> {
        self.conn.lock().query_row("SELECT COALESCE(SUM(cost_micros), 0) FROM spends", [], |r| r.get(0)).map_err(err)
    }

    /// Every asset a task filed that still has its file, the trash's included, oldest first.
    pub fn task_assets(&self, task_id: &str) -> Result<Vec<Asset>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!("{SELECT_ASSET} WHERE a.task_id = ?1 AND a.purged = 0 ORDER BY a.created_ms, a.rowid")).map_err(err)?;
        let rows = stmt.query_map([task_id], read_asset).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }
}

fn collections_in(conn: &Connection) -> Result<Vec<Collection>, String> {
    let mut out: Vec<Collection> = {
        let mut stmt = conn.prepare("SELECT id, name, cover, created_at FROM collections ORDER BY rowid").map_err(err)?;
        let rows = stmt
            .query_map([], |r| Ok(Collection { id: r.get(0)?, name: r.get(1)?, cover: r.get(2)?, created_at: r.get(3)?, asset_ids: Vec::new() }))
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)?
    };
    let mut stmt = conn.prepare("SELECT collection_id, asset_id FROM collection_items ORDER BY collection_id, pos").map_err(err)?;
    let items = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(err)?;
    for item in items {
        let (c, a) = item.map_err(err)?;
        if let Some(col) = out.iter_mut().find(|x| x.id == c) {
            col.asset_ids.push(a);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::gallery::ingest;

    struct Scratch {
        base: PathBuf,
        g: Gallery,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn scratch() -> Scratch {
        let base = std::env::temp_dir().join(format!("hive-gallery-store-{}", uuid::Uuid::new_v4()));
        let g = Gallery::open(&base.join("gallery")).unwrap();
        Scratch { base, g }
    }

    fn worker(badge: u32) -> MadeBy {
        MadeBy { kind: BY_WORKER.into(), tab_id: format!("tab-{badge}"), role: "image".into(), worker_title: format!("Worker {badge}"), badge }
    }

    fn new(title: &str, task: Option<&str>) -> NewAsset {
        let id = mint(ASSET_PREFIX);
        NewAsset {
            file: format!("{id}.png"),
            id,
            kind: KIND_IMAGE.into(),
            mime: "image/png".into(),
            bytes: 3,
            sha256: format!("sha-{title}"),
            title: title.into(),
            made_by: if task.is_some() { worker(2) } else { MadeBy { kind: BY_COORDINATOR.into(), tab_id: "coord".into(), ..MadeBy::default() } },
            task_id: task.map(str::to_string),
            ..NewAsset::default()
        }
    }

    fn add(s: &Scratch, title: &str, task: Option<&str>) -> Asset {
        let a = new(title, task);
        fs::write(s.g.files_dir().join(&a.file), b"png").unwrap();
        s.g.insert_asset(a).unwrap()
    }

    fn ids(rows: &[AssetRow]) -> Vec<String> {
        rows.iter().map(|r| r.title.clone()).collect()
    }

    #[test]
    fn open_makes_the_folders_and_schema_and_reopening_is_idempotent() {
        let s = scratch();
        for d in [FILES, THUMBS, TRASH] {
            assert!(s.base.join("gallery").join(d).is_dir(), "{d}");
        }
        let a = add(&s, "hero", Some("t1"));
        drop(Gallery::open(&s.base.join("gallery")).unwrap());
        let again = Gallery::open(&s.base.join("gallery")).unwrap();
        assert_eq!(again.get(&a.id).unwrap().unwrap(), a);
        let v: String = again.conn.lock().query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn an_index_a_newer_build_stamped_and_widened_still_opens_lists_and_files() {
        let s = scratch();
        add(&s, "old", Some("t1"));
        {
            let c = s.g.conn.lock();
            c.execute_batch(
                "UPDATE meta SET value = '9' WHERE key = 'schema_version';
                 ALTER TABLE assets ADD COLUMN future TEXT;
                 ALTER TABLE spends ADD COLUMN currency TEXT NOT NULL DEFAULT 'usd';
                 CREATE TABLE later_things (x TEXT);",
            )
            .unwrap();
        }
        let dir = s.base.join("gallery");
        let g = Gallery::open(&dir).unwrap();
        let a = new("new", Some("t1"));
        fs::write(g.files_dir().join(&a.file), b"png").unwrap();
        g.insert_asset(a).unwrap();
        g.record_spend(NewSpend { cost_micros: 5, ..NewSpend::default() }).unwrap();
        assert_eq!(ids(&g.list(&ListFilter::default()).unwrap()), vec!["new", "old"]);
        let v: String = g.conn.lock().query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "9", "an older build never lowers the stamp");
    }

    #[test]
    fn insert_and_get_round_trip_every_field() {
        let s = scratch();
        let parent = add(&s, "base", Some("t1"));
        let mut a = new("Hero, warm", Some("t1"));
        a.width = Some(1024);
        a.height = Some(768);
        a.duration_ms = Some(12_000);
        a.caption = Some("the second one".into());
        a.source_path = "/x/scratch/hero.png".into();
        a.recipe = Some(Recipe {
            tool: "mcp:fal".into(),
            model: Some("flux-pro".into()),
            prompt: Some("a hero shot".into()),
            negative: Some("blur".into()),
            seed: Some(-1),
            params: Some(serde_json::json!({"steps": 30})),
            source_url: Some("https://fal.media/x.png".into()),
            cost_usd: Some(0.04),
        });
        a.parents = vec![parent.id.clone(), parent.id.clone()];
        a.tags = vec!["Hero".into(), "hero ".into(), "Warm".into(), "  ".into()];
        let id = a.id.clone();
        let got = s.g.insert_asset(a.clone()).unwrap();
        assert_eq!(got, s.g.get(&id).unwrap().unwrap());
        assert_eq!((got.width, got.height, got.duration_ms), (Some(1024), Some(768), Some(12_000)));
        assert_eq!((got.title.as_str(), got.caption.as_deref(), got.source_path.as_str()), ("Hero, warm", Some("the second one"), "/x/scratch/hero.png"));
        assert_eq!(got.made_by, worker(2));
        assert_eq!(got.task_id.as_deref(), Some("t1"));
        assert_eq!(got.recipe, a.recipe);
        assert_eq!(got.parents, vec![parent.id.clone()], "a parent named twice is one parent");
        assert_eq!(got.tags, vec!["hero", "warm"], "lowercased, deduplicated, blanks out");
        assert_eq!((got.status.as_str(), got.starred, got.thumb.as_str()), ("", false, THUMB_PENDING));
        assert_eq!((got.kind.as_str(), got.mime.as_str(), got.bytes, got.sha256.as_str()), (KIND_IMAGE, "image/png", 3, "sha-Hero, warm"));
        assert!(got.created_ms > 0 && got.created_at.ends_with('Z') && got.deleted_at.is_none() && !got.purged);
        assert_eq!(s.g.get(&parent.id).unwrap().unwrap().children, 1);
        assert!(s.g.get("a-000000000000").unwrap().is_none());

        let mut orphan = new("orphan", None);
        orphan.parents = vec!["a-ffffffffffff".into()];
        assert!(s.g.insert_asset(orphan).unwrap_err().contains("No asset"));
        let mut own = new("own", None);
        own.parents = vec![own.id.clone()];
        assert!(s.g.insert_asset(own).is_err());
        let mut bad = new("bad", None);
        bad.id = "../x".into();
        assert!(s.g.insert_asset(bad).is_err());
    }

    #[test]
    fn list_is_newest_first_and_each_filter_narrows() {
        let s = scratch();
        let one = add(&s, "one", Some("t1"));
        let two = add(&s, "two", Some("t2"));
        let three = add(&s, "three", None);
        let mut vid = new("clip", Some("t2"));
        vid.kind = KIND_VIDEO.into();
        vid.recipe = Some(Recipe { tool: "ffmpeg".into(), prompt: Some("50%_off banner".into()), ..Recipe::default() });
        let vid = s.g.insert_asset(vid).unwrap();
        s.g.tag(std::slice::from_ref(&two.id), &TagChange { tags: vec!["Shortlist".into()], status: Some(STATUS_PICKED.into()), starred: Some(true), ..TagChange::default() }).unwrap();
        s.g.collect(&CollectChange { collection: "Storyboard".into(), create: true, add: vec![one.id.clone(), three.id.clone()], ..CollectChange::default() }).unwrap();

        let list = |f: ListFilter| ids(&s.g.list(&f).unwrap());
        assert_eq!(list(ListFilter::default()), vec!["clip", "three", "two", "one"]);
        assert_eq!(list(ListFilter { kind: Some(KIND_VIDEO.into()), ..Default::default() }), vec!["clip"]);
        assert_eq!(list(ListFilter { made_by: Some(BY_COORDINATOR.into()), ..Default::default() }), vec!["three"]);
        assert_eq!(list(ListFilter { made_by: Some("tab-2".into()), ..Default::default() }), vec!["clip", "two", "one"]);
        assert_eq!(list(ListFilter { task_id: Some("t2".into()), ..Default::default() }), vec!["clip", "two"]);
        assert_eq!(list(ListFilter { collection: Some("storyboard".into()), ..Default::default() }), vec!["three", "one"]);
        assert_eq!(list(ListFilter { status: Some(STATUS_PICKED.into()), ..Default::default() }), vec!["two"]);
        assert_eq!(list(ListFilter { status: Some(String::new()), ..Default::default() }), vec!["clip", "three", "one"]);
        assert_eq!(list(ListFilter { tag: Some("SHORTLIST".into()), ..Default::default() }), vec!["two"]);
        assert_eq!(list(ListFilter { starred: Some(true), ..Default::default() }), vec!["two"]);
        assert_eq!(list(ListFilter { starred: Some(false), limit: Some(2), ..Default::default() }), vec!["clip", "three"]);
        assert_eq!(list(ListFilter { query: Some("TWO".into()), ..Default::default() }), vec!["two"], "the title, any case");
        assert_eq!(list(ListFilter { query: Some("banner".into()), ..Default::default() }), vec!["clip"], "the prompt");
        assert_eq!(list(ListFilter { query: Some("shortl".into()), ..Default::default() }), vec!["two"], "the tags");
        assert_eq!(list(ListFilter { query: Some("%".into()), ..Default::default() }), vec!["clip"], "a % is a character, not a wildcard");
        assert_eq!(list(ListFilter { query: Some("0%_o".into()), ..Default::default() }), vec!["clip"]);
        assert!(list(ListFilter { query: Some("t_o".into()), ..Default::default() }).is_empty(), "nor is a _");
        let row = s.g.list(&ListFilter { kind: Some(KIND_VIDEO.into()), ..Default::default() }).unwrap().remove(0);
        assert_eq!((row.id, row.prompt_head.as_deref()), (vid.id, Some("50%_off banner")));
    }

    #[test]
    fn lineage_counts_children_out_of_the_trash() {
        let s = scratch();
        let base = add(&s, "base", Some("t1"));
        let mut kids = Vec::new();
        for n in 0..3 {
            let mut k = new(&format!("v{n}"), Some("t1"));
            k.parents = vec![base.id.clone()];
            fs::write(s.g.files_dir().join(&k.file), b"png").unwrap();
            kids.push(s.g.insert_asset(k).unwrap());
        }
        assert_eq!(s.g.get(&base.id).unwrap().unwrap().children, 3);
        s.g.trash(std::slice::from_ref(&kids[0].id)).unwrap();
        assert_eq!(s.g.get(&base.id).unwrap().unwrap().children, 2);
        assert_eq!(s.g.list(&ListFilter::default()).unwrap().iter().find(|r| r.id == base.id).unwrap().children, 2);
        assert_eq!(kids[1].parents, vec![base.id.clone()]);
    }

    #[test]
    fn tag_adds_and_removes_on_every_id_or_none() {
        let s = scratch();
        let (a, b) = (add(&s, "a", Some("t1")), add(&s, "b", Some("t1")));
        let both = vec![a.id.clone(), b.id.clone(), a.id.clone()];
        let out = s.g.tag(&both, &TagChange { tags: vec!["Hero".into(), "warm".into()], ..TagChange::default() }).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|x| x.tags == vec!["hero", "warm"]));
        let out = s.g.tag(std::slice::from_ref(&a.id), &TagChange { tags: vec!["cool".into()], untag: vec!["WARM".into()], status: Some(STATUS_FINAL.into()), starred: Some(true) }).unwrap();
        assert_eq!(out[0].tags, vec!["cool", "hero"]);
        assert_eq!((out[0].status.as_str(), out[0].starred), (STATUS_FINAL, true));
        assert!(s.g.tag(std::slice::from_ref(&a.id), &TagChange { status: Some("best".into()), ..TagChange::default() }).is_err());
        let e = s.g.tag(&[b.id.clone(), "a-ffffffffffff".into()], &TagChange { tags: vec!["x".into()], ..TagChange::default() }).unwrap_err();
        assert!(e.contains("No asset"));
        assert_eq!(s.g.get(&b.id).unwrap().unwrap().tags, vec!["hero", "warm"], "nothing changed on a refusal");
    }

    #[test]
    fn collect_adds_removes_orders_by_permutation_and_sets_the_cover() {
        let s = scratch();
        let [a, b, c] = ["a", "b", "c"].map(|t| add(&s, t, Some("t1")));
        let change = |c: CollectChange| s.g.collect(&c);
        assert!(change(CollectChange { collection: "Shortlist".into(), add: vec![a.id.clone()], ..Default::default() }).unwrap_err().contains("No collection"));
        let col = change(CollectChange { collection: "Shortlist".into(), create: true, add: vec![a.id.clone(), b.id.clone(), c.id.clone(), a.id.clone()], ..Default::default() }).unwrap();
        assert!(is_collection_id(&col.id));
        assert_eq!(col.asset_ids, vec![a.id.clone(), b.id.clone(), c.id.clone()]);
        assert!(change(CollectChange { collection: "SHORTLIST".into(), create: true, ..Default::default() }).unwrap().id == col.id, "names match without case");
        assert_eq!(s.g.get(&a.id).unwrap().unwrap().collections, vec![col.id.clone()]);

        let reorder = |order: Vec<&String>| change(CollectChange { collection: col.id.clone(), order: Some(order.into_iter().cloned().collect()), ..Default::default() });
        assert_eq!(reorder(vec![&c.id, &a.id, &b.id]).unwrap().asset_ids, vec![c.id.clone(), a.id.clone(), b.id.clone()]);
        assert!(reorder(vec![&c.id, &a.id]).unwrap_err().contains("each of"), "a missing member");
        assert!(reorder(vec![&c.id, &a.id, &a.id]).is_err(), "a repeat");
        assert!(reorder(vec![&c.id, &a.id, &b.id, &b.id]).is_err());

        let col = change(CollectChange { collection: col.id.clone(), cover: Some(b.id.clone()), ..Default::default() }).unwrap();
        assert_eq!(col.cover.as_deref(), Some(b.id.as_str()));
        let other = add(&s, "other", None);
        assert!(change(CollectChange { collection: col.id.clone(), cover: Some(other.id.clone()), ..Default::default() }).unwrap_err().contains("isn't in the collection"));
        let col = change(CollectChange { collection: col.id.clone(), remove: vec![b.id.clone()], ..Default::default() }).unwrap();
        assert_eq!((col.asset_ids, col.cover), (vec![c.id.clone(), a.id.clone()], None), "removing the cover clears it");

        s.g.trash(std::slice::from_ref(&c.id)).unwrap();
        assert!(change(CollectChange { collection: col.id.clone(), add: vec![c.id.clone()], ..Default::default() }).unwrap_err().contains("in the trash"));
        let col = change(CollectChange { collection: col.id.clone(), add: vec![b.id.clone()], order: Some(vec![b.id.clone(), a.id.clone()]), ..Default::default() }).unwrap();
        assert_eq!(col.asset_ids, vec![b.id.clone(), a.id.clone(), c.id.clone()], "an order names the live members; the trashed keep their place after");
        assert!(change(CollectChange { collection: "c-0123456789ab".into(), create: true, ..Default::default() }).is_err(), "a name can't pass for an id");
        assert_eq!(s.g.collections().unwrap().len(), 1);
    }

    #[test]
    fn the_ledger_sums_every_spend_kept_or_not() {
        let s = scratch();
        let a = add(&s, "a", Some("t1"));
        let sp = s.g.record_spend(NewSpend { asset_id: Some(a.id.clone()), task_id: Some("t1".into()), made_by: worker(2), cost_micros: 40_000, tool: "mcp:fal".into(), model: "flux".into() }).unwrap();
        assert!(sp.id.starts_with(SPEND_PREFIX) && sp.at_ms > 0);
        assert_eq!(sp.tab_id.as_deref(), Some("tab-2"));
        for _ in 0..1000 {
            s.g.record_spend(NewSpend { made_by: worker(2), cost_micros: usd_to_micros(0.003), ..NewSpend::default() }).unwrap();
        }
        assert_eq!(s.g.spent_micros().unwrap(), 40_000 + 3_000_000, "spend-only rows count, without float drift");
        assert!(s.g.record_spend(NewSpend { cost_micros: -1, ..NewSpend::default() }).is_err());
        assert!(s.g.record_spend(NewSpend { asset_id: Some("a-ffffffffffff".into()), ..NewSpend::default() }).is_err());
    }

    #[test]
    fn by_sha_finds_the_same_tasks_live_asset_only() {
        let s = scratch();
        let a = add(&s, "same", Some("t1"));
        assert_eq!(s.g.by_sha(Some("t1"), "sha-same").unwrap().map(|x| x.id), Some(a.id.clone()));
        assert!(s.g.by_sha(Some("t2"), "sha-same").unwrap().is_none(), "another task files its own");
        assert!(s.g.by_sha(None, "sha-same").unwrap().is_none());
        let c = add(&s, "mine", None);
        assert_eq!(s.g.by_sha(None, "sha-mine").unwrap().map(|x| x.id), Some(c.id));
        let mut h = new("dropped", None);
        h.made_by = MadeBy::human();
        s.g.insert_asset(h).unwrap();
        assert!(s.g.by_sha(None, "sha-dropped").unwrap().is_none(), "a human's file is never the coordinator's duplicate");
        s.g.trash(std::slice::from_ref(&a.id)).unwrap();
        assert!(s.g.by_sha(Some("t1"), "sha-same").unwrap().is_none(), "a trashed asset is no duplicate");
    }

    #[test]
    fn meta_lands_and_pending_thumbs_wait_out_of_the_trash() {
        let s = scratch();
        let (a, b, c) = (add(&s, "a", Some("t1")), add(&s, "b", Some("t1")), add(&s, "c", Some("t1")));
        assert_eq!(s.g.pending_thumbs().unwrap(), vec![a.id.clone(), b.id.clone(), c.id.clone()]);
        s.g.set_meta(&a.id, &MediaMeta { width: Some(640), height: Some(360), duration_ms: Some(3_000), thumb: THUMB_READY.into(), strip: true, wave: false }).unwrap();
        s.g.trash(std::slice::from_ref(&c.id)).unwrap();
        assert_eq!(s.g.pending_thumbs().unwrap(), vec![b.id.clone()]);
        let got = s.g.get(&a.id).unwrap().unwrap();
        assert_eq!((got.width, got.height, got.duration_ms, got.thumb.as_str()), (Some(640), Some(360), Some(3_000), THUMB_READY));
        assert_eq!((got.extras.get("strip"), got.extras.get("wave")), (Some(&Value::Bool(true)), Some(&Value::Bool(false))));
        s.g.set_meta(&a.id, &MediaMeta::default()).unwrap();
        let got = s.g.get(&a.id).unwrap().unwrap();
        assert_eq!((got.width, got.thumb.as_str()), (Some(640), THUMB_READY), "what a run couldn't read stays");
        assert!(s.g.set_meta("a-ffffffffffff", &MediaMeta::default()).is_err());
    }

    #[test]
    fn trash_restore_and_purge_keep_a_tombstone() {
        let s = scratch();
        let root = s.base.join("src");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("hero.png"), b"png!").unwrap();
        let src = ingest::authorize(&root.join("hero.png"), &root, std::slice::from_ref(&root), s.g.root()).unwrap();
        let mut n = new("hero", Some("t1"));
        let stored = ingest::store_file(&src, &s.g.files_dir(), &n.id).unwrap();
        (n.file, n.sha256, n.bytes) = (stored.file, stored.sha256, stored.bytes);
        let hero = s.g.insert_asset(n).unwrap();
        let mut kid = new("hero 2", Some("t1"));
        kid.parents = vec![hero.id.clone()];
        let kid = s.g.insert_asset(kid).unwrap();
        let col = s.g.collect(&CollectChange { collection: "Final".into(), create: true, add: vec![hero.id.clone(), kid.id.clone()], cover: Some(hero.id.clone()), ..Default::default() }).unwrap();
        for t in [format!("{}.png", hero.id), format!("{}.strip.jpg", hero.id)] {
            fs::write(s.g.thumbs_dir().join(t), b"t").unwrap();
        }
        let other_thumb = s.g.thumbs_dir().join(format!("{}.png", kid.id));
        fs::write(&other_thumb, b"t").unwrap();

        assert_eq!(s.g.trash(&[hero.id.clone(), hero.id.clone()]).unwrap(), 1);
        assert_eq!(s.g.trash(std::slice::from_ref(&hero.id)).unwrap(), 0, "already there");
        assert!(s.g.list(&ListFilter::default()).unwrap().iter().all(|r| r.id != hero.id), "the trash is hidden");
        let with = s.g.list(&ListFilter { include_trash: true, ..Default::default() }).unwrap();
        assert!(with.iter().find(|r| r.id == hero.id).unwrap().deleted_at.is_some());
        assert!(s.g.get(&hero.id).unwrap().unwrap().in_trash());
        assert_eq!(s.g.restore(std::slice::from_ref(&hero.id)).unwrap(), 1);
        assert!(s.g.list(&ListFilter::default()).unwrap().iter().any(|r| r.id == hero.id), "back");

        s.g.trash(std::slice::from_ref(&hero.id)).unwrap();
        let file = s.g.file_path(&hero);
        assert!(file.is_file());
        assert_eq!(s.g.purge_trash().unwrap(), 1);
        assert!(!file.exists(), "the file is gone");
        assert!(fs::read_dir(s.g.trash_dir()).unwrap().next().is_none(), "nothing left in trash/");
        assert!(!s.g.thumbs_dir().join(format!("{}.png", hero.id)).exists() && !s.g.thumbs_dir().join(format!("{}.strip.jpg", hero.id)).exists());
        assert!(other_thumb.exists(), "another asset's thumbnail stays");
        let tomb = s.g.get(&hero.id).unwrap().unwrap();
        assert!(tomb.purged && tomb.title == "hero", "a tombstone still answers");
        assert_eq!(s.g.get(&kid.id).unwrap().unwrap().parents, vec![hero.id.clone()], "lineage holds");
        assert!(s.g.list(&ListFilter { include_trash: true, ..Default::default() }).unwrap().iter().all(|r| r.id != hero.id), "purged never lists");
        assert_eq!(s.g.restore(std::slice::from_ref(&hero.id)).unwrap(), 0, "an emptied asset can't come back");
        let col = s.g.collections().unwrap().into_iter().find(|c| c.id == col.id).unwrap();
        assert_eq!((col.asset_ids, col.cover), (vec![kid.id.clone()], None));
        assert!(s.g.task_assets("t1").unwrap().iter().all(|a| a.id != hero.id));
        assert_eq!(s.g.purge_trash().unwrap(), 0);
        let mut grandkid = new("hero 3", Some("t1"));
        grandkid.parents = vec![hero.id.clone()];
        assert!(s.g.insert_asset(grandkid).unwrap_err().contains("emptied"));
    }

    #[test]
    fn open_settles_a_purge_a_crash_cut_short() {
        let s = scratch();
        let (kept, gone) = (add(&s, "kept", Some("t1")), add(&s, "gone", Some("t1")));
        s.g.trash(&[kept.id.clone(), gone.id.clone()]).unwrap();
        // The crash: both files moved to trash/, only `gone` committed as purged.
        for a in [&kept, &gone] {
            fs::rename(s.g.file_path(a), s.g.trash_dir().join(&a.file)).unwrap();
        }
        s.g.conn.lock().execute("UPDATE assets SET purged = 1 WHERE id = ?1", [&gone.id]).unwrap();
        fs::write(s.g.trash_dir().join("stray"), b"x").unwrap();
        let g = Gallery::open(s.g.root()).unwrap();
        assert!(g.file_path(&kept).is_file(), "an uncommitted purge's file goes back");
        assert!(!g.file_path(&gone).exists());
        assert!(fs::read_dir(g.trash_dir()).unwrap().next().is_none());
        assert_eq!(g.task_assets("t1").unwrap().iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), vec!["kept"]);
    }

    #[test]
    fn the_process_cache_opens_once_and_forgets() {
        let s = scratch();
        let id = format!("h-{}", uuid::Uuid::new_v4());
        let g1 = crate::hive::gallery::open_cached(&id, s.g.root()).unwrap();
        let g2 = crate::hive::gallery::open_cached(&id, s.g.root()).unwrap();
        assert!(std::sync::Arc::ptr_eq(&g1, &g2));
        crate::hive::gallery::forget(&id);
        assert!(!crate::hive::gallery::is_open(&id));
        assert!(crate::hive::gallery::for_hive("../x").is_err());
        assert!(crate::hive::gallery::for_hive(&format!("missing-{}", uuid::Uuid::new_v4())).unwrap_err().contains("gone"), "a missing Hive folder isn't re-created");
    }
}
