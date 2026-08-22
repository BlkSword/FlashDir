use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::scan::{CompactString, Item, ScanResult};
use crate::global_search::IndexEntry;

/// 磁盘缓存管理器
///
/// 初始化失败（HOME 不可写、DB 文件被占用、杀软干扰等）时降级为
/// "无磁盘缓存"模式：读操作返回 None，写操作返回错误，绝不 panic。
pub struct DiskCache {
    /// SQLite 连接；None 表示降级模式
    conn: Mutex<Option<Connection>>,
    max_size_mb: usize,
    current_size_mb: Mutex<usize>,
}

static DISK_CACHE: OnceLock<Arc<DiskCache>> = OnceLock::new();

impl DiskCache {
    pub fn instance() -> Arc<DiskCache> {
        DISK_CACHE
            .get_or_init(|| {
                Arc::new(Self::new().unwrap_or_else(|e| {
                    eprintln!("[FlashDir] 磁盘缓存初始化失败，降级为无磁盘缓存模式: {e:#}");
                    Self::disabled()
                }))
            })
            .clone()
    }

    /// 构造禁用态缓存（数据库不可用时的降级实例）
    fn disabled() -> Self {
        Self {
            conn: Mutex::new(None),
            max_size_mb: 500,
            current_size_mb: Mutex::new(0),
        }
    }

    fn disabled_err() -> anyhow::Error {
        anyhow::anyhow!("磁盘缓存不可用（初始化失败，已降级为无缓存模式）")
    }

    pub fn new() -> Result<Self> {
        let cache_path = Self::get_cache_path()?;

        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&cache_path)?;

        // 多线程/多进程场景下降低锁冲突；WAL 提升读并发和崩溃安全性
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        // 条目级磁盘缓存：每个扫描结果拆成 meta + items 两表，
        // 避免大目录命中时必须反序列化整个 BLOB。
        conn.execute(
            "CREATE TABLE IF NOT EXISTS scan_meta (
                scan_path TEXT PRIMARY KEY,
                dir_mtime INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mft_available INTEGER NOT NULL,
                item_count INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS scan_items (
                scan_path TEXT NOT NULL,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                size INTEGER NOT NULL,
                size_formatted TEXT NOT NULL,
                is_dir INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                PRIMARY KEY (scan_path, path)
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scan_items_path ON scan_items(path)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scan_items_scan_path ON scan_items(scan_path)",
            [],
        )?;

        // 旧版本整表 BLOB 缓存不再使用，直接删除以释放磁盘空间
        conn.execute("DROP TABLE IF EXISTS scan_cache", [])?;

        // ── 快照表：同一目录的多版本扫描历史 ──
        conn.execute(
            "CREATE TABLE IF NOT EXISTS snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                scan_time INTEGER NOT NULL,
                data BLOB NOT NULL,
                total_size INTEGER NOT NULL,
                total_size_formatted TEXT NOT NULL,
                item_count INTEGER NOT NULL,
                file_count INTEGER NOT NULL,
                dir_count INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_snapshots_path_time ON snapshots(path, scan_time DESC)",
            [],
        )?;

        // ── 全局搜索索引表：持久化全局索引条目 ──
        conn.execute(
            "CREATE TABLE IF NOT EXISTS global_index (
                path TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                name_lower TEXT NOT NULL,
                size INTEGER NOT NULL,
                is_dir INTEGER NOT NULL,
                drive TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_global_index_name_lower ON global_index(name_lower)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_global_index_drive ON global_index(drive)",
            [],
        )?;

        let current_size: i64 = conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM scan_meta", [], |row| row.get(0))
            .unwrap_or(0);

        let cache = Self {
            conn: Mutex::new(Some(conn)),
            max_size_mb: 500,
            current_size_mb: Mutex::new((current_size / 1024 / 1024) as usize),
        };

        cache.cleanup_old_entries()?;

        Ok(cache)
    }

    fn get_cache_path() -> Result<PathBuf> {
        let home_dir = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map_err(|_| anyhow::anyhow!("Cannot get home directory"))?;

        let mut path = PathBuf::from(home_dir);
        path.push(".flashdir");
        path.push("cache_v2.db");
        Ok(path)
    }

    pub fn get(&self, path: &str, dir_mtime: i64) -> Option<ScanResult> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        Self::load_scan(conn, path, dir_mtime, false)
    }

    /// \u83b7\u53d6\u7f13\u5b58\u7684\u626b\u63cf\u7ed3\u679c\uff0c\u5ffd\u7565 mtime \u68c0\u67e5\uff08\u7528\u4e8e USN \u589e\u91cf\u66f4\u65b0\uff09
    pub fn get_stale(&self, path: &str) -> Option<ScanResult> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        Self::load_scan(conn, path, 0, true)
    }

    fn load_scan(conn: &Connection, path: &str, dir_mtime: i64, ignore_mtime: bool) -> Option<ScanResult> {
        let meta: Option<(i64, i64)> = conn
            .query_row(
                "SELECT dir_mtime, mft_available FROM scan_meta WHERE scan_path = ?1",
                params![path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();

        let (cached_mtime, mft_available) = meta?;
        if !ignore_mtime && cached_mtime < dir_mtime {
            return None;
        }

        let _ = conn.execute(
            "UPDATE scan_meta SET created_at = ?1 WHERE scan_path = ?2",
            params![chrono::Utc::now().timestamp(), path],
        );

        let mut stmt = conn
            .prepare(
                "SELECT path, name, size, size_formatted, is_dir, mtime
                 FROM scan_items WHERE scan_path = ?1 ORDER BY size DESC",
            )
            .ok()?;

        let rows = stmt
            .query_map(params![path], |row| {
                Ok(Item {
                    path: CompactString::from(row.get::<_, String>(0)?),
                    name: CompactString::from(row.get::<_, String>(1)?),
                    size: row.get(2)?,
                    size_formatted: CompactString::from(row.get::<_, String>(3)?),
                    is_dir: row.get::<_, i64>(4)? != 0,
                    mtime: row.get(5)?,
                })
            })
            .ok()?;

        let items: Vec<Item> = rows.filter_map(|r| r.ok()).collect();
        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();

        Some(ScanResult {
            items,
            total_size,
            total_size_formatted: crate::scan::format_size(total_size),
            scan_time: 0.0,
            path: CompactString::from(path),
            mft_available: mft_available != 0,
            timing: None,
            perf_metrics: None,
        })
    }

    pub fn insert(&self, path: &str, result: &ScanResult, dir_mtime: i64) -> Result<()> {
        let data_size: usize = result
            .items
            .iter()
            .map(|i| i.path.len() + i.name.len() + i.size_formatted.len() + 48)
            .sum::<usize>()
            + 128;

        self.maybe_cleanup(data_size)?;

        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;

        tx.execute("DELETE FROM scan_items WHERE scan_path = ?1", params![path])?;
        tx.execute("DELETE FROM scan_meta WHERE scan_path = ?1", params![path])?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO scan_items
                 (scan_path, path, name, size, size_formatted, is_dir, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for item in &result.items {
                stmt.execute(params![
                    path,
                    item.path.as_str(),
                    item.name.as_str(),
                    item.size,
                    item.size_formatted.as_str(),
                    item.is_dir as i64,
                    item.mtime,
                ])?;
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO scan_meta
             (scan_path, dir_mtime, created_at, size, mft_available, item_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                path,
                dir_mtime,
                chrono::Utc::now().timestamp(),
                data_size as i64,
                result.mft_available as i64,
                result.items.len() as i64,
            ],
        )?;

        tx.commit()?;

        let mut current = self.current_size_mb.lock();
        *current += data_size / 1024 / 1024;

        Ok(())
    }

    pub fn get_derived(&self, child_path: &str, _dir_mtime: i64) -> Option<ScanResult> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;

        let mut stmt = conn
            .prepare("SELECT scan_path, mft_available FROM scan_meta")
            .ok()?;
        let metas: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .ok()?
            .filter_map(|r| r.ok())
            .collect();

        let mut best: Option<(String, i64)> = None;
        for (scan_path, mft_available) in metas {
            if scan_path.len() < child_path.len() && is_same_or_child(&scan_path, child_path) {
                if best.as_ref().map_or(true, |b| scan_path.len() > b.0.len()) {
                    best = Some((scan_path, mft_available));
                }
            }
        }

        let (scan_path, mft_available) = best?;
        let prefix = {
            let trimmed = child_path.trim_end_matches('/');
            if trimmed.is_empty() {
                "/".to_string()
            } else {
                format!("{}/", trimmed)
            }
        };

        let child_exists: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM scan_items
                    WHERE scan_path = ?1 AND path = ?2 AND is_dir = 1
                 )",
                params![scan_path, child_path],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !child_exists {
            return None;
        }

        let like = format!("{}%", escape_like(&prefix));
        let mut stmt = conn
            .prepare(
                "SELECT path, name, size, size_formatted, is_dir, mtime
                 FROM scan_items
                 WHERE scan_path = ?1 AND path LIKE ?2 ESCAPE '\\'
                 ORDER BY size DESC",
            )
            .ok()?;

        let rows = stmt
            .query_map(params![scan_path, like], |row| {
                Ok(Item {
                    path: CompactString::from(row.get::<_, String>(0)?),
                    name: CompactString::from(row.get::<_, String>(1)?),
                    size: row.get(2)?,
                    size_formatted: CompactString::from(row.get::<_, String>(3)?),
                    is_dir: row.get::<_, i64>(4)? != 0,
                    mtime: row.get(5)?,
                })
            })
            .ok()?;

        let items: Vec<Item> = rows.filter_map(|r| r.ok()).collect();
        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();

        Some(ScanResult {
            items,
            total_size,
            total_size_formatted: crate::scan::format_size(total_size),
            scan_time: 0.0,
            path: CompactString::from(child_path),
            mft_available: mft_available != 0,
            timing: None,
            perf_metrics: None,
        })
    }

    fn cleanup_old_entries(&self) -> Result<()> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

        let guard = self.conn.lock();
        let Some(conn) = guard.as_ref() else {
            return Ok(());
        };
        conn.execute(
            "DELETE FROM scan_items WHERE scan_path IN (
                SELECT scan_path FROM scan_meta WHERE created_at < ?1
             )",
            params![cutoff.timestamp()],
        )?;
        conn.execute(
            "DELETE FROM scan_meta WHERE created_at < ?1",
            params![cutoff.timestamp()],
        )?;

        Ok(())
    }

    fn maybe_cleanup(&self, new_entry_size: usize) -> Result<()> {
        let max_bytes = self.max_size_mb * 1024 * 1024;
        let new_size = *self.current_size_mb.lock() * 1024 * 1024 + new_entry_size;

        if new_size > max_bytes {
            let guard = self.conn.lock();
            let Some(conn) = guard.as_ref() else {
                return Ok(());
            };

            let to_remove = (new_size - max_bytes + max_bytes / 4) / 1024 / 1024;

            conn.execute(
                "DELETE FROM scan_items WHERE scan_path IN (
                    SELECT scan_path FROM scan_meta ORDER BY created_at ASC LIMIT ?1
                 )",
                params![to_remove.max(1)],
            )?;
            conn.execute(
                "DELETE FROM scan_meta WHERE scan_path IN (
                    SELECT scan_path FROM scan_meta ORDER BY created_at ASC LIMIT ?1
                 )",
                params![to_remove.max(1)],
            )?;
        }

        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute("DELETE FROM scan_items", [])?;
        conn.execute("DELETE FROM scan_meta", [])?;
        *self.current_size_mb.lock() = 0;
        Ok(())
    }

    pub fn get_stats(&self) -> CacheStats {
        let guard = self.conn.lock();
        let Some(conn) = guard.as_ref() else {
            return CacheStats {
                entry_count: 0,
                total_size_bytes: 0,
                total_size_mb: 0.0,
                max_size_mb: self.max_size_mb,
                oldest_entry_timestamp: None,
                enabled: false,
            };
        };

        let (entry_count, total_size): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM scan_meta",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        let oldest_entry: Option<i64> = conn
            .query_row(
                "SELECT MIN(created_at) FROM scan_meta",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);

        CacheStats {
            entry_count: entry_count as usize,
            total_size_bytes: total_size as usize,
            total_size_mb: (total_size / 1024 / 1024) as f64,
            max_size_mb: self.max_size_mb,
            oldest_entry_timestamp: oldest_entry,
            enabled: true,
        }
    }

    pub fn invalidate(&self, path: &str) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute(
            "DELETE FROM scan_items WHERE scan_path = ?1 OR scan_path LIKE ?2 ESCAPE '\\'",
            params![path, path_child_pattern(path)],
        )?;
        conn.execute(
            "DELETE FROM scan_meta WHERE scan_path = ?1 OR scan_path LIKE ?2 ESCAPE '\\'",
            params![path, path_child_pattern(path)],
        )?;
        Ok(())
    }

    // ─── 快照操作 ──────────────────────────────────────────

    /// 保存一次扫描结果作为快照
    pub fn insert_snapshot(
        &self,
        path: &str,
        result: &ScanResult,
        file_count: usize,
        dir_count: usize,
    ) -> Result<i64> {
        let data = bincode::serialize(result)?;
        let now = chrono::Utc::now().timestamp();

        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute(
            "INSERT INTO snapshots (path, scan_time, data, total_size, total_size_formatted, item_count, file_count, dir_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                path,
                now,
                data,
                result.total_size,
                result.total_size_formatted.as_str(),
                result.items.len(),
                file_count,
                dir_count,
            ],
        )?;

        let id = conn.last_insert_rowid();

        // 每个路径最多保留 50 个快照
        conn.execute(
            "DELETE FROM snapshots WHERE path = ?1 AND id NOT IN (
                SELECT id FROM snapshots WHERE path = ?1 ORDER BY scan_time DESC LIMIT 50
            )",
            params![path],
        )?;

        // 30 天 TTL
        let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
        conn.execute(
            "DELETE FROM snapshots WHERE scan_time < ?1",
            params![cutoff.timestamp()],
        )?;

        Ok(id)
    }

    /// 列出某路径的所有快照（元数据，不含完整数据）
    pub fn list_snapshots(&self, path: &str) -> Result<Vec<SnapshotInfo>> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        let mut stmt = conn.prepare(
            "SELECT id, path, scan_time, total_size, total_size_formatted, item_count, file_count, dir_count
             FROM snapshots WHERE path = ?1 ORDER BY scan_time DESC LIMIT 50",
        )?;

        let snapshots = stmt
            .query_map(params![path], |row| {
                Ok(SnapshotInfo {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    scan_time: row.get(2)?,
                    total_size: row.get(3)?,
                    total_size_formatted: row.get(4)?,
                    item_count: row.get(5)?,
                    file_count: row.get(6)?,
                    dir_count: row.get(7)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(snapshots)
    }

    /// 获取指定 ID 的快照完整数据
    pub fn get_snapshot(&self, id: i64) -> Option<ScanResult> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;

        let data: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM snapshots WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();

        data.and_then(|d| bincode::deserialize(&d).ok())
    }

    /// 删除指定快照
    pub fn delete_snapshot(&self, id: i64) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute("DELETE FROM snapshots WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ─── 全局搜索索引持久化 ─────────────────────────────────

    /// 加载全部全局索引条目
    pub fn load_global_index(&self) -> Result<Vec<IndexEntry>> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        let mut stmt = conn.prepare(
            "SELECT path, name, name_lower, size, is_dir, mtime FROM global_index",
        )?;
        let entries = stmt
            .query_map([], |row| {
                Ok(IndexEntry {
                    path: row.get(0)?,
                    name: row.get(1)?,
                    name_lower: row.get(2)?,
                    size: row.get(3)?,
                    is_dir: row.get::<_, i64>(4)? != 0,
                    mtime: row.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    /// 全量重建全局索引时批量写入（事务内先清空再插入）
    pub fn save_global_index_batch(&self, entries: &[IndexEntry]) -> Result<()> {
        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM global_index", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO global_index
                 (path, name, name_lower, size, is_dir, drive, mtime, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for e in entries {
                let drive = Self::extract_drive(&e.path).unwrap_or('?').to_string();
                stmt.execute(params![
                    e.path,
                    e.name,
                    e.name_lower,
                    e.size,
                    e.is_dir as i64,
                    drive,
                    e.mtime,
                    chrono::Utc::now().timestamp(),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 单条 upsert（USN 增量同步）
    pub fn upsert_global_index_entry(&self, entry: &IndexEntry) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        let drive = Self::extract_drive(&entry.path).unwrap_or('?').to_string();
        conn.execute(
            "INSERT OR REPLACE INTO global_index
             (path, name, name_lower, size, is_dir, drive, mtime, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.path,
                entry.name,
                entry.name_lower,
                entry.size,
                entry.is_dir as i64,
                drive,
                entry.mtime,
                chrono::Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }

    /// 批量 upsert（USN 增量同步）：单事务写入，避免逐条自动提交带来的 fsync 开销
    pub fn upsert_global_index_entries(&self, entries: &[IndexEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO global_index
                 (path, name, name_lower, size, is_dir, drive, mtime, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let now = chrono::Utc::now().timestamp();
            for entry in entries {
                let drive = Self::extract_drive(&entry.path).unwrap_or('?').to_string();
                stmt.execute(params![
                    entry.path,
                    entry.name,
                    entry.name_lower,
                    entry.size,
                    entry.is_dir as i64,
                    drive,
                    entry.mtime,
                    now,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 批量按绝对路径删除（USN 增量同步）：单事务
    pub fn remove_global_index_by_paths(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM global_index WHERE path = ?1")?;
            for p in paths {
                stmt.execute(params![p])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 按绝对路径删除条目（USN 删除/重命名旧名称）
    pub fn remove_global_index_by_path(&self, path: &str) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute("DELETE FROM global_index WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// 按前缀删除条目（USN 增量失败时重建某路径）
    pub fn remove_global_index_by_prefix(&self, prefix: &str) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute(
            "DELETE FROM global_index WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            params![prefix, path_child_pattern(prefix)],
        )?;
        Ok(())
    }

    /// 清空全局索引
    pub fn clear_global_index(&self) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute("DELETE FROM global_index", [])?;
        Ok(())
    }

    fn extract_drive(path: &str) -> Option<char> {
        let bytes = path.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            Some(bytes[0] as char)
        } else {
            None
        }
    }
}

/// 构造“仅匹配自身与直接子路径”的 SQL LIKE 模式。
/// 避免 `C:/foo` 误匹配 `C:/foobar`。
fn path_child_pattern(path: &str) -> String {
    let escaped = escape_like(path);
    if path.ends_with('/') {
        format!("{}%", escaped)
    } else {
        format!("{}/%", escaped)
    }
}

/// 转义 SQL LIKE 模式中的通配符，避免路径中的 `%` / `_` 被当作通配符。
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 判断 `key` 是否为 `base` 本身或其子路径（路径分隔符已统一为 `/`）。
fn is_same_or_child(base: &str, key: &str) -> bool {
    if key.eq_ignore_ascii_case(base) {
        return true;
    }
    let child_prefix = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{}/", base)
    };
    key.get(..child_prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(&child_prefix))
}

/// 快照元数据（不含完整文件列表）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    pub id: i64,
    pub path: String,
    pub scan_time: i64,
    pub total_size: i64,
    pub total_size_formatted: String,
    pub item_count: usize,
    pub file_count: usize,
    pub dir_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub entry_count: usize,
    pub total_size_bytes: usize,
    pub total_size_mb: f64,
    pub max_size_mb: usize,
    pub oldest_entry_timestamp: Option<i64>,
    /// 磁盘缓存是否可用（false = 初始化失败，已降级为无缓存模式）
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_child_pattern() {
        assert_eq!(path_child_pattern("C:/Users"), "C:/Users/%");
        assert_eq!(path_child_pattern("C:/Users/"), "C:/Users/%");
        assert_eq!(path_child_pattern("C:/"), "C:/%");
    }

    #[test]
    fn test_escape_like() {
        assert_eq!(escape_like(r"C:\Users"), r"C:\\Users");
        assert_eq!(escape_like("100%_done"), r"100\%\_done");
    }
}
