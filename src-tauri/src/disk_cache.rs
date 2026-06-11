use anyhow::Result;
use chrono;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use lazy_static::lazy_static;

use crate::scan::ScanResult;

/// 磁盘缓存管理器
pub struct DiskCache {
    conn: Mutex<Connection>,
    max_size_mb: usize,
    current_size_mb: Mutex<usize>,
}

lazy_static! {
    static ref DISK_CACHE: Arc<DiskCache> = Arc::new(
        DiskCache::new().expect("Failed to initialize disk cache")
    );
}

impl DiskCache {
    pub fn instance() -> Arc<DiskCache> {
        DISK_CACHE.clone()
    }

    pub fn new() -> Result<Self> {
        let cache_path = Self::get_cache_path()?;

        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&cache_path)?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS scan_cache (
                path TEXT PRIMARY KEY,
                data BLOB NOT NULL,
                dir_mtime INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                size INTEGER NOT NULL,
                item_count INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_created_at ON scan_cache(created_at)",
            [],
        )?;

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

        let current_size: i64 = conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM scan_cache", [], |row| row.get(0))
            .unwrap_or(0);

        let cache = Self {
            conn: Mutex::new(conn),
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
        let conn = self.conn.lock();

        let result: Option<(Vec<u8>, i64)> = conn
            .query_row(
                "SELECT data, dir_mtime FROM scan_cache WHERE path = ?1",
                params![path],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .ok()
            .flatten();

        if let Some((data, cached_mtime)) = result {
            if cached_mtime >= dir_mtime {
                let _ = conn.execute(
                    "UPDATE scan_cache SET created_at = ?1 WHERE path = ?2",
                    params![chrono::Utc::now().timestamp(), path],
                );

                return bincode::deserialize(&data).ok();
            }
        }

        None
    }

    /// 获取缓存的扫描结果，忽略 mtime 检查（用于 USN 增量更新）
    /// 返回即使缓存已过期也能使用的数据
    pub fn get_stale(&self, path: &str) -> Option<ScanResult> {
        let conn = self.conn.lock();

        let data: Option<Vec<u8>> = conn
            .query_row(
                "SELECT data FROM scan_cache WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();

        data.and_then(|d| bincode::deserialize(&d).ok())
    }

    pub fn insert(&self, path: &str, result: &ScanResult, dir_mtime: i64) -> Result<()> {
        let data = bincode::serialize(result)?;
        let size = data.len();
        let item_count = result.items.len();

        self.maybe_cleanup(size)?;

        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO scan_cache (path, data, dir_mtime, created_at, size, item_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                path,
                data,
                dir_mtime,
                chrono::Utc::now().timestamp(),
                size,
                item_count
            ],
        )?;

        let mut current = self.current_size_mb.lock();
        *current += size / 1024 / 1024;

        Ok(())
    }

    fn cleanup_old_entries(&self) -> Result<()> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM scan_cache WHERE created_at < ?1",
            params![cutoff.timestamp()],
        )?;

        Ok(())
    }

    fn maybe_cleanup(&self, new_entry_size: usize) -> Result<()> {
        let max_bytes = self.max_size_mb * 1024 * 1024;
        let new_size = *self.current_size_mb.lock() * 1024 * 1024 + new_entry_size;

        if new_size > max_bytes {
            let conn = self.conn.lock();

            let to_remove = (new_size - max_bytes + max_bytes / 4) / 1024 / 1024;

            conn.execute(
                "DELETE FROM scan_cache WHERE path IN (
                    SELECT path FROM scan_cache ORDER BY created_at ASC LIMIT ?1
                )",
                params![to_remove.max(1)],
            )?;
        }

        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM scan_cache", [])?;
        *self.current_size_mb.lock() = 0;
        Ok(())
    }

    pub fn get_stats(&self) -> CacheStats {
        let conn = self.conn.lock();

        let (entry_count, total_size): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM scan_cache",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        let oldest_entry: Option<i64> = conn
            .query_row(
                "SELECT MIN(created_at) FROM scan_cache",
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
        }
    }

    pub fn invalidate(&self, path: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM scan_cache WHERE path = ?1 OR path LIKE ?2",
            params![path, format!("{}%", path)],
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

        let conn = self.conn.lock();
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
        let conn = self.conn.lock();
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
        let conn = self.conn.lock();
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
        let conn = self.conn.lock();
        conn.execute("DELETE FROM snapshots WHERE id = ?1", params![id])?;
        Ok(())
    }
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
}
