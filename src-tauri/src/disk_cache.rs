use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::scan::{CompactString, Item, ScanResult};
use crate::global_search::IndexEntry;

/// blob 文件所在目录：~/.flashdir/blobs
fn blobs_dir() -> Option<std::path::PathBuf> {
    let mut p = DiskCache::get_cache_path().ok()?;
    p.pop();
    p.push("blobs");
    Some(p)
}

/// 路径 → blob 文件名（FNV-1a 64 位 + .bin）。
/// 文件名内含 hash，文件头内含原始路径，读取时校验，避免 hash 冲突串数据。
fn blob_file_name(path: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}.bin", hash)
}

const BLOB_MAGIC: &[u8; 4] = b"FDBL";
const BLOB_VERSION: u8 = 1;

/// 组装 blob 文件内容：magic + version + path_len + path + bincode(items)
fn encode_blob(path: &str, payload: &[u8]) -> Vec<u8> {
    let path_bytes = path.as_bytes();
    let mut buf = Vec::with_capacity(7 + path_bytes.len() + payload.len());
    buf.extend_from_slice(BLOB_MAGIC);
    buf.push(BLOB_VERSION);
    buf.extend_from_slice(&(path_bytes.len().min(u16::MAX as usize) as u16).to_le_bytes());
    buf.extend_from_slice(&path_bytes[..path_bytes.len().min(u16::MAX as usize)]);
    buf.extend_from_slice(payload);
    buf
}

/// 校验文件头并取出 payload（路径不匹配/版本不符 → None，当作缓存未命中）
fn decode_blob<'a>(expected_path: &str, raw: &'a [u8]) -> Option<&'a [u8]> {
    if raw.len() < 7 || &raw[..4] != BLOB_MAGIC || raw[4] != BLOB_VERSION {
        return None;
    }
    let path_len = u16::from_le_bytes([raw[5], raw[6]]) as usize;
    if raw.len() < 7 + path_len {
        return None;
    }
    let path = std::str::from_utf8(&raw[7..7 + path_len]).ok()?;
    if path != expected_path {
        return None;
    }
    Some(&raw[7 + path_len..])
}

/// 原子写 blob 文件（tmp + rename）
fn write_blob_file(name: &str, data: &[u8]) -> Result<()> {
    let dir = blobs_dir().ok_or_else(|| anyhow::anyhow!("无法定位 blob 目录"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!("{}.tmp", name));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn delete_blob_file(name: &str) {
    if let Some(dir) = blobs_dir() {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// 清理孤儿 blob 文件（进程中断残留 / 行已被淘汰）
fn cleanup_orphan_blobs(conn: &Connection) {
    let Some(dir) = blobs_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(mut stmt) = conn.prepare("SELECT blob_file FROM scan_meta WHERE blob_file IS NOT NULL") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for name in rows.flatten() {
                keep.insert(name);
            }
        }
    }
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tmp") || !keep.contains(&name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

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
        // 大 blob 写入会让 WAL 短时间涨到数百 MB；checkpoint 后截断到 64MB 以内
        let _ = conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024i64);

        // 目录级磁盘缓存：每个扫描目录一行 blob（bincode(Vec<Item>)）。
        // 早期是"每条目一行 + 多索引"，30 万行的写入要几十秒、读取要数秒；
        // 单行 blob 写入/读取都在百毫秒级（USN 增量本来就持有全量条目，整块重写即可）。
        conn.execute(
            "CREATE TABLE IF NOT EXISTS scan_meta (
                scan_path TEXT PRIMARY KEY,
                dir_mtime INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mft_available INTEGER NOT NULL,
                item_count INTEGER NOT NULL,
                verified_usn INTEGER NOT NULL DEFAULT 0,
                blob_file TEXT
            )",
            [],
        )?;

        // 兼容旧库：补齐列
        let _ = conn.execute(
            "ALTER TABLE scan_meta ADD COLUMN verified_usn INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE scan_meta ADD COLUMN blob_file TEXT", []);
        // 旧库把 blob 内嵌在 data 列：迁移为外部文件成本高，直接丢弃这些行（缓存可重建），
        // 并 DROP 列以释放空间
        let _ = conn.execute("DELETE FROM scan_meta WHERE data IS NOT NULL", []);
        let _ = conn.execute("ALTER TABLE scan_meta DROP COLUMN data", []);

        // 旧版本条目级缓存表不再使用，直接删除以释放磁盘空间
        let _ = conn.execute("DROP TABLE IF EXISTS scan_items", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_scan_items_path", []);
        let _ = conn.execute("DROP INDEX IF EXISTS idx_scan_items_scan_path", []);
        // 无 blob 文件的历史行视为未命中，清理掉
        let _ = conn.execute("DELETE FROM scan_meta WHERE blob_file IS NULL", []);
        cleanup_orphan_blobs(&conn);


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
    /// 读取某目录的缓存（单行 blob：bincode(Vec<Item>)）
    fn load_scan(
        conn: &Connection,
        path: &str,
        dir_mtime: i64,
        ignore_mtime: bool,
    ) -> Option<(ScanResult, i64)> {
        // 元信息 + blob 文件名（blob 本体放在 ~/.flashdir/blobs/，避免占 DB/WAL）
        let row: Option<(i64, i64, i64, Option<String>)> = conn
            .query_row(
                "SELECT dir_mtime, mft_available, verified_usn, blob_file
                 FROM scan_meta WHERE scan_path = ?1",
                params![path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();

        let (cached_mtime, mft_available, verified_usn, blob_file) = row?;
        if !ignore_mtime && cached_mtime < dir_mtime {
            return None;
        }
        let blob_file = blob_file?; // 旧版本遗留行视为未命中

        // 顺序读外部文件（比 SQLite blob 路径快数倍），并校验文件头里的路径
        let raw = {
            let dir = blobs_dir()?;
            std::fs::read(dir.join(&blob_file)).ok()?
        };
        let payload = decode_blob(path, &raw)?;

        let _ = conn.execute(
            "UPDATE scan_meta SET created_at = ?1 WHERE scan_path = ?2",
            params![chrono::Utc::now().timestamp(), path],
        );

        let mut items: Vec<Item> = bincode::deserialize(payload).ok()?;
        // SQLite ORDER BY 需 temp b-tree（30 万行 ~0.7s），内存排序只要 ~15ms
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

    /// 仅供基准测试：最大 blob 对应的路径
    pub fn largest_blob_path_for_bench(&self) -> Option<String> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        conn.query_row(
            "SELECT scan_path FROM scan_meta WHERE blob_file IS NOT NULL ORDER BY size DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 仅供基准测试：读取最大的那份 blob 文件字节
    pub fn raw_largest_blob_for_bench(&self) -> Option<Vec<u8>> {
        let name: String = {
            let guard = self.conn.lock();
            let conn = guard.as_ref()?;
            conn.query_row(
                "SELECT blob_file FROM scan_meta WHERE blob_file IS NOT NULL ORDER BY size DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()?
        };
        let dir = blobs_dir()?;
        std::fs::read(dir.join(name)).ok()
    }

    /// 写入某目录的完整扫描结果（单行 blob）。
    ///
    /// 相比"每条目一行 + 多索引"，大目录写入从数十秒降到百毫秒级，
    /// 读取也不再需要物化几十万行；代价是增量更新要重写整块
    /// （USN 路径本来就持有全量条目，直接整块写回即可）。
    pub fn insert(
        &self,
        path: &str,
        items: &[Item],
        mft_available: bool,
        dir_mtime: i64,
        verified_usn: i64,
    ) -> Result<()> {
        let payload = bincode::serialize(items)?;
        let file_name = blob_file_name(path);
        let encoded = encode_blob(path, &payload);
        let data_len = encoded.len() as u64;

        self.maybe_cleanup(data_len as usize)?;

        // 先落文件再写元信息：文件写失败不会留下"指向不存在数据的新鲜缓存"
        write_blob_file(&file_name, &encoded)?;

        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO scan_meta
             (scan_path, dir_mtime, created_at, size, mft_available, item_count, verified_usn, blob_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                path,
                dir_mtime,
                chrono::Utc::now().timestamp(),
                data_len as i64,
                mft_available as i64,
                items.len() as i64,
                verified_usn.max(0),
                file_name,
            ],
        )?;
        tx.commit()?;

        Self::refresh_size_counter(conn, &self.current_bytes);
        Ok(())
    }


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

        // 从 blob 载入父缓存（同时拿到父的已校验 USN）
        let (parent, parent_usn) = Self::load_scan(conn, &scan_path, 0, true)?;
        let verified_usn = verified_usn.max(parent_usn);

        // 子目录本身必须作为目录条目存在于父结果中
        if !parent
            .items
            .iter()
            .any(|i| i.is_dir && i.path.as_str() == child_path)
        {
            return None;
        }


        let mut items: Vec<Item> = parent
            .items
            .into_iter()
            .filter(|i| i.path.starts_with(prefix.as_str()))
            .collect();
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
        let expired: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT blob_file FROM scan_meta WHERE created_at < ?1 AND blob_file IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![cutoff.timestamp()], |row| row.get::<_, String>(0))?;
            rows.flatten().collect()
        };
        conn.execute(
            "DELETE FROM scan_meta WHERE created_at < ?1",
            params![cutoff.timestamp()],
        )?;
        for blob in expired {
            delete_blob_file(&blob);
        }

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
        let victims = {
            let mut stmt = tx.prepare(
                "SELECT scan_path, size, blob_file FROM scan_meta ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;

            let mut victims: Vec<(String, Option<String>)> = Vec::new();
            let mut remaining = projected;
            for row in rows {
                if remaining <= target {
                    break;
                }
                let (victim_path, victim_size, victim_blob) = row?;
                remaining = remaining.saturating_sub(victim_size.max(0) as u64);
                victims.push((victim_path, victim_blob));
            }

            for (victim, _) in &victims {
                tx.execute("DELETE FROM scan_meta WHERE scan_path = ?1", params![victim])?;
            }
            victims
        };
        tx.commit()?;
        for (_, blob) in victims {
            if let Some(name) = blob {
                delete_blob_file(&name);
            }
        }

        Self::refresh_size_counter(conn, &self.current_bytes);
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let guard = self.conn.lock();
        let conn = guard.as_ref().ok_or_else(Self::disabled_err)?;
        let blobs: Vec<String> = {
            let mut stmt =
                conn.prepare("SELECT blob_file FROM scan_meta WHERE blob_file IS NOT NULL")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.flatten().collect()
        };
        conn.execute("DELETE FROM scan_meta", [])?;
        for blob in blobs {
            delete_blob_file(&blob);
        }
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
            "SELECT path, name_lower, size, is_dir, mtime FROM global_index",
        )?;
        let entries = stmt
            .query_map([], |row| {
                Ok(IndexEntry {
                    path: row.get(0)?,
                    name_lower: row.get(1)?,
                    size: row.get(2)?,
                    is_dir: row.get::<_, i64>(3)? != 0,
                    mtime: row.get(4)?,
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
                        "",
                        e.name_lower,
                        "",
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
                    "",
                    entry.name_lower,
                    "",
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
