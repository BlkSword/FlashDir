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
    /// 当前缓存字节数（以字节为单位维护，删除时同步回退）
    current_bytes: Mutex<u64>,
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
            current_bytes: Mutex::new(0),
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
                item_count INTEGER NOT NULL,
                verified_usn INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        // 兼容旧库：缺少 verified_usn 列时补上
        let _ = conn.execute(
            "ALTER TABLE scan_meta ADD COLUMN verified_usn INTEGER NOT NULL DEFAULT 0",
            [],
        );

        conn.execute(
            "CREATE TABLE IF NOT EXISTS scan_items (
                scan_path TEXT NOT NULL,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                size INTEGER NOT NULL,
                is_dir INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                PRIMARY KEY (scan_path, path)
            )",
            [],
        )?;

        // 兼容旧库：size_formatted 不再持久化（按需格式化即可）
        let _ = conn.execute("ALTER TABLE scan_items DROP COLUMN size_formatted", []);

        // 主键 (scan_path, path) 已覆盖 `WHERE scan_path=? [AND path LIKE ?]` 前缀查询；
        // 多余索引会让每次缓存写入多 2 次 B-tree 插入（实测 200k 行 18s → 7.7s）
        let _ = conn.execute("DROP INDEX IF EXISTS idx_scan_items_path", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_scan_items_scan_path", []);

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
                ext TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL,
                is_dir INTEGER NOT NULL,
                drive TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;

        // 兼容旧库：缺少 ext 列时补上
        let _ = conn.execute("ALTER TABLE global_index ADD COLUMN ext TEXT NOT NULL DEFAULT ''", []);

        // 全局索引只按主键(path)读写或全表加载：name_lower / drive 索引从未被查询使用
        let _ = conn.execute("DROP INDEX IF EXISTS idx_global_index_name_lower", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_global_index_drive", []);

        // 全局索引元数据（是否为全盘构建 / 盘符列表），用于重启后恢复状态语义
        conn.execute(
            "CREATE TABLE IF NOT EXISTS index_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;

        let current_size: i64 = conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM scan_meta", [], |row| row.get(0))
            .unwrap_or(0);

        let cache = Self {
            conn: Mutex::new(Some(conn)),
            max_size_mb: 500,
            current_bytes: Mutex::new(current_size.max(0) as u64),
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

    /// 按目录 mtime 读取缓存，返回 (扫描结果, 已校验 USN)
    pub fn get_with_usn(&self, path: &str, dir_mtime: i64) -> Option<(ScanResult, i64)> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        Self::load_scan(conn, path, dir_mtime, false)
    }

    /// 忽略 mtime 检查读取缓存（用于 USN 增量校验的基底）
    pub fn get_stale_with_usn(&self, path: &str) -> Option<(ScanResult, i64)> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        Self::load_scan(conn, path, 0, true)
    }

    /// 只读取"已校验 USN"，避免为了判断是否需要增量而加载全部条目
    pub fn verified_usn(&self, path: &str) -> Option<i64> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        conn.query_row(
            "SELECT verified_usn FROM scan_meta WHERE scan_path = ?1",
            params![path],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 估算一批条目在缓存中的字节占用（含固定开销）
    fn estimate_items_size(items: &[Item]) -> usize {
        items
            .iter()
            .map(|i| i.path.len() + i.name.len() + 40)
            .sum::<usize>()
            + 128
    }

    /// USN 增量写回：只写变更的行，避免为了几十条变更重写整份目录缓存
    /// （百万级条目的整份重写会带来秒级延迟与大量 WAL 写入）。
    ///
    /// 返回 `Ok(false)` 表示基底行已不存在（缓存被淘汰），调用方应回退整份写入。
    pub fn apply_items_delta(
        &self,
        path: &str,
        removed_paths: &[String],
        upserted: &[Item],
        mft_available: bool,
        dir_mtime: i64,
        verified_usn: i64,
    ) -> Result<bool> {
        let added_size = Self::estimate_items_size(upserted) as i64;
        let removed_size: i64 = removed_paths
            .iter()
            .map(|p| (p.len() + 48) as i64)
            .sum();

        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;

        let base_exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM scan_meta WHERE scan_path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        if base_exists == 0 {
            // 事务未提交，drop 时自动回滚
            return Ok(false);
        }

        {
            let mut del = tx.prepare(
                "DELETE FROM scan_items WHERE scan_path = ?1 AND path = ?2",
            )?;
            for removed in removed_paths {
                del.execute(params![path, removed.as_str()])?;
            }

            let mut upsert = tx.prepare(
                "INSERT OR REPLACE INTO scan_items
                 (scan_path, path, name, size, is_dir, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for item in upserted {
                upsert.execute(params![
                    path,
                    item.path.as_str(),
                    item.name.as_str(),
                    item.size,
                    item.is_dir as i64,
                    item.mtime,
                ])?;
            }
        }

        // item_count / size 为近似值（用于统计与容量控制，不参与正确性）
        let item_delta = upserted.len() as i64 - removed_paths.len() as i64;
        tx.execute(
            "UPDATE scan_meta
             SET dir_mtime = ?1,
                 mft_available = ?2,
                 verified_usn = ?3,
                 created_at = ?4,
                 item_count = MAX(item_count + ?5, 0),
                 size = MAX(size + ?6, 0)
             WHERE scan_path = ?7",
            params![
                dir_mtime,
                mft_available as i64,
                verified_usn.max(0),
                chrono::Utc::now().timestamp(),
                item_delta,
                added_size - removed_size,
                path,
            ],
        )?;

        tx.commit()?;
        Self::refresh_size_counter(conn, &self.current_bytes);
        Ok(true)
    }

    /// USN 校验通过后刷新有效期与已校验 USN（不重写条目）
    pub fn touch_meta(&self, path: &str, dir_mtime: i64, verified_usn: i64) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute(
            "UPDATE scan_meta
             SET dir_mtime = ?1, verified_usn = MAX(verified_usn, ?2), created_at = ?3
             WHERE scan_path = ?4",
            params![dir_mtime, verified_usn, chrono::Utc::now().timestamp(), path],
        )?;
        Ok(())
    }

    fn load_scan(
        conn: &Connection,
        path: &str,
        dir_mtime: i64,
        ignore_mtime: bool,
    ) -> Option<(ScanResult, i64)> {
        let meta: Option<(i64, i64, i64)> = conn
            .query_row(
                "SELECT dir_mtime, mft_available, verified_usn FROM scan_meta WHERE scan_path = ?1",
                params![path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();

        let (cached_mtime, mft_available, verified_usn) = meta?;
        if !ignore_mtime && cached_mtime < dir_mtime {
            return None;
        }

        let _ = conn.execute(
            "UPDATE scan_meta SET created_at = ?1 WHERE scan_path = ?2",
            params![chrono::Utc::now().timestamp(), path],
        );

        let mut stmt = conn
            .prepare(
                "SELECT path, name, size, is_dir, mtime
                 FROM scan_items WHERE scan_path = ?1",
            )
            .ok()?;

        let rows = stmt
            .query_map(params![path], |row| {
                Ok(Item {
                    path: CompactString::from(row.get::<_, String>(0)?),
                    name: CompactString::from(row.get::<_, String>(1)?),
                    size: row.get(2)?,
                    // 不再持久化格式化文本：调用方按需 format_size
                    size_formatted: CompactString::new(),
                    is_dir: row.get::<_, i64>(3)? != 0,
                    mtime: row.get(4)?,
                })
            })
            .ok()?;

        let mut items: Vec<Item> = rows.filter_map(|r| r.ok()).collect();
        // SQLite 的 ORDER BY 会走 temp b-tree（实测 30 万行 ~0.7s），
        // 内存排序只要 ~15ms，且调用方本来就要求 size 降序。
        items.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();

        Some((
            ScanResult {
                items,
                total_size,
                total_size_formatted: crate::scan::format_size(total_size),
                scan_time: 0.0,
                path: CompactString::from(path),
                mft_available: mft_available != 0,
                timing: None,
                perf_metrics: None,
            },
            verified_usn.max(0),
        ))
    }

    /// 写入某目录的完整扫描结果。
    /// `verified_usn` 表示这份数据已被 USN 校验到的位置（0 = 未知）。
    pub fn insert(
        &self,
        path: &str,
        items: &[Item],
        mft_available: bool,
        dir_mtime: i64,
        verified_usn: i64,
    ) -> Result<()> {
        let data_size: usize = Self::estimate_items_size(items);

        self.maybe_cleanup(data_size)?;

        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;

        tx.execute("DELETE FROM scan_items WHERE scan_path = ?1", params![path])?;
        tx.execute("DELETE FROM scan_meta WHERE scan_path = ?1", params![path])?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO scan_items
                 (scan_path, path, name, size, is_dir, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for item in items {
                stmt.execute(params![
                    path,
                    item.path.as_str(),
                    item.name.as_str(),
                    item.size,
                    item.is_dir as i64,
                    item.mtime,
                ])?;
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO scan_meta
             (scan_path, dir_mtime, created_at, size, mft_available, item_count, verified_usn)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                path,
                dir_mtime,
                chrono::Utc::now().timestamp(),
                data_size as i64,
                mft_available as i64,
                items.len() as i64,
                verified_usn.max(0),
            ],
        )?;

        tx.commit()?;

        // 以数据库真实值为准同步内存计数，避免长期漂移
        if let Ok(total) = conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM scan_meta",
            [],
            |row| row.get::<_, i64>(0),
        ) {
            *self.current_bytes.lock() = total.max(0) as u64;
        }

        Ok(())
    }

    /// 从上层目录的磁盘缓存推导子目录结果。
    ///
    /// 新鲜度：仅当"父缓存写入时间 >= 子目录自身 mtime"时才允许推导，
    /// 否则子目录可能在上层扫描之后发生过变更，必须走完整的缓存/USN/扫描链路。
    /// 返回 (扫描结果, 父缓存已校验 USN)。
    pub fn get_derived(&self, child_path: &str, dir_mtime: i64) -> Option<(ScanResult, i64)> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;

        let mut stmt = conn
            .prepare("SELECT scan_path, mft_available, created_at, verified_usn FROM scan_meta")
            .ok()?;
        let metas: Vec<(String, i64, i64, i64)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .ok()?
            .filter_map(|r| r.ok())
            .collect();

        let mut best: Option<(String, i64, i64, i64)> = None;
        for (scan_path, mft_available, created_at, verified_usn) in metas {
            if scan_path.len() < child_path.len() && is_same_or_child(&scan_path, child_path) {
                if best
                    .as_ref()
                    .map_or(true, |b| scan_path.len() > b.0.len())
                {
                    best = Some((scan_path, mft_available, created_at, verified_usn));
                }
            }
        }

        let (scan_path, mft_available, created_at, verified_usn) = best?;
        // 父缓存写入时间早于子目录 mtime：可能已过期，不用推导
        if created_at < dir_mtime {
            return None;
        }
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
                "SELECT path, name, size, is_dir, mtime
                 FROM scan_items
                 WHERE scan_path = ?1 AND path LIKE ?2 ESCAPE '\\'",
            )
            .ok()?;

        let rows = stmt
            .query_map(params![scan_path, like], |row| {
                Ok(Item {
                    path: CompactString::from(row.get::<_, String>(0)?),
                    name: CompactString::from(row.get::<_, String>(1)?),
                    size: row.get(2)?,
                    // 不再持久化格式化文本：调用方按需 format_size
                    size_formatted: CompactString::new(),
                    is_dir: row.get::<_, i64>(3)? != 0,
                    mtime: row.get(4)?,
                })
            })
            .ok()?;

        let mut items: Vec<Item> = rows.filter_map(|r| r.ok()).collect();
        // SQLite 的 ORDER BY 会走 temp b-tree（实测 30 万行 ~0.7s），
        // 内存排序只要 ~15ms，且调用方本来就要求 size 降序。
        items.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();

        Some((
            ScanResult {
                items,
                total_size,
                total_size_formatted: crate::scan::format_size(total_size),
                scan_time: 0.0,
                path: CompactString::from(child_path),
                mft_available: mft_available != 0,
                timing: None,
                perf_metrics: None,
            },
            verified_usn.max(0),
        ))
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

        Self::refresh_size_counter(conn, &self.current_bytes);
        Ok(())
    }

    /// 以数据库为准刷新内存中的缓存字节计数
    fn refresh_size_counter(conn: &Connection, counter: &Mutex<u64>) {
        if let Ok(total) = conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM scan_meta",
            [],
            |row| row.get::<_, i64>(0),
        ) {
            *counter.lock() = total.max(0) as u64;
        }
    }

    /// 容量控制：超限时按最久未访问顺序删除整份目录缓存。
    ///
    /// 早期实现把"需要回收的 MB 数"当作行数 LIMIT 使用，且删除后不回退
    /// 内存计数，导致计数只增不减、之后每次写入都触发淘汰（缓存抖动）。
    fn maybe_cleanup(&self, new_entry_size: usize) -> Result<()> {
        let max_bytes = (self.max_size_mb as u64) * 1024 * 1024;
        let current = *self.current_bytes.lock();
        let projected = current.saturating_add(new_entry_size as u64);
        if projected <= max_bytes {
            return Ok(());
        }

        // 回收到容量的 75%，留出 25% 余量，避免每次写入都触发淘汰
        let target = max_bytes - max_bytes / 4;

        let mut guard = self.conn.lock();
        let Some(conn) = guard.as_mut() else {
            return Ok(());
        };

        let tx = conn.transaction()?;
        {
            let mut stmt =
                tx.prepare("SELECT scan_path, size FROM scan_meta ORDER BY created_at ASC")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;

            let mut victims: Vec<String> = Vec::new();
            let mut remaining = projected;
            for row in rows {
                if remaining <= target {
                    break;
                }
                let (victim_path, victim_size) = row?;
                remaining = remaining.saturating_sub(victim_size.max(0) as u64);
                victims.push(victim_path);
            }

            for victim in &victims {
                tx.execute(
                    "DELETE FROM scan_items WHERE scan_path = ?1",
                    params![victim],
                )?;
                tx.execute("DELETE FROM scan_meta WHERE scan_path = ?1", params![victim])?;
            }
        }
        tx.commit()?;

        Self::refresh_size_counter(conn, &self.current_bytes);
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute("DELETE FROM scan_items", [])?;
        conn.execute("DELETE FROM scan_meta", [])?;
        *self.current_bytes.lock() = 0;
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
        Self::refresh_size_counter(conn, &self.current_bytes);
        Ok(())
    }

    // ─── 快照操作 ──────────────────────────────────────────

    /// 保存一次扫描结果作为快照。
    ///
    /// 只序列化条目列表（`Vec<Item>`），总量/格式化文本等元信息以列形式存储，
    /// 避免整包 `ScanResult` 里的重复字段。
    pub fn insert_snapshot(
        &self,
        path: &str,
        items: &[Item],
        total_size: i64,
        total_size_formatted: &str,
        file_count: usize,
        dir_count: usize,
    ) -> Result<i64> {
        let data = bincode::serialize(items)?;
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
                total_size,
                total_size_formatted,
                items.len(),
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
    /// 读取快照完整数据。
    /// 新格式只存 `Vec<Item>`，总数/路径等来自 meta 列；
    /// 同时兼容旧版本存储的整包 `ScanResult`。
    pub fn get_snapshot(&self, id: i64) -> Option<ScanResult> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;

        let row: Option<(String, i64, String, Vec<u8>)> = conn
            .query_row(
                "SELECT path, total_size, total_size_formatted, data FROM snapshots WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();

        let (path, total_size, size_formatted, data) = row?;

        if let Ok(items) = bincode::deserialize::<Vec<Item>>(&data) {
            return Some(ScanResult {
                items,
                total_size,
                total_size_formatted: CompactString::from(size_formatted.as_str()),
                scan_time: 0.0,
                path: CompactString::from(path.as_str()),
                mft_available: false,
                timing: None,
                perf_metrics: None,
            });
        }

        // 旧格式回退
        bincode::deserialize::<ScanResult>(&data).ok()
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
            "SELECT path, name, name_lower, ext, size, is_dir, mtime FROM global_index",
        )?;
        let entries = stmt
            .query_map([], |row| {
                Ok(IndexEntry {
                    path: row.get(0)?,
                    name: row.get(1)?,
                    name_lower: row.get(2)?,
                    ext: row.get(3)?,
                    size: row.get(4)?,
                    is_dir: row.get::<_, i64>(5)? != 0,
                    mtime: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    /// 流式全量重建：避免把整个内存索引 clone 成一个大 Vec 再写入。
    pub fn save_global_index_stream(
        &self,
        rx: std::sync::mpsc::Receiver<Vec<IndexEntry>>,
    ) -> Result<()> {
        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM global_index", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO global_index
                 (path, name, name_lower, ext, size, is_dir, drive, mtime, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let now = chrono::Utc::now().timestamp();
            for chunk in rx {
                for e in chunk {
                    let drive = Self::extract_drive(&e.path).unwrap_or('?').to_string();
                    stmt.execute(params![
                        e.path,
                        e.name,
                        e.name_lower,
                        e.ext,
                        e.size,
                        e.is_dir as i64,
                        drive,
                        e.mtime,
                        now,
                    ])?;
                }
            }
        }
        tx.commit()?;
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
                 (path, name, name_lower, ext, size, is_dir, drive, mtime, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let now = chrono::Utc::now().timestamp();
            for entry in entries {
                let drive = Self::extract_drive(&entry.path).unwrap_or('?').to_string();
                stmt.execute(params![
                    entry.path,
                    entry.name,
                    entry.name_lower,
                    entry.ext,
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

    /// 保存全局索引元数据（JSON），用于重启后恢复"是否全盘构建"等语义
    pub fn save_index_meta(&self, meta_json: &str) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        conn.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('meta', ?1)",
            params![meta_json],
        )?;
        Ok(())
    }

    /// 读取全局索引元数据
    pub fn load_index_meta(&self) -> Option<String> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        conn.query_row(
            "SELECT value FROM index_meta WHERE key = 'meta'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
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
