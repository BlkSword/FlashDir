use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
// 2 = Item 增加 atime 字段
// 3 = 落盘形态改为 ItemDisk（去掉派生字段 name / size_formatted）
//
// 版本不匹配即视为缓存未命中（decode_blob 返回 None → 首次访问走全量扫描后重写），
// 语义可接受：blob 是可重建的派生数据，升级后每个目录第一次访问多付一次扫描成本。
// 注意：判定为未命中的行仍留在 scan_meta 里（dir_mtime/verified_usn 仍在），
// 因此不会误删其它有效缓存，也不会把"上次已校验 USN"清零。
const BLOB_VERSION: u8 = 3;

/// 缓存库路径的**环境变量覆盖**：`FLASHDIR_CACHE_DB`。
///
/// 只为测试/基准服务 —— `global_index` 的 schema 收拢**不可逆**（老列被 DROP 后
/// 只能靠重建整份索引恢复），必须先在真实库的**副本**上验证，绝不能拿用户那份
/// ~1GB 的真实索引当试验田。设置后 `DiskCache` 的所有 I/O（含迁移）都指向该文件；
/// 未设置或为空串时行为与旧版完全一致（`%USERPROFILE%\.flashdir\cache_v2.db`）。
const CACHE_DB_ENV: &str = "FLASHDIR_CACHE_DB";

/// `global_index` **新 schema** 的列集合 = `load_global_index` 真正读取的 4 列。
///
/// 老 schema 有 9 列，其中 5 列只写不读，全部收拢掉：
/// - `name` / `name_lower` / `ext`：都能由 `path` 派生（`name_lower_from_path`），
///   载入侧已不再读 `name_lower`，写入侧 `name` / `ext` 本来就恒写空串；
/// - `drive`：无任何查询使用，其索引 `idx_global_index_drive` 早已 DROP；
/// - `created_at`：写入时打时间戳，**从未被读过**（`scan_meta.created_at` 是另一张表，
///   与本表无关）。
const GLOBAL_INDEX_COLUMNS: [&str; 4] = ["path", "size", "is_dir", "mtime"];

/// `index_meta` 中"`global_index` 已收拢到新 schema"的标记键。
///
/// 与列探测（`PRAGMA table_info`）双重判定：标记保证"同一份库不重复迁移"，
/// 列探测保证"标记丢失/写了标记但迁移没落地"时仍能补做一次真实迁移。
const GLOBAL_INDEX_SCHEMA_KEY: &str = "schema_v2";

/// 仅用于落盘的紧凑条目（**不是**对外契约，`scan::Item` 的字段集合与名字都不变）。
///
/// 旧实现直接用 bincode 序列化 `scan::Item`（7 个字段），其中两个是纯派生字段：
/// - `name` 就是 `path` 的最后一段（MFT / 目录遍历 / USN 三条写入路径都满足该不变式）；
/// - `size_formatted` = `format_size(size)`，且 IPC 契约不依赖它：
///   commands.rs 只对当前页与 Top5 重新 `format_size`，前端另有自己的 `formatSize`。
///
/// 实测这两列占 370MB blob 的约两成（388.6MB → 308.4MB），反序列化时每行还多一次
/// `String` 读取。这里只落盘 5 个不可派生字段，载入时再派生 `name`
/// （`SmartString<Compact>` 对 ≤23 字节内联，派生几乎零分配）、`size_formatted` 留空。
#[derive(Serialize, Deserialize)]
struct ItemDisk {
    path: CompactString,
    size: i64,
    is_dir: bool,
    mtime: i64,
    atime: i64,
}

impl ItemDisk {
    #[inline]
    fn into_item(self) -> Item {
        Item {
            // name 由 path 派生，但**不在这里**做：串行解码路径上逐条 rfind 是纯浪费，
            // 统一交给 `fill_derived_names` 在解码后并行补齐。
            name: CompactString::new(),
            path: self.path,
            size: self.size,
            // 派生字段不落盘：展示层（commands.rs 的分页/Top5、前端 formatSize）按需重算
            size_formatted: CompactString::new(),
            is_dir: self.is_dir,
            mtime: self.mtime,
            atime: self.atime,
        }
    }
}

/// 把 `&[Item]` 直接写成 ItemDisk 序列（不额外建 `Vec<ItemDisk>` 中间体：
/// 大目录一份就是 190 万条，临时 Vec 要多占 ~140MB、多一次全量搬运）。
///
/// 字节形态与 `bincode::serialize(&Vec<ItemDisk>)` 完全一致：
/// `Vec` 的序列化本身就是 `serialize_seq(len)` + 逐元素，两侧都用 bincode 顶层接口的同一套配置。
struct ItemDiskSeq<'a>(&'a [Item]);

impl Serialize for ItemDiskSeq<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&ItemDisk {
                path: item.path.clone(),
                size: item.size,
                is_dir: item.is_dir,
                mtime: item.mtime,
                atime: item.atime,
            })?;
        }
        seq.end()
    }
}

/// 直接反序列化成 `Vec<Item>`，避免"先建 `Vec<ItemDisk>` 再转一份 Vec<Item>"的双份内存峰值。
/// 用 bincode 写下的长度前缀做 `Vec::with_capacity` 预分配（seq 的 size_hint）。
/// `name` 由调用方在解码后统一派生（见 `fill_derived_names`）。
struct ItemsFromBlob(Vec<Item>);

impl<'de> Deserialize<'de> for ItemsFromBlob {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ItemsFromBlob;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("紧凑落盘条目序列")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<ItemsFromBlob, A::Error> {
                let mut items: Vec<Item> = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(disk) = seq.next_element::<ItemDisk>()? {
                    items.push(disk.into_item());
                }
                Ok(ItemsFromBlob(items))
            }
        }
        d.deserialize_seq(V)
    }
}

/// 解码后的补齐：`name` 由 `path` 末段派生。
///
/// 单线程逐条 `rfind('/')` 在 C:/ 这份 193 万条的 blob 上要 ~60ms（实测：新形态单线程
/// 解码 736ms vs 旧形态直接从流里读 name 672ms）——派生本身成了新形态的一笔开销，
/// 但它是**纯 CPU、可完美并行**的：按块并行摊到多核后只剩个位数毫秒。
/// 小 blob 不值得付 rayon 的调度开销，低于阈值仍走顺序分支。
fn fill_derived_names(items: &mut [Item]) {
    const PARALLEL_MIN_ITEMS: usize = 8192;
    if items.len() >= PARALLEL_MIN_ITEMS {
        use rayon::prelude::*;
        items
            .par_iter_mut()
            .for_each(|it| it.name = crate::scan::name_from_path(&it.path));
    } else {
        for it in items.iter_mut() {
            it.name = crate::scan::name_from_path(&it.path);
        }
    }
}

/// 解码 blob payload（当前落盘形态）→ `Vec<Item>`，并补齐派生字段 `name`。
fn decode_items_payload(payload: &[u8]) -> Option<Vec<Item>> {
    let mut items = bincode::deserialize::<ItemsFromBlob>(payload).ok()?.0;
    fill_derived_names(&mut items);
    Some(items)
}

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

/// 迁移：为旧库补上 `last_access` 列并回填（幂等）。
///
/// 列已存在时 `ALTER TABLE` 报错，忽略即可（沿用文件里既有的 `let _ = ...` 迁移风格）；
/// 回填用 `created_at`，`WHERE last_access = 0` 保证重复执行不会覆盖真实访问时间。
fn migrate_last_access(conn: &Connection) {
    let _ = conn.execute(
        "ALTER TABLE scan_meta ADD COLUMN last_access INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "UPDATE scan_meta SET last_access = created_at WHERE last_access = 0",
        [],
    );
}

/// 记录一次"读取命中"：只刷新 `last_access`。
///
/// 旧实现在这里改写 `created_at`，使"写入时间"与"最后访问时间"混为一谈，
/// 直接破坏了 `get_derived` 按 `created_at` 判断父缓存新鲜度的语义。
fn mark_accessed(conn: &Connection, path: &str) {
    let _ = conn.execute(
        "UPDATE scan_meta SET last_access = ?1 WHERE scan_path = ?2",
        params![chrono::Utc::now().timestamp(), path],
    );
}

/// 读 `global_index` 的列名（空 = 表不存在）。
fn global_index_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(global_index)")?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(cols)
}

/// 是否为收拢后的新 schema（列集合恰好等于 `GLOBAL_INDEX_COLUMNS`，与顺序无关）。
fn is_new_global_index_schema(cols: &[String]) -> bool {
    cols.len() == GLOBAL_INDEX_COLUMNS.len()
        && GLOBAL_INDEX_COLUMNS
            .iter()
            .all(|need| cols.iter().any(|c| c == need))
}

/// 建新 schema 的 `global_index`（空库 / 表缺失时用）。
fn create_global_index_table(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS global_index (
            path TEXT PRIMARY KEY,
            size INTEGER NOT NULL,
            is_dir INTEGER NOT NULL,
            mtime INTEGER NOT NULL
        )",
        [],
    )?;
    Ok(())
}

/// 落 `schema_v2` 标记。
fn mark_global_index_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO index_meta (key, value) VALUES (?1, '1')",
        params![GLOBAL_INDEX_SCHEMA_KEY],
    )?;
    Ok(())
}

/// 启动路径的**廉价表准备**：只保证 `index_meta` / `global_index` 存在，**绝不动已有表**。
///
/// 与 `migrate_global_index_schema` 的关键区别：这里不做任何表重建、不做 VACUUM。
/// `CREATE TABLE IF NOT EXISTS` 在老库（9 列）上是一条 no-op —— SQLite 只读 schema 元数据、
/// 不碰数据页，因此开销与表大小无关（310 万行也一样是微秒级）。
///
/// 这正是不再有启动冻结的原因：`DiskCache::instance()` 在 main.rs 里于**建窗之前**被调用，
/// 原本它会在 `open_index_conn` → `migrate_global_index_schema` 里同步跑"重建 34s + VACUUM 8s"。
/// 现在启动只建表；schema 收拢改由两条本就要花时间的路径承担：
/// 1. 用户主动重建索引 → `save_global_index_stream`（它第一件事本来就是清空整表 + 全量重灌）；
/// 2. 在那之前，写路径按"当前列集合"自适应（`DiskCache::global_index_is_new_schema`），
///    老库不迁移也能继续读写，行为对调用方完全一致。
fn ensure_global_index_tables(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS index_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )?;
    // 表缺失（全新库）→ 直接是新 schema；表已存在（老 9 列库）→ IF NOT EXISTS 使它原样保留
    create_global_index_table(conn)?;
    Ok(())
}

/// 全局索引相关表与 schema 迁移（幂等）。
///
/// **注意：本函数已不在启动路径上**，`DiskCache::open` / `open_index_conn` 只走
/// `ensure_global_index_tables`。保留为**内部工具**：测试用它验证"重建式迁移"的事务原子性
/// （含强制失败完整回滚），未来若要在空闲时刻做一次性收拢也可以复用它。
///
/// P0-3b：这些语句只在**一处**执行——要么在独立的索引连接上，要么（第二条连接打不开时）
/// 退回主连接补做。两条连接共享同一个 DB 文件，表是库级对象，重复迁移既无意义也会
/// 在启动路径上白付 ALTER/DROP 的代价。
///
/// schema 收拢（9 列 → `GLOBAL_INDEX_COLUMNS`）：用"重建表 + 拷贝 + 改名"而不是
/// `ALTER TABLE DROP COLUMN` —— 重建在一个**事务**里完成，失败即完整回滚，不会留下
/// 半迁移状态；事务提交后再补一次 `VACUUM` 把 freelist 真正还给文件系统
/// （见 `rebuild_global_index_schema` 的实测数据）。
///
/// **不可逆**：老列一旦 DROP 就无法还原，唯一的恢复手段是删掉索引库重建整份索引。
///
/// 失败语义：返回 Err → 索引连接不可用 → 退回主连接再试一次 → 仍失败则整个缓存降级为
/// 无缓存模式。宁可降级也不在半迁移的库上继续读写（老 schema 下写入侧的 4 列 INSERT
/// 会撞上 name/name_lower/drive/created_at 的 NOT NULL，索引将永远无法重建）。
#[allow(dead_code)] // 仅测试/未来工具使用，见上方说明
fn migrate_global_index_schema(conn: &mut Connection) -> Result<()> {
    // index_meta 先建：迁移标记就存在这里
    conn.execute(
        "CREATE TABLE IF NOT EXISTS index_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )?;

    let cols = global_index_columns(conn)?;
    let marked = conn
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            params![GLOBAL_INDEX_SCHEMA_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some();

    // 表缺失（全新库）：直接建新 schema，不算迁移
    if cols.is_empty() {
        create_global_index_table(conn)?;
        mark_global_index_schema(conn)?;
        return Ok(());
    }

    // 已是新 schema：no-op（只补标记；重复执行不会再动表）
    if is_new_global_index_schema(&cols) {
        if !marked {
            mark_global_index_schema(conn)?;
        }
        return Ok(());
    }

    // 老 schema → 一次性收拢。标记只是"快路径"信息，真正决定动作的是列探测：
    // 即使标记丢了（或被误写）也仍会走一次迁移，不会漏。
    rebuild_global_index_schema(conn, &cols)
}

/// 老 schema → 新 schema 的**一次性**收拢：单事务重建 + 事务外 VACUUM。
///
/// 步骤：建新表 → 逐行拷贝 4 列 → DROP 老表（连带其旧索引）→ 改名 → 落标记 → 提交。
/// `Transaction` 在任意一步出错时析构回滚，库里仍是**原样**的老 schema。
///
/// 提交后必须补一次 `VACUUM`：重建只把老表的页退进 freelist，SQLite **不会**在提交时
/// 收缩数据库文件。本机真实库副本实测（3,106,896 行）：`global_index` 表 548.8MB →
/// 414.1MB，而库文件反而从 994.5MB 涨到 1854.9MB；补 `VACUUM`（8.2s）后落到 803.1MB。
/// `VACUUM` 不能在事务里、也不参与迁移原子性：失败只影响空间回收（schema 已是新形态），
/// 因此只记日志、不改变返回值。
///
/// 只在 `migrate_global_index_schema`（内部工具）里用；生产路径的 schema 收拢在
/// `save_global_index_stream`，那里用更简单的"直接 DROP + CREATE"（理由见该函数注释）。
#[allow(dead_code)] // 仅被同样保留为工具的 migrate_global_index_schema 调用
fn rebuild_global_index_schema(conn: &mut Connection, cols: &[String]) -> Result<()> {
    for need in GLOBAL_INDEX_COLUMNS {
        if !cols.iter().any(|c| c == need) {
            anyhow::bail!("global_index 缺少列 {need}（实际列: {cols:?}），拒绝迁移以免丢数据");
        }
    }

    let started = std::time::Instant::now();
    let outcome = (|| -> Result<usize> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "CREATE TABLE global_index_v2 (
                path TEXT PRIMARY KEY,
                size INTEGER NOT NULL,
                is_dir INTEGER NOT NULL,
                mtime INTEGER NOT NULL
            )",
            [],
        )?;
        let copied = tx.execute(
            "INSERT INTO global_index_v2 (path, size, is_dir, mtime)
             SELECT path, size, is_dir, mtime FROM global_index",
            [],
        )?;
        // 老表连同它的索引（idx_global_index_name_lower / _drive）一起消失
        tx.execute("DROP TABLE global_index", [])?;
        tx.execute("ALTER TABLE global_index_v2 RENAME TO global_index", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES (?1, '1')",
            params![GLOBAL_INDEX_SCHEMA_KEY],
        )?;
        tx.commit()?;
        Ok(copied)
    })();

    match outcome {
        Ok(copied) => {
            let rebuild_cost = started.elapsed();
            let vacuum = std::time::Instant::now();
            let vacuum_note = match conn.execute_batch("VACUUM") {
                Ok(()) => {
                    // 顺手把 VACUUM 撑大的 WAL 折回主文件（失败无所谓）
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                    format!("VACUUM {:?}", vacuum.elapsed())
                }
                Err(e) => format!("VACUUM 失败（仅影响空间回收，schema 已生效）: {e:#}"),
            };
            crate::diag::log_line(&format!(
                "[Index] global_index schema 收拢完成: {copied} 行, 老列 {:?} -> {:?}, 重建 {rebuild_cost:?} + {vacuum_note}（不可逆，恢复只能重建索引）",
                cols,
                GLOBAL_INDEX_COLUMNS,
            ));
            Ok(())
        }
        Err(e) => {
            crate::diag::log_line(&format!(
                "[Index] global_index schema 迁移失败，已回滚（库仍是老 schema）: {e:#}"
            ));
            Err(e)
        }
    }
}

/// 从绝对路径派生小写文件名（与 `global_search::name_from_path` 同一套规则）。
///
/// P0-3a'：`global_index.name_lower` 的取值就是"path 末段的小写"，派生可以省掉
/// 310 万行的整列读取（实测 3,104,835 行"派生 == 存储"零不一致）。
/// 与 `name_from_path` 的 `rsplit_once('/')` 语义一致：`"C:/"` → `""`。
///
/// P2：返回值是 `CompactString`（`IndexEntry::name_lower` 的类型）——≤23B 的小写文件名
/// 直接内联在该字段里，不再为每条落一次堆分配。中间仍是一次 `to_lowercase()`（返回 String），
/// `into()` 对 >23B 的会复用那块堆缓冲，对 ≤23B 的则回收它并改为内联。
#[inline]
fn name_lower_from_path(path: &str) -> CompactString {
    match path.rfind('/') {
        Some(i) => path[i + 1..].to_lowercase().into(),
        None => path.to_lowercase().into(),
    }
}

/// 从绝对路径取盘符（`"C:/…"` → `'C'`）。
///
/// 只为**老 schema（9 列）**服务：那一版的 `drive` 列是 NOT NULL，而它从来没有任何读方
/// （其索引 `idx_global_index_drive` 早已 DROP）。新 schema 已无该列，所以这个函数只在
/// `global_index` 仍是 9 列时被调用 —— 它存在的意义就是让**老库在迁移前也能继续写**。
#[inline]
fn extract_drive(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        Some(bytes[0] as char)
    } else {
        None
    }
}

/// 磁盘缓存管理器
///
/// 初始化失败（HOME 不可写、DB 文件被占用、杀软干扰等）时降级为
/// "无磁盘缓存"模式：读操作返回 None，写操作返回错误，绝不 panic。
pub struct DiskCache {
    /// 目录缓存 / 快照连接（scan_meta、snapshots）；None 表示降级模式
    conn: Mutex<Option<Connection>>,
    /// 全局索引专用连接（同一 DB 文件、**独立** SQLite 连接与独立锁）。
    ///
    /// P0-3b：旧实现只有 `conn` 一把锁，`load_global_index`（310 万行全表 SELECT，约 3s）、
    /// `save_global_index_stream`（单事务 DELETE 全表 + 310 万行 INSERT，可达数十秒）、
    /// USN 增量 upsert 全都在这把锁下执行，期间 scan_meta / blob / 快照读写全部排队 ——
    /// 界面翻页与后台缓存写入线程一起卡住。
    /// 拆成两条连接后两者互不阻塞：跨连接冲突交给 SQLite 的 busy_timeout
    /// （WAL 下读不阻塞写；写者之间按 busy_timeout 等待，语义见 with_index 注释）。
    /// 这里**不是**再复制一把 Rust 锁去串行化它们——那正是要消除的耦合。
    index_conn: Mutex<Option<Connection>>,
    max_size_mb: usize,
    /// 当前缓存字节数（以字节为单位维护，删除时同步回退）
    current_bytes: Mutex<u64>,
    /// **本实例打开的这份库**里 `global_index` 是否为收拢后的 4 列 schema。
    ///
    /// 写路径必须按表结构分支（老库 9 列 / 新库 4 列），但绝不能每次写都 `PRAGMA table_info`
    /// （`upsert_global_index_entries` 在 USN 增量同步里是高频调用），所以探测一次就缓存住。
    ///
    /// 刻意**不用进程级 `static OnceLock`**：同一进程会先后打开**不同**的库
    /// （测试/基准用 `FLASHDIR_CACHE_DB` 指向副本、或用户删库后重开），
    /// 进程级缓存会把 A 库的结论套到 B 库上，直接按错误的列集合去 INSERT。
    /// 实例级缓存随 `DiskCache` 生命周期失效，且 `save_global_index_stream` 收拢 schema 后会
    /// 立刻把它刷成 `true`。
    global_index_new_schema: Mutex<Option<bool>>,
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
            index_conn: Mutex::new(None),
            max_size_mb: 500,
            current_bytes: Mutex::new(0),
            global_index_new_schema: Mutex::new(None),
        }
    }

    fn disabled_err() -> anyhow::Error {
        anyhow::anyhow!("磁盘缓存不可用（初始化失败，已降级为无缓存模式）")
    }

    pub fn new() -> Result<Self> {
        Self::open(Self::get_cache_path()?)
    }

    /// 按**显式路径**打开缓存库（`new` 的实参化版本）。
    ///
    /// 测试/基准用它指向真实库的副本：`new()` 走 `get_cache_path()`（含
    /// `FLASHDIR_CACHE_DB` 覆盖），这里绕开全局单例，避免测试之间互相污染。
    fn open(cache_path: PathBuf) -> Result<Self> {
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
                last_access INTEGER NOT NULL DEFAULT 0,
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
        // created_at 早期一列兼两职：写入时间 + "最后访问时间"（load_scan 每次读取都
        // UPDATE created_at = now）。这让 get_derived 的"父缓存是否够新"判断失效——
        // 任何一次读取（含它自己的 load_scan）都会把 created_at 刷成当前时间，
        // 于是父 blob 数据可能停留在很久以前的写入时刻却被判为"足够新"。
        // 这里拆成两列：created_at 只表示写入时间，last_access 表示最后访问时间。
        migrate_last_access(&conn);
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

        // ── 全局搜索索引连接：与目录缓存分离的**第二条**连接 ──
        //
        // 建表只在下面这一处执行一次（两条连接共享同一 DB 文件，表是库级对象，
        // 因此绝不能让两条连接各跑一遍）。第二条连接只负责全局索引与 index_meta。
        //
        // **启动路径不做 schema 迁移**：这里只调 `ensure_global_index_tables`（两条
        // `CREATE TABLE IF NOT EXISTS`）。老库（9 列）上它是一条 no-op，SQLite 只读 schema
        // 不碰数据页，因此 `DiskCache::instance()`（main.rs 在**建窗之前**就调用了它）
        // 不再有"重建 34s + VACUUM 8s ≈ 42s 窗口都不出现"的冻结，只剩微秒级开销。
        // schema 收拢改由 `save_global_index_stream`（用户主动重建索引那条本来就很重的路径）
        // 顺手完成；在那之前，写路径按当前列集合自适应，老库不迁移也能正常读写。
        //
        // 降级语义（与旧实现一致）：只有"主连接上的表准备失败"才会让整个缓存进入
        // conn=None 的降级模式；第二条连接打不开时退回主连接跑索引 I/O，功能不丢。
        let index_conn = match Self::open_index_conn(&cache_path) {
            Ok(ic) => Some(ic),
            Err(e) => {
                eprintln!(
                    "[FlashDir] 全局索引独立连接不可用，索引 I/O 退回主连接（目录缓存不受影响）: {e:#}"
                );
                None
            }
        };
        // 第二条连接不可用：索引相关表在主连接上补做（旧行为，同样不做迁移）
        if index_conn.is_none() {
            ensure_global_index_tables(&conn)?;
        }

        let current_size: i64 = conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM scan_meta", [], |row| row.get(0))
            .unwrap_or(0);

        let cache = Self {
            conn: Mutex::new(Some(conn)),
            index_conn: Mutex::new(index_conn),
            max_size_mb: 500,
            current_bytes: Mutex::new(current_size.max(0) as u64),
            global_index_new_schema: Mutex::new(None),
        };

        cache.cleanup_old_entries()?;

        Ok(cache)
    }

    /// 打开全局索引专用连接并完成它的**表准备**（不迁移 schema）。
    ///
    /// 与主连接共享同一个 DB 文件：WAL 是库级设置（旧库首次打开时已写入），
    /// 这里只需给这条连接单独设置 busy_timeout —— 跨连接写冲突（例如
    /// `save_global_index_stream` 的长事务 vs 目录缓存的一次 scan_meta 写入）
    /// 由 SQLite 自己按 busy_timeout 排队，而不是靠一把 Rust 大锁把所有 I/O 串起来。
    fn open_index_conn(cache_path: &std::path::Path) -> Result<Connection> {
        let conn = Connection::open(cache_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // 索引重建（全表 DELETE + 310 万行 INSERT）与 schema 收拢都会让 WAL 短时间大幅增长：
        // checkpoint 后截断到 64MB 以内，避免 WAL 常驻几百 MB。
        let _ = conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024i64);
        ensure_global_index_tables(&conn)?;
        Ok(conn)
    }

    /// 缓存库路径。
    ///
    /// 默认 `%USERPROFILE%\.flashdir\cache_v2.db`（`HOME` 回退）；
    /// 环境变量 `FLASHDIR_CACHE_DB` 可覆盖（用途见 `CACHE_DB_ENV` 的注释：只给测试/基准
    /// 指向真实库副本用，避免在用户真实索引上试错 schema 迁移）。
    fn get_cache_path() -> Result<PathBuf> {
        if let Some(custom) = std::env::var_os(CACHE_DB_ENV) {
            if !custom.is_empty() {
                return Ok(PathBuf::from(custom));
            }
        }
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

        // 读取只刷新"最后访问时间"：created_at 保持写入时间，
        // 否则 get_derived 用 created_at 判父缓存新鲜度会被这次读取污染。
        mark_accessed(conn, path);

        // 反序列化成本占整份 blob 加载的 ~80%（370MB/193 万条实测 646ms）。
        // 现落盘形态只含 5 个不可派生字段，name 由 path 派生、size_formatted 留空；
        // 版本不匹配（BLOB_VERSION 已 bump 到 3）时 payload 在这里失败 → 当作未命中重扫。
        let mut items: Vec<Item> = decode_items_payload(payload)?;
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

    /// 仅供基准测试：最小的那份 blob 对应的路径。
    /// 用途：锁竞争基准需要一个"便宜"的目录缓存读目标——最大 blob 是 C:/（370MB/193 万条），
    /// 它自身加载就要 0.8s，会掩盖锁等待。
    pub fn smallest_blob_path_for_bench(&self) -> Option<String> {
        let guard = self.conn.lock();
        let conn = guard.as_ref()?;
        conn.query_row(
            "SELECT scan_path FROM scan_meta WHERE blob_file IS NOT NULL ORDER BY size ASC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 仅供基准测试：按**当前落盘形态**解码 blob payload（含 name 派生）。
    /// 形态对照与体积/耗时数据见 `bench_blob_disk_form`。
    pub fn decode_blob_payload_for_bench(payload: &[u8]) -> Option<Vec<Item>> {
        decode_items_payload(payload)
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
        // 落盘用紧凑形态（ItemDisk）：只写不可派生字段，见 ItemDisk 注释
        let payload = bincode::serialize(&ItemDiskSeq(items))?;
        let file_name = blob_file_name(path);
        let encoded = encode_blob(path, &payload);
        let data_len = encoded.len() as u64;

        self.maybe_cleanup(data_len as usize)?;

        // 先落文件再写元信息：文件写失败不会留下"指向不存在数据的新鲜缓存"
        write_blob_file(&file_name, &encoded)?;

        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().timestamp();
        tx.execute(
            "INSERT OR REPLACE INTO scan_meta
             (scan_path, dir_mtime, created_at, last_access, size, mft_available, item_count, verified_usn, blob_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                path,
                dir_mtime,
                // 新写入的行：写入时间与最后访问时间同为当前时间
                now,
                now,
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
        // 只更新 mtime / USN 与"最后访问时间"；created_at 是写入时间，不再被 touch 改写
        conn.execute(
            "UPDATE scan_meta
             SET dir_mtime = ?1, verified_usn = MAX(verified_usn, ?2), last_access = ?3
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
        // 父缓存**写入时间**早于子目录 mtime：可能已过期，不用推导。
        // created_at 现在真的是写入时间（读取只刷 last_access），
        // 早期它被每次读取刷新，这里的判断几乎永远为假 → 会返回陈旧派生结果。
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
        // README 语义："7 天未访问" —— 按 last_access 而不是写入时间判定
        let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

        let guard = self.conn.lock();
        let Some(conn) = guard.as_ref() else {
            return Ok(());
        };
        let expired: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT blob_file FROM scan_meta WHERE last_access < ?1 AND blob_file IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![cutoff.timestamp()], |row| row.get::<_, String>(0))?;
            rows.flatten().collect()
        };
        conn.execute(
            "DELETE FROM scan_meta WHERE last_access < ?1",
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
            // README 语义"按最久未访问整份淘汰"：LRU 依据是 last_access，
            // 早期误用 created_at，而 created_at 又会被每次读取刷新 → 淘汰顺序失真
            let mut stmt = tx.prepare(
                "SELECT scan_path, size, blob_file FROM scan_meta ORDER BY last_access ASC",
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

        // 最早写入的缓存条目时间（字段名与序列化保持不变，供前端/MCP 显示）
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

    /// 在"全局索引连接"上执行一段操作（P0-3b 的核心）。
    ///
    /// 旧实现在这里锁的是目录缓存那把 `conn`：310 万行全表 SELECT / 单事务全表重建
    /// 期间所有 scan_meta、blob、快照读写都得排队。现在索引 I/O 走自己的连接与锁：
    ///
    /// - 互不阻塞的依据：WAL 模式下**读事务不阻塞写事务、写事务也不阻塞读事务**；
    ///   两条连接之间唯一的冲突是"写者 vs 写者"，SQLite 按各自连接的 `busy_timeout`
    ///   排队（本文件两条连接都是 5s）。
    /// - 已知代价（有意接受）：全局索引重建（单事务数十秒）期间，目录缓存的一次
    ///   `insert`/淘汰若超过 busy_timeout 会以 SQLITE_BUSY 失败并只打印一行日志——
    ///   即"这条目录的缓存这次没写成"，下次扫描会重写；而旧行为是缓存写入线程静默
    ///   阻塞同样久，同时界面翻页也跟着卡住。要彻底消除需要把索引重建切成小事务，
    ///   那会牺牲"重建是原子的"，留给 owner 决定。
    /// - 降级语义不变：索引连接缺失时退回主连接；主连接也为 None（初始化失败）时
    ///   与旧实现一样返回"磁盘缓存不可用"。
    fn with_index<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R>) -> Result<R> {
        {
            let mut guard = self.index_conn.lock();
            if let Some(conn) = guard.as_mut() {
                return f(conn);
            }
        }
        let mut guard = self.conn.lock();
        let conn = guard.as_mut().ok_or_else(Self::disabled_err)?;
        f(conn)
    }

    /// 当前 `global_index` 是不是收拢后的 4 列 schema（**每个实例只探测一次**）。
    ///
    /// 探测本身很便宜（`PRAGMA table_info` 只读 schema，与行数无关），但 `upsert_global_index_entries`
    /// 在 USN 增量同步里是高频调用，所以结果缓存在 `global_index_new_schema` 里：
    /// 进程内、每实例一次，之后所有写入都不再问表结构。
    ///
    /// 返回值决定写路径用哪套列：
    /// - `false`（老 9 列库）→ 按老列写，补 `name`/`ext`/`drive`/`created_at`；
    /// - `true`（新 4 列库）→ 只写 `path, size, is_dir, mtime`。
    ///
    /// 表意外缺失（正常路径已由 `ensure_global_index_tables` 建好）时补建成新 schema 并返回 `true`，
    /// 这样写入永远不会撞上"表不存在"。
    fn global_index_is_new_schema(&self, conn: &Connection) -> Result<bool> {
        if let Some(cached) = *self.global_index_new_schema.lock() {
            return Ok(cached);
        }
        let cols = global_index_columns(conn)?;
        let is_new = if cols.is_empty() {
            create_global_index_table(conn)?;
            true
        } else {
            is_new_global_index_schema(&cols)
        };
        *self.global_index_new_schema.lock() = Some(is_new);
        Ok(is_new)
    }

    /// 一次性修复历史畸形索引路径（形如 `C:\root/C:/root/xxx`）。
    ///
    /// 这类脏行是早期版本拼接路径时留下的，占过索引三成：既让搜索结果出现
    /// 奇怪路径与重复项，又让"粘贴完整路径"永远搜不到东西。
    /// 修复策略：截出其中的真实绝对路径；已有正确行就只删脏行（保留较新的元数据），
    /// 没有就按脏行的元数据补写一条。返回（合并数, 补写数）。
    ///
    /// 按**表结构自适应**写列（老库 9 列 / 新库 4 列），对调用方语义完全一致。
    pub fn repair_corrupt_paths(&self) -> Result<(usize, usize)> {
        self.with_index(|conn| {
            let legacy = !self.global_index_is_new_schema(conn)?;
            // 先取出候选行，避免边遍历边改
            let rows: Vec<(String, i64, i64, i64)> = {
                let mut stmt = conn
                    .prepare("SELECT path, size, is_dir, mtime FROM global_index WHERE path LIKE '%/%:/%'")?;
                let mapped = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?;
                mapped.filter_map(|r| r.ok()).collect()
            };
            if rows.is_empty() {
                return Ok((0, 0));
            }

            let tx = conn.transaction()?;
            let mut merged = 0usize;
            let mut written = 0usize;
            {
                let mut del = tx.prepare("DELETE FROM global_index WHERE path = ?1")?;
                if legacy {
                    // 老 schema：9 列都是 NOT NULL，必须把派生列补齐（与收拢前的实现逐字一致）
                    let now = chrono::Utc::now().timestamp();
                    let mut ins = tx.prepare(
                        "INSERT OR IGNORE INTO global_index
                         (path, name, name_lower, ext, size, is_dir, drive, mtime, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    )?;
                    for (bad, size, is_dir, mtime) in &rows {
                        if let Some(fixed) = crate::global_search::repair_corrupt_path(bad) {
                            let name = fixed.rsplit('/').next().unwrap_or(fixed.as_str());
                            let ext = if *is_dir != 0 {
                                String::new()
                            } else {
                                name.rsplit_once('.')
                                    .map(|(_, e)| e.to_lowercase())
                                    .unwrap_or_default()
                            };
                            let drive = extract_drive(&fixed).unwrap_or('?').to_string();
                            let changed = ins.execute(params![
                                fixed,
                                "",
                                name.to_lowercase(),
                                ext,
                                size,
                                is_dir,
                                drive,
                                mtime,
                                now
                            ])?;
                            if changed > 0 {
                                written += 1;
                            } else {
                                merged += 1;
                            }
                        }
                        del.execute([bad.as_str()])?;
                    }
                } else {
                    // 新 schema 只有 4 列：name / name_lower / ext / drive / created_at 都已收拢掉
                    // （name_lower 由 path 末段派生，语义不变）。
                    let mut ins = tx.prepare(
                        "INSERT OR IGNORE INTO global_index
                         (path, size, is_dir, mtime)
                         VALUES (?1, ?2, ?3, ?4)",
                    )?;
                    for (bad, size, is_dir, mtime) in &rows {
                        if let Some(fixed) = crate::global_search::repair_corrupt_path(bad) {
                            let changed = ins.execute(params![fixed, size, is_dir, mtime])?;
                            if changed > 0 {
                                written += 1;
                            } else {
                                merged += 1;
                            }
                        }
                        del.execute([bad.as_str()])?;
                    }
                }
            }
            tx.commit()?;
            Ok((merged, written))
        })
    }

    /// 加载全部全局索引条目（310 万行，载入成本实测拆解见 `bench_load_global_index`）。
    ///
    /// P0-3a' 的两处改动：
    /// 1. `name_lower` 不再从库里读，改由 path 末段派生（省掉每行一次 TEXT 列读取；
    ///    同一条连接上 A/B 实测 2.551s → 2.473s / 2.567s → 2.449s，约 -3~5%，
    ///    真实库 3,104,835 行"派生 == 存储"零不一致，写入侧全部遵守同一不变式）。
    /// 2. 去掉 `query_map(...).filter_map(|r| r.ok())` 的中间层，直接走 `rows.next()`。
    ///
    /// **没有**做 `Vec::with_capacity(COUNT(*))`：实测 `COUNT(*)` 自身要 0.49~0.54s 全表扫描，
    /// 而 310 万条 `IndexEntry` 的几何增长只多搬约 0.1s，预分配是净亏（见同一基准的
    /// A/E 两档数据）。载入成本实测拆解：SQLite 行遍历 ~0.9s、两列 `String` 分配 ~0.8s
    /// （p4 1.71s − p2 0.90s），剩下的才是 struct/Vec 构造。这两块都受制于
    /// `IndexEntry` 的字段形状（每行必须产出两个 `String`），只能由 owner 侧改
    /// （懒构建/并行构建/紧凑索引形态）。
    pub fn load_global_index(&self) -> Result<Vec<IndexEntry>> {
        self.with_index(|conn| {
            let mut stmt = conn.prepare("SELECT path, size, is_dir, mtime FROM global_index")?;
            let mut rows = stmt.query([])?;
            // 不做预分配：见上面注释（COUNT(*) 比省下的搬运更贵）
            let mut entries: Vec<IndexEntry> = Vec::new();
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let name_lower = name_lower_from_path(&path);
                // 走构造函数：path_ascii 由 path 派生（载入是唯一"路径来自磁盘"的入口，
                // 必须在这里算一次，否则热路径又得逐条扫 is_ascii()）
                entries.push(IndexEntry::new(
                    path,
                    name_lower,
                    row.get(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get(3)?,
                ));
            }
            Ok(entries)
        })
    }

    /// 按块流式加载全局索引（供 owner 接线用，当前未被调用）。
    ///
    /// 与 `load_global_index` 的差别：不构造整份 `Vec<IndexEntry>`（310 万条约 500MB 峰值），
    /// 逐块交给调用方直接灌进内存索引，省掉一次大 Vec 的分配与增长搬运。
    /// 注意：`load_persisted` 目前是"先取回整份 Vec 再 upsert"，要吃到这个收益需要
    /// 把 global_search.rs 的载入改成块式消费（这属于跨文件接线，本文件不擅自改）。
    #[allow(dead_code)]
    pub fn for_each_global_index_chunk(
        &self,
        chunk_size: usize,
        mut f: impl FnMut(Vec<IndexEntry>) -> Result<()>,
    ) -> Result<usize> {
        let chunk_size = chunk_size.max(1);
        self.with_index(|conn| {
            let mut stmt = conn.prepare("SELECT path, size, is_dir, mtime FROM global_index")?;
            let mut rows = stmt.query([])?;
            let mut buf: Vec<IndexEntry> = Vec::with_capacity(chunk_size);
            let mut total = 0usize;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let name_lower = name_lower_from_path(&path);
                buf.push(IndexEntry::new(
                    path,
                    name_lower,
                    row.get(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get(3)?,
                ));
                total += 1;
                if buf.len() >= chunk_size {
                    f(std::mem::replace(&mut buf, Vec::with_capacity(chunk_size)))?;
                }
            }
            if !buf.is_empty() {
                f(buf)?;
            }
            Ok(total)
        })
    }

    /// 流式全量重建：避免把整个内存索引 clone 成一个大 Vec 再写入。
    ///
    /// P0-3b：整段（DELETE 全表 + 310 万行 INSERT，单个事务）现在跑在**索引连接**上，
    /// 期间目录缓存的 blob/快照读写不再排队。
    ///
    /// **schema 收拢就挂在这条路径上**：它本来就要清空整表 + 全量重灌（用户主动重建索引），
    /// 而 `DiskCache::instance()` 的启动路径已不再迁移 schema（见 `ensure_global_index_tables`）。
    /// 所以老库（9 列）会在这里被换成收拢后的 4 列：
    ///
    /// - 用"直接 `DROP TABLE` + `CREATE TABLE`"而不是老迁移的"建 `global_index_v2` → 拷 4 列 →
    ///   DROP 老表 → 改名"：本函数第一件事就是全表清空，表里**现有每一行都注定被删掉**，
    ///   把 9 列拷进新表纯属白干（真实库 310 万行 = 那 34s 的重建成本，正是要消灭的开销），
    ///   而 DROP + CREATE 是瞬时的。选择更简单的这条。
    /// - 事务安全：SQLite 的 DDL 参与事务，`DROP`/`CREATE`/`INSERT` 任意一步出错（含调用方
    ///   提前关掉 channel 导致 `for` 循环后的 `?`），`Transaction` 析构即整体回滚，
    ///   老表连同全部数据原样回来——"表被删了但数据没写进去"不会发生。
    /// - 提交后再补一次 `VACUUM`（只能在事务外）：老表那 9 列的页只进 freelist，
    ///   不 VACUUM 的话文件只会涨（实测 994.5MB → 1854.9MB，VACUUM 后 803.1MB）。
    ///   只在这一条"刚收拢过 schema"的分支做——新 schema 分支不需要，DELETE 腾出的页会被
    ///   紧接着的 INSERT 原地复用，白白 VACUUM 8s 是净亏。
    ///   VACUUM 失败只记 diag，不影响本函数契约（"提交后即可被 `load_global_index` 正确读回"）。
    ///
    /// 新 schema 分支只写 4 列：`name_lower` 由 `path` 末段派生（载入侧本来就不读它），
    /// `name` / `ext` / `drive` / `created_at` 从来没有任何读方。
    pub fn save_global_index_stream(
        &self,
        rx: std::sync::mpsc::Receiver<Vec<IndexEntry>>,
    ) -> Result<()> {
        self.with_index(|conn| {
            let legacy = !self.global_index_is_new_schema(conn)?;
            let tx = conn.transaction()?;
            if legacy {
                tx.execute("DROP TABLE global_index", [])?;
                tx.execute(
                    "CREATE TABLE global_index (
                        path TEXT PRIMARY KEY,
                        size INTEGER NOT NULL,
                        is_dir INTEGER NOT NULL,
                        mtime INTEGER NOT NULL
                    )",
                    [],
                )?;
            } else {
                // 已是新 schema：表结构不用动，清空即可（腾出的页会被下面的 INSERT 复用）
                tx.execute("DELETE FROM global_index", [])?;
            }
            {
                let mut stmt = tx.prepare(
                    "INSERT OR REPLACE INTO global_index
                     (path, size, is_dir, mtime)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for chunk in rx {
                    for e in chunk {
                        stmt.execute(params![e.path, e.size, e.is_dir as i64, e.mtime])?;
                    }
                }
            }
            // 全量重建结束时这份库一定是 4 列 schema（老库刚被 DROP+CREATE 换掉），
            // 顺手落标记：与 migrate_global_index_schema 同一套语义（见 GLOBAL_INDEX_SCHEMA_KEY）。
            // 标记只是"快路径"信息，真正判定始终是列探测，所以这里写不写都不影响正确性。
            mark_global_index_schema(&tx)?;
            tx.commit()?;
            if legacy {
                // 缓存必须立刻跟上：否则同一实例后面的 upsert 还会按 9 列去写这张 4 列的表
                *self.global_index_new_schema.lock() = Some(true);

                let vacuum = std::time::Instant::now();
                match conn.execute_batch("VACUUM") {
                    Ok(()) => {
                        // 顺手把 VACUUM 撑大的 WAL 折回主文件（失败无所谓）
                        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                        crate::diag::log_line(&format!(
                            "[Index] 索引重建顺带完成 global_index schema 收拢（4 列）: VACUUM {:?}",
                            vacuum.elapsed()
                        ));
                    }
                    Err(e) => crate::diag::log_line(&format!(
                        "[Index] 索引重建已换成 4 列 schema，但 VACUUM 失败（仅影响空间回收，索引正确性不受影响）: {e:#}"
                    )),
                }
            }
            Ok(())
        })
    }

    /// 批量 upsert（USN 增量同步）：单事务写入，避免逐条自动提交带来的 fsync 开销。
    ///
    /// 按**表结构自适应**写列（见 `global_index_is_new_schema`）：老库 9 列时按老列写
    /// （`name`/`ext` 空串、`drive` 由盘符派生、`created_at` 取当前时间戳），新库只写 4 列。
    /// 两条分支对调用方的可见行为完全一致 —— 老库不需要迁移也能继续写索引。
    pub fn upsert_global_index_entries(&self, entries: &[IndexEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.with_index(|conn| {
            let legacy = !self.global_index_is_new_schema(conn)?;
            let tx = conn.transaction()?;
            {
                if legacy {
                    let now = chrono::Utc::now().timestamp();
                    let mut stmt = tx.prepare(
                        "INSERT OR REPLACE INTO global_index
                         (path, name, name_lower, ext, size, is_dir, drive, mtime, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    )?;
                    for entry in entries {
                        let drive = extract_drive(&entry.path).unwrap_or('?').to_string();
                        stmt.execute(params![
                            entry.path,
                            "",
                            entry.name_lower.as_str(),
                            "",
                            entry.size,
                            entry.is_dir as i64,
                            drive,
                            entry.mtime,
                            now
                        ])?;
                    }
                } else {
                    let mut stmt = tx.prepare(
                        "INSERT OR REPLACE INTO global_index
                         (path, size, is_dir, mtime)
                         VALUES (?1, ?2, ?3, ?4)",
                    )?;
                    for entry in entries {
                        stmt.execute(params![
                            entry.path,
                            entry.size,
                            entry.is_dir as i64,
                            entry.mtime
                        ])?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// 批量按绝对路径删除（USN 增量同步）：单事务
    pub fn remove_global_index_by_paths(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.with_index(|conn| {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare("DELETE FROM global_index WHERE path = ?1")?;
                for p in paths {
                    stmt.execute(params![p])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// 按前缀删除条目（USN 增量失败时重建某路径）
    pub fn remove_global_index_by_prefix(&self, prefix: &str) -> Result<()> {
        self.with_index(|conn| {
            conn.execute(
                "DELETE FROM global_index WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
                params![prefix, path_child_pattern(prefix)],
            )?;
            Ok(())
        })
    }

    /// 保存全局索引元数据（JSON），用于重启后恢复"是否全盘构建"等语义
    pub fn save_index_meta(&self, meta_json: &str) -> Result<()> {
        self.with_index(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('meta', ?1)",
                params![meta_json],
            )?;
            Ok(())
        })
    }

    /// 读取全局索引元数据
    pub fn load_index_meta(&self) -> Option<String> {
        self.with_index(|conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM index_meta WHERE key = 'meta'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })
        .ok()
        .flatten()
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

    // ─── global_index schema 收拢（9 列 → `GLOBAL_INDEX_COLUMNS`） ─────────────

    /// 唯一的临时库路径（测试结束用 `remove_db_files` 清掉整个临时目录）。
    fn temp_db_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flashdir-idx-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("cache_v2.db")
    }

    fn remove_db_files(db: &std::path::Path) {
        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 在缓存实例的**索引连接**上跑一段只读查询（测试用）。
    fn with_index_conn<R>(cache: &DiskCache, f: impl FnOnce(&Connection) -> R) -> R {
        let g = cache.index_conn.lock();
        f(g.as_ref().expect("索引连接可用"))
    }

    /// `sqlite_master` 里某对象的建表 SQL（对象不存在时为空串）。
    fn master_sql(conn: &Connection, name: &str) -> String {
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            params![name],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .unwrap()
        .flatten()
        .unwrap_or_default()
    }

    /// `sqlite_master` 里某名字的对象个数（查残留表/索引用）。
    fn master_count(conn: &Connection, name: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// `global_index` 的 rootpage（老 schema 的 b-tree 根页）。
    fn table_rootpage(conn: &Connection, name: &str) -> i64 {
        conn.query_row(
            "SELECT rootpage FROM sqlite_master WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// 当前 `global_index` 的列（走缓存实例的**索引连接**）。
    fn cache_columns(cache: &DiskCache) -> Vec<String> {
        let g = cache.index_conn.lock();
        global_index_columns(g.as_ref().expect("索引连接可用")).unwrap()
    }

    /// 是否已落 `schema_v2` 标记。
    fn schema_marked(cache: &DiskCache) -> bool {
        let g = cache.index_conn.lock();
        g.as_ref()
            .expect("索引连接可用")
            .query_row(
                "SELECT value FROM index_meta WHERE key = ?1",
                params![GLOBAL_INDEX_SCHEMA_KEY],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap()
            .is_some()
    }

    /// `global_index` 的 rootpage（老 schema 的 b-tree 根页）。
    fn cache_rootpage(cache: &DiskCache) -> i64 {
        let g = cache.index_conn.lock();
        g.as_ref()
            .expect("索引连接可用")
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = 'global_index'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    }

    /// 建老 schema（9 列）的 `global_index` 并插 3 行；`name/name_lower/ext/drive/created_at`
    /// 都填**真实值**，用来证明这些列确实被丢弃而不是"本来就是空串"。
    fn create_legacy_global_index(conn: &Connection) {
        conn.execute(
            "CREATE TABLE global_index (
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
        )
        .unwrap();
        let rows = [
            ("C:/dir/a.TXT", "a.TXT", "a.txt", "txt", 100i64, 0i64, "C", 111i64),
            ("C:/dir/sub", "sub", "sub", "", 0i64, 1i64, "C", 222i64),
            ("D:/目录/文件.log", "文件.log", "文件.log", "log", 7i64, 0i64, "D", 333i64),
        ];
        for (path, name, name_lower, ext, size, is_dir, drive, mtime) in rows {
            conn.execute(
                "INSERT INTO global_index
                 (path, name, name_lower, ext, size, is_dir, drive, mtime, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 4000)",
                params![path, name, name_lower, ext, size, is_dir, drive, mtime],
            )
            .unwrap();
        }
    }

    /// **改造核心 1**：老库（9 列）上 `DiskCache::open()` 不做任何迁移、不碰表。
    ///
    /// 断言口径刻意**不依赖计时**：42s 那种量级要靠 310 万行才复现，单测造不出来。
    /// 这里证明的是"表根本被动过没有"：
    /// - 列集合仍是 9 列（不是收拢后的 4 列）；
    /// - `sqlite_master` 里 `global_index` 的建表 SQL 与打开前**逐字节相同**；
    /// - b-tree 根页不变（重建表必然换根页）；
    /// - 老列的数据原样还在（`name`/`drive` 等派生列未被清空）；
    /// - 没落 `schema_v2` 标记、没有 `global_index_v2` 残留。
    ///
    /// 最后再附一个宽松的耗时上限，作为"启动路径上没有秒级任务"的烟测
    /// （真实量级见 `bench_legacy_startup_and_rebuild_real_db_copy`）。
    #[test]
    fn legacy_db_open_keeps_legacy_schema_untouched() {
        let db = temp_db_path("open-nomigrate");
        let (sql_before, rootpage_before);
        {
            let conn = Connection::open(&db).unwrap();
            create_legacy_global_index(&conn);
            sql_before = master_sql(&conn, "global_index");
            rootpage_before = table_rootpage(&conn, "global_index");
            assert_eq!(global_index_columns(&conn).unwrap().len(), 9, "前置条件：老库 9 列");
        }

        let t = std::time::Instant::now();
        let cache = DiskCache::open(db.clone()).expect("打开老库必须成功（且不迁移）");
        let open_cost = t.elapsed();

        let cols = cache_columns(&cache);
        assert_eq!(cols.len(), 9, "open() 不得改变 schema: {cols:?}");
        assert!(!is_new_global_index_schema(&cols), "老库不得在启动路径被收拢");
        assert_eq!(
            with_index_conn(&cache, |c| master_sql(c, "global_index")),
            sql_before,
            "建表 SQL 必须与打开前逐字节一致（表没被重建）"
        );
        assert_eq!(
            cache_rootpage(&cache),
            rootpage_before,
            "重建表必然换 b-tree 根页；根页不变才说明没重建"
        );
        assert!(!schema_marked(&cache), "启动路径不得落 schema_v2 标记");
        assert_eq!(
            with_index_conn(&cache, |c| master_count(c, "global_index_v2")),
            0,
            "不得留下半迁移的新表"
        );
        // 老列里的真实值仍在（不是被清成空串）
        let (name, drive): (String, String) = with_index_conn(&cache, |c| {
            c.query_row(
                "SELECT name, drive FROM global_index WHERE path = 'C:/dir/a.TXT'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        });
        assert_eq!((name.as_str(), drive.as_str()), ("a.TXT", "C"));
        // 读取路径照旧可用
        assert_eq!(cache.load_global_index().unwrap().len(), 3);

        assert!(
            open_cost < std::time::Duration::from_secs(5),
            "启动路径不该有秒级任务（老实现会在这里重建 34s + VACUUM 8s）: {open_cost:?}"
        );

        drop(cache);
        remove_db_files(&db);
    }

    /// **改造核心 2**：老库（9 列）上 `upsert_global_index_entries` / `repair_corrupt_paths`
    /// 必须照老列写齐（NOT NULL 一列都不能漏），写入结果能被正常读回。
    #[test]
    fn legacy_schema_writes_fill_old_columns() {
        let db = temp_db_path("legacy-write");
        {
            let conn = Connection::open(&db).unwrap();
            create_legacy_global_index(&conn);
            // 脏行：早期版本拼接路径留下的 `C:\root/C:/dir/bad.log`
            conn.execute(
                "INSERT INTO global_index
                 (path, name, name_lower, ext, size, is_dir, drive, mtime, created_at)
                 VALUES ('C:\\root/C:/dir/bad.log', 'bad.log', 'bad.log', 'log', 7, 0, 'C', 123, 4000)",
                [],
            )
            .unwrap();
        }
        let cache = DiskCache::open(db.clone()).expect("打开老库");

        // ── upsert：一条新 path + 一条已存在 path 的覆盖 ──
        cache
            .upsert_global_index_entries(&[
                IndexEntry::new("C:/dir/new.txt".into(), "new.txt".into(), 42, false, 777),
                IndexEntry::new("C:/dir/a.TXT".into(), "a.txt".into(), 999, false, 555),
            ])
            .expect("老 schema 下 upsert 必须成功");

        assert_eq!(cache_columns(&cache).len(), 9, "老库 upsert 不得顺手改 schema");
        let (name, name_lower, ext, drive, created_at, size, mtime, is_dir): (
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            i64,
        ) = with_index_conn(&cache, |c| {
            c.query_row(
                "SELECT name, name_lower, ext, drive, created_at, size, mtime, is_dir
                 FROM global_index WHERE path = 'C:/dir/new.txt'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .unwrap()
        });
        assert_eq!(name, "", "老实现里 name 恒写空串");
        assert_eq!(name_lower, "new.txt", "name_lower 必须写入（NOT NULL）");
        assert_eq!(ext, "", "老实现里 ext 恒写空串");
        assert_eq!(drive, "C", "drive 必须由盘符派生（NOT NULL）");
        assert!(created_at > 0, "created_at 必须是当前时间戳: {created_at}");
        assert_eq!((size, mtime, is_dir), (42, 777, 0));

        // 覆盖语义不变：path 是主键，不新增行
        let after = cache.load_global_index().unwrap();
        assert_eq!(after.len(), 5, "老库 4 行（3 正常 + 1 脏行）+ 1 条新增");
        let replaced = after.iter().find(|e| e.path == "C:/dir/a.TXT").unwrap();
        assert_eq!((replaced.size, replaced.mtime), (999, 555));

        // ── repair_corrupt_paths：脏行 → 真实路径，新行同样补齐老列 ──
        let (merged, written) = cache
            .repair_corrupt_paths()
            .expect("老 schema 下 repair 必须成功");
        assert_eq!((merged, written), (0, 1), "脏行应被改写成 C:/dir/bad.log");
        let (path, name, name_lower, ext, drive): (String, String, String, String, String) =
            with_index_conn(&cache, |c| {
                c.query_row(
                    "SELECT path, name, name_lower, ext, drive FROM global_index
                     WHERE path = 'C:/dir/bad.log'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .unwrap()
            });
        assert_eq!(path, "C:/dir/bad.log");
        assert_eq!(name, "", "老实现里 repair 写入的 name 也是空串");
        assert_eq!(name_lower, "bad.log", "repair 写入的 name_lower 是修正后路径的末段小写");
        assert_eq!(ext, "log");
        assert_eq!(drive, "C");
        assert_eq!(
            with_index_conn(&cache, |c| c
                .query_row(
                    "SELECT COUNT(*) FROM global_index WHERE path LIKE '%/%:/%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap()),
            0,
            "脏行必须已被删除"
        );
        let repaired = cache.load_global_index().unwrap();
        assert_eq!(repaired.len(), 5, "repair 不得改变行数（改路径 + 删脏行）");
        assert!(repaired.iter().any(|e| e.path == "C:/dir/bad.log"));

        drop(cache);
        remove_db_files(&db);
    }

    /// **改造核心 3**：老库上走一次 `save_global_index_stream`（= 用户主动重建索引那条重路径）
    /// 就地把 schema 收拢成 4 列，写入的条目能被 `load_global_index` 原样读回，
    /// 且随后的 upsert 自动切到新列。
    #[test]
    fn legacy_db_stream_save_trims_schema_and_roundtrips() {
        let db = temp_db_path("legacy-save");
        {
            let conn = Connection::open(&db).unwrap();
            create_legacy_global_index(&conn);
            // 老库上的索引，必须随 DROP TABLE 一起消失
            conn.execute(
                "CREATE INDEX idx_global_index_name_lower ON global_index(name_lower)",
                [],
            )
            .unwrap();
        }
        let cache = DiskCache::open(db.clone()).expect("打开老库");
        assert_eq!(cache_columns(&cache).len(), 9, "前置条件：打开后仍是 9 列");

        // 300 条，分块发送（模拟生产的 5000/块）
        let entries: Vec<IndexEntry> = (0..300)
            .map(|i| {
                IndexEntry::new(
                    format!("C:/bulk/dir{}/file{i}.dat", i % 13),
                    format!("file{i}.dat").into(),
                    i as i64 * 3,
                    i % 7 == 0,
                    1_700_000_000 + i as i64,
                )
            })
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        for chunk in entries.chunks(64) {
            tx.send(chunk.to_vec()).unwrap();
        }
        drop(tx);
        cache
            .save_global_index_stream(rx)
            .expect("老库上流式重建必须成功");

        let cols = cache_columns(&cache);
        assert!(is_new_global_index_schema(&cols), "重建后必须收拢为 4 列: {cols:?}");
        assert_eq!(cols.len(), 4);
        assert!(schema_marked(&cache), "收拢后必须落 schema_v2 标记");
        with_index_conn(&cache, |c| {
            for old in ["name", "name_lower", "ext", "drive", "created_at"] {
                let err = c
                    .prepare(&format!("SELECT {old} FROM global_index"))
                    .expect_err(&format!("{old} 列必须已消失"));
                assert!(
                    err.to_string().contains("no such column"),
                    "{old} 应报 no such column: {err}"
                );
            }
            assert_eq!(master_count(c, "idx_global_index_name_lower"), 0, "老索引随表一起 DROP");
            assert_eq!(master_count(c, "global_index_v2"), 0, "不得留下中间表");
        });

        let loaded = cache.load_global_index().expect("载入");
        assert_eq!(loaded.len(), entries.len(), "老库 3 行被全量替换，而不是叠加");
        for (got, want) in loaded.iter().zip(entries.iter()) {
            assert_eq!(got.path, want.path);
            assert_eq!(got.name_lower, want.name_lower, "name_lower 由 path 派生后必须一致");
            assert_eq!(got.size, want.size);
            assert_eq!(got.is_dir, want.is_dir);
            assert_eq!(got.mtime, want.mtime);
        }

        // 收拢后同一实例的写入必须自动切到 4 列（实例内探测缓存已刷新）
        cache
            .upsert_global_index_entries(&[IndexEntry::new(
                "C:/bulk/new.bin".into(),
                "new.bin".into(),
                5,
                false,
                9,
            )])
            .expect("新 schema 下 upsert");
        assert_eq!(cache.load_global_index().unwrap().len(), 301);

        // 模拟重启：仍是 4 列、条目数一致
        drop(cache);
        let cache2 = DiskCache::open(db.clone()).expect("重启后打开");
        assert!(is_new_global_index_schema(&cache_columns(&cache2)));
        assert_eq!(cache2.load_global_index().unwrap().len(), 301);
        drop(cache2);
        remove_db_files(&db);
    }

    /// 老库上重建中途失败必须**连 `DROP TABLE` 一起回滚**：老表 9 列与全部数据原样，
    /// 不会出现"表被删了但数据没写进去"。
    ///
    /// 失败点用 `index_meta` 上的触发器精确地推到**最坏位置**：`DROP` 已生效、新表已建、
    /// 300 行已全部灌完、只差最后落标记 + 提交。`index_meta` 不是被 DROP 的表，
    /// 所以触发器能活到那一刻（用 `global_index` 自己的 CHECK 做不到：约束会随 DROP 一起消失，
    /// 那样插入反而会成功）。
    #[test]
    fn legacy_stream_save_failure_rolls_back_drop() {
        let db = temp_db_path("legacy-save-rollback");
        {
            let conn = Connection::open(&db).unwrap();
            create_legacy_global_index(&conn);
            conn.execute(
                "CREATE TABLE IF NOT EXISTS index_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "CREATE TRIGGER block_schema_mark BEFORE INSERT ON index_meta
                 BEGIN SELECT RAISE(ABORT, 'blocked by test'); END",
                [],
            )
            .unwrap();
        }
        let cache = DiskCache::open(db.clone()).expect("打开老库");

        let entries: Vec<IndexEntry> = (0..300)
            .map(|i| {
                IndexEntry::new(
                    format!("C:/x/f{i}.bin"),
                    format!("f{i}.bin").into(),
                    i as i64,
                    false,
                    i as i64,
                )
            })
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(entries).unwrap();
        drop(tx);
        let err = cache
            .save_global_index_stream(rx)
            .expect_err("触发器必须让提交前的最后一步失败");
        eprintln!("[test] 预期的重建失败: {err:#}");

        // 完整回滚：老表 9 列、老行数与内容不变、没有残留新表
        let cols = cache_columns(&cache);
        assert_eq!(cols.len(), 9, "失败后必须还是老 schema: {cols:?}");
        with_index_conn(&cache, |c| {
            assert_eq!(master_count(c, "global_index_v2"), 0, "不得留下半成品新表");
            let n: i64 = c
                .query_row("SELECT COUNT(*) FROM global_index", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 3, "DROP 必须被回滚：失败后行数不得变化");
            let name: String = c
                .query_row(
                    "SELECT name FROM global_index WHERE path = 'C:/dir/a.TXT'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(name, "a.TXT", "老列数据必须完好");
        });

        // 拆掉触发器后重跑必须成功：失败不得留下不可恢复的状态
        with_index_conn(&cache, |c| {
            c.execute("DROP TRIGGER block_schema_mark", []).unwrap()
        });
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(vec![IndexEntry::new(
            "C:/only.bin".into(),
            "only.bin".into(),
            1,
            false,
            2,
        )])
        .unwrap();
        drop(tx);
        cache
            .save_global_index_stream(rx)
            .expect("拆掉触发器后必须能重建成功");
        assert!(is_new_global_index_schema(&cache_columns(&cache)));
        assert_eq!(cache.load_global_index().unwrap().len(), 1);

        drop(cache);
        remove_db_files(&db);
    }

    /// 新库 save → load 往返：条目字段与顺序一致，且表就是收拢后的 4 列。
    #[test]
    fn global_index_new_schema_roundtrip() {
        let db = temp_db_path("roundtrip");
        let cache = DiskCache::open(db.clone()).expect("打开空库");

        let entries = vec![
            IndexEntry::new("C:/dir/a.TXT".into(), "a.txt".into(), 100, false, 111),
            IndexEntry::new("C:/dir/sub".into(), "sub".into(), 0, true, 0),
            IndexEntry::new("D:/目录/文件.log".into(), "文件.log".into(), 7, false, 333),
        ];
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(entries.clone()).unwrap();
        drop(tx);
        cache.save_global_index_stream(rx).expect("流式写入");

        assert!(is_new_global_index_schema(&cache_columns(&cache)), "空库必须直接是新 schema");
        assert!(schema_marked(&cache), "空库也要落 schema_v2 标记");

        let loaded = cache.load_global_index().expect("载入");
        assert_eq!(loaded.len(), entries.len());
        for (got, want) in loaded.iter().zip(entries.iter()) {
            assert_eq!(got.path, want.path);
            assert_eq!(got.name_lower, want.name_lower, "name_lower 由 path 派生后必须一致");
            assert_eq!(got.size, want.size);
            assert_eq!(got.is_dir, want.is_dir);
            assert_eq!(got.mtime, want.mtime);
            assert_eq!(got.path_ascii, want.path_ascii);
        }
        // 顺序语义不变：无 ORDER BY 的全表扫描按 rowid（= 写入顺序，`INSERT OR REPLACE` 保序）
        let paths: Vec<&str> = loaded.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["C:/dir/a.TXT", "C:/dir/sub", "D:/目录/文件.log"]);

        // upsert 路径同样只写 4 列，且覆盖语义不变
        cache
            .upsert_global_index_entries(&[IndexEntry::new(
                "C:/dir/a.TXT".into(),
                "a.txt".into(),
                555,
                false,
                999,
            )])
            .expect("upsert");
        let after = cache.load_global_index().unwrap();
        assert_eq!(after.len(), 3, "upsert 已存在的 path 不得新增行");
        // 注意：`INSERT OR REPLACE` 是 DELETE+INSERT，被覆盖的行会换到新的 rowid（= 扫到末尾）。
        // 这是**既有**语义（迁移前也是同一条 SQL），所以这里按 path 定位而不是按位置。
        let replaced = after
            .iter()
            .find(|e| e.path == "C:/dir/a.TXT")
            .expect("覆盖后的条目必须还在");
        assert_eq!(replaced.size, 555);
        assert_eq!(replaced.mtime, 999);
        assert!(!replaced.is_dir);

        // 新 schema 下再走一次全量重建：语义是**整表替换**（老行必须消失）
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(vec![IndexEntry::new(
            "C:/dir/a.TXT".into(),
            "a.txt".into(),
            1,
            false,
            2,
        )])
        .unwrap();
        drop(tx);
        cache.save_global_index_stream(rx).expect("第二次流式写入");
        assert!(is_new_global_index_schema(&cache_columns(&cache)), "重建不得换掉 schema");
        let only = cache.load_global_index().unwrap();
        assert_eq!(only.len(), 1, "全量重建必须替换整表: {:?}", only.len());
        assert_eq!(only[0].path, "C:/dir/a.TXT");
        assert_eq!((only[0].size, only[0].mtime), (1, 2));

        drop(cache);
        remove_db_files(&db);
    }

    /// 老 schema 副本上的迁移（**内部工具** `migrate_global_index_schema`，已不在启动路径上）：
    /// 列收拢、载入结果不变、可重复执行（第二次 no-op）。
    ///
    /// 注意：`DiskCache::open` 现在**不会**迁移（见 `legacy_db_open_keeps_legacy_schema_untouched`），
    /// 所以这里显式地在索引连接上调用迁移函数本身。
    #[test]
    fn legacy_global_index_schema_migrates_once() {
        let db = temp_db_path("legacy");
        {
            let conn = Connection::open(&db).unwrap();
            create_legacy_global_index(&conn);
            assert_eq!(global_index_columns(&conn).unwrap().len(), 9, "前置条件：老库是 9 列");
        }

        let cache = DiskCache::open(db.clone()).expect("打开老库（启动路径不迁移）");
        assert_eq!(cache_columns(&cache).len(), 9, "前置条件：open() 后仍是 9 列");

        // 显式调用内部迁移工具（这就是被移出启动路径的那段逻辑）
        {
            let mut g = cache.index_conn.lock();
            let conn = g.as_mut().expect("索引连接可用");
            migrate_global_index_schema(conn).expect("迁移必须成功");
        }
        let cols = cache_columns(&cache);
        assert!(is_new_global_index_schema(&cols), "迁移后列集合必须收拢: {cols:?}");
        assert_eq!(cols.len(), 4);
        assert!(schema_marked(&cache), "迁移必须落 schema_v2 标记");

        // 行数与内容正确，`load_global_index` 读出同样的条目
        let loaded = cache.load_global_index().expect("载入");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].path, "C:/dir/a.TXT");
        assert_eq!(loaded[0].name_lower, "a.txt");
        assert_eq!(loaded[0].size, 100);
        assert!(!loaded[0].is_dir);
        assert_eq!(loaded[0].mtime, 111);
        assert_eq!(loaded[1].path, "C:/dir/sub");
        assert!(loaded[1].is_dir);
        assert_eq!(loaded[2].path, "D:/目录/文件.log");
        assert_eq!(loaded[2].name_lower, "文件.log");
        assert_eq!(loaded[2].size, 7);

        // 老列是真的被 DROP 了（不是留空）
        {
            let g = cache.index_conn.lock();
            let conn = g.as_ref().unwrap();
            for old in ["name", "name_lower", "ext", "drive", "created_at"] {
                let err = conn
                    .prepare(&format!("SELECT {old} FROM global_index"))
                    .expect_err(&format!("{old} 列必须已被 DROP"));
                assert!(
                    err.to_string().contains("no such column"),
                    "{old} 列应报 no such column: {err}"
                );
            }
        }

        // 幂等：第二次迁移必须是 no-op —— 用 b-tree 根页不变来证明"表没有被重建"
        let root_before = cache_rootpage(&cache);
        {
            let mut g = cache.index_conn.lock();
            let conn = g.as_mut().unwrap();
            migrate_global_index_schema(conn).expect("第二次迁移必须成功（no-op）");
        }
        assert_eq!(
            cache_rootpage(&cache),
            root_before,
            "第二次迁移必须是 no-op：global_index 的 b-tree 根页不得变化"
        );
        assert!(schema_marked(&cache));
        assert_eq!(cache.load_global_index().unwrap().len(), 3, "重复迁移不得丢行");

        // 模拟重启：再开一次库，仍是新 schema、条目一致
        drop(cache);
        let cache2 = DiskCache::open(db.clone()).expect("重启后打开");
        assert!(is_new_global_index_schema(&cache_columns(&cache2)));
        let reloaded = cache2.load_global_index().unwrap();
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded[2].path, "D:/目录/文件.log");
        drop(cache2);
        remove_db_files(&db);
    }

    /// 迁移中途失败必须**完整回滚**：库仍是老 schema、行数不变、不留半迁移残留。
    ///
    /// 用外键把失败点精确地推到"新表已建好、数据已拷完"之后（`DROP TABLE global_index`
    /// 触发隐式 DELETE → 外键冲突），这是最坏位置的失败。
    #[test]
    fn failed_migration_rolls_back_completely() {
        let db = temp_db_path("rollback");
        {
            let conn = Connection::open(&db).unwrap();
            create_legacy_global_index(&conn);
            conn.execute(
                "CREATE TABLE ref_guard (
                    id INTEGER PRIMARY KEY,
                    p TEXT REFERENCES global_index(path)
                )",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO ref_guard (id, p) VALUES (1, 'C:/dir/a.TXT')", [])
                .unwrap();
        }

        let mut conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        let err = migrate_global_index_schema(&mut conn).expect_err("外键挡住 DROP，迁移必须失败");
        eprintln!("[test] 预期的迁移失败: {err:#}");

        // 老表原样：9 列、3 行、内容可读
        assert_eq!(global_index_columns(&conn).unwrap().len(), 9, "失败后必须还是老 schema");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM global_index", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 3, "失败后行数不得变化");
        let name: String = conn
            .query_row("SELECT name FROM global_index WHERE path = 'C:/dir/a.TXT'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "a.TXT", "失败后老列数据必须完好");

        // 事务内建的新表必须随回滚消失，且不得落下"已完成"的标记
        let leftover: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'global_index_v2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover, 0, "半迁移残留表必须随事务回滚");
        let marked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM index_meta WHERE key = ?1",
                params![GLOBAL_INDEX_SCHEMA_KEY],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marked, 0, "迁移失败不得落 schema_v2 标记");
        drop(conn);
        remove_db_files(&db);
    }

    /// 建一张旧库结构的 scan_meta（没有 last_access 列）并插一行。
    fn legacy_conn_with_row(created_at: i64) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE scan_meta (
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
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_meta
             (scan_path, dir_mtime, created_at, size, mft_available, item_count, verified_usn, blob_file)
             VALUES ('C:/a', 1, ?1, 10, 0, 1, 0, 'x.bin')",
            params![created_at],
        )
        .unwrap();
        conn
    }

    /// 缺陷 9：旧库迁移必须补列并用 created_at 回填，且重复执行幂等。
    #[test]
    fn last_access_migration_is_idempotent() {
        let conn = legacy_conn_with_row(111);
        migrate_last_access(&conn);
        let last: i64 = conn
            .query_row("SELECT last_access FROM scan_meta WHERE scan_path = 'C:/a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(last, 111, "旧行的 last_access 必须用 created_at 回填");

        // 已有真实访问时间：重复迁移不得覆盖
        conn.execute("UPDATE scan_meta SET last_access = 222", []).unwrap();
        migrate_last_access(&conn);
        let last: i64 = conn
            .query_row("SELECT last_access FROM scan_meta WHERE scan_path = 'C:/a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(last, 222, "重复迁移不得覆盖既有 last_access");

        // 已带该列的新库也能直接跑（列存在时 ALTER 失败被忽略）
        let fresh = Connection::open_in_memory().unwrap();
        fresh
            .execute(
                "CREATE TABLE scan_meta (
                    scan_path TEXT PRIMARY KEY,
                    created_at INTEGER NOT NULL,
                    last_access INTEGER NOT NULL DEFAULT 0
                )",
                [],
            )
            .unwrap();
        migrate_last_access(&fresh);
    }

    /// 缺陷 9 的核心：读取命中只允许刷新 last_access，
    /// created_at（写入时间）必须保持原值，供 get_derived 判断新鲜度。
    #[test]
    fn read_hit_does_not_rewrite_created_at() {
        let conn = legacy_conn_with_row(111);
        migrate_last_access(&conn);
        // 回填后 last_access == created_at == 111
        mark_accessed(&conn, "C:/a");

        let (created_at, last_access): (i64, i64) = conn
            .query_row(
                "SELECT created_at, last_access FROM scan_meta WHERE scan_path = 'C:/a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(created_at, 111, "读取命中不得改写 created_at（写入时间）");
        assert!(
            last_access >= 111,
            "读取命中必须刷新 last_access: {last_access}"
        );
    }

    /// P0-2：落盘形态（ItemDisk）往返 —— `name` 由 path 派生、`size_formatted` 留空。
    #[test]
    fn item_disk_roundtrip_derives_name_and_drops_derived_fields() {
        let items = vec![
            Item {
                path: CompactString::from("C:/a/b.txt"),
                name: CompactString::from("b.txt"),
                size: 5,
                size_formatted: crate::scan::format_size(5),
                is_dir: false,
                mtime: 1,
                atime: 2,
            },
            Item {
                path: CompactString::from(format!("C:/very/long/path/{}", "n".repeat(40))),
                name: CompactString::from("n".repeat(40)),
                size: 9,
                size_formatted: crate::scan::format_size(9),
                is_dir: false,
                mtime: 3,
                atime: 4,
            },
            Item {
                path: CompactString::from("C:/a/dir"),
                name: CompactString::from("dir"),
                size: 14,
                size_formatted: CompactString::new(),
                is_dir: true,
                mtime: 5,
                atime: 6,
            },
        ];

        let payload = bincode::serialize(&ItemDiskSeq(&items)).unwrap();
        // 走生产解码路径（ItemsFromBlob 只做字段搬运，name 由 decode_items_payload 补齐）
        let back = decode_items_payload(&payload).unwrap();
        assert_eq!(back.len(), 3);
        for (old, new) in items.iter().zip(back.iter()) {
            assert_eq!(new.path, old.path, "path 必须原样保存");
            assert_eq!(new.size, old.size);
            assert_eq!(new.is_dir, old.is_dir);
            assert_eq!(new.mtime, old.mtime);
            assert_eq!(new.atime, old.atime);
            // name 由 path 派生（与旧存储值一致，因为写入侧满足同一不变式）
            assert_eq!(new.name, old.name, "派生 name 必须与存储值一致");
            assert_eq!(
                new.name,
                crate::scan::name_from_path(&old.path),
                "name 必须等于 path 末段"
            );
            // 派生字段不落盘：载入后为空，由展示层按需重算
            assert!(new.size_formatted.is_empty(), "size_formatted 不再落盘");
        }

        // 形态确实更小：不含 name 文本，且少了 size_formatted 的长度前缀
        let legacy = bincode::serialize(&items).unwrap();
        assert!(
            payload.len() < legacy.len(),
            "紧凑形态必须比旧形态小: {} vs {}",
            payload.len(),
            legacy.len()
        );
    }

    /// P0-2：blob 头的路径校验与版本校验都不能被削弱。
    #[test]
    fn blob_header_path_and_version_are_validated() {
        let enc = encode_blob("C:/x", b"abc");
        assert_eq!(decode_blob("C:/x", &enc), Some(&b"abc"[..]));
        assert_eq!(decode_blob("C:/y", &enc), None, "路径不匹配必须判为未命中");
        assert_eq!(decode_blob("C:/x", b"FDB"), None, "截断的头必须判为未命中");

        // 旧版本（bump 之前的 blob 文件）必须判为未命中 → 首次访问重扫
        let mut old = enc.clone();
        old[4] = BLOB_VERSION - 1;
        assert_eq!(decode_blob("C:/x", &old), None, "旧版本 blob 必须判为未命中");
    }

    /// P0-2：整条落盘链路（encode_blob → decode_blob → decode_items_payload）往返一致。
    /// 这是"blob 文件名是路径哈希、头里带原始路径做冲突校验"这一层的端到端校验。
    #[test]
    fn blob_file_roundtrip_keeps_items_and_path_check() {
        let path = "C:/Users/me";
        let items = vec![
            Item {
                path: CompactString::from("C:/Users/me/a.txt"),
                name: CompactString::from("a.txt"),
                size: 1024,
                size_formatted: crate::scan::format_size(1024),
                is_dir: false,
                mtime: 111,
                atime: 222,
            },
            Item {
                path: CompactString::from("C:/Users/me/sub"),
                name: CompactString::from("sub"),
                size: 2048,
                size_formatted: CompactString::new(),
                is_dir: true,
                mtime: 333,
                atime: 444,
            },
        ];
        let payload = bincode::serialize(&ItemDiskSeq(&items)).unwrap();
        let file = encode_blob(path, &payload);

        // 头里的路径与实际读取路径不一致 → 未命中（哈希冲突/串数据防护）
        assert_eq!(decode_blob("C:/Users/other", &file), None);
        let got = decode_blob(path, &file).expect("同路径必须命中");
        let back = decode_items_payload(got).expect("必须能解码");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].path, items[0].path);
        assert_eq!(back[0].name, items[0].name, "name 由 path 派生后必须一致");
        assert_eq!(back[0].size, items[0].size);
        assert_eq!(back[0].mtime, items[0].mtime);
        assert_eq!(back[0].atime, items[0].atime);
        assert_eq!(back[1].is_dir, items[1].is_dir);
        assert_eq!(back[1].name, items[1].name);
        assert!(back[0].size_formatted.is_empty(), "size_formatted 不落盘");
    }

    // ─── 基准 ─────────────────────────────────────────────
    //
    // 统一跑法：cargo test --release --lib -- --ignored --nocapture bench
    /// 与 global_search::name_from_path 同构的"路径末段小写"派生。
    /// 基准里用它验证：能否省掉 name_lower 列的读取而结果不变。
    /// 返回 `CompactString`：与 `IndexEntry::name_lower` 同型（P2 后 ≤23B 内联）。
    fn last_segment_lower(path: &str) -> CompactString {
        match path.rfind('/') {
            Some(i) => path[i + 1..].to_lowercase().into(),
            None => path.to_lowercase().into(),
        }
    }

    /// 独立探测连接（只读基准用）：可分别设置 cache_size / mmap_size 观察页读取开销。
    fn probe_conn(db: &std::path::Path, mmap_mb: i64, cache_kb: i64) -> Connection {
        let conn = Connection::open(db).expect("open probe conn");
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        let _ = conn.pragma_update(None, "cache_size", -cache_kb);
        if mmap_mb > 0 {
            let _ = conn.pragma_update(None, "mmap_size", mmap_mb * 1024 * 1024);
        }
        conn
    }

    /// 变体载入：`derive_name=true` 时只读 4 列，name_lower 由路径末段派生。
    ///
    /// schema 收拢后 `name_lower` 列已不存在，`has_name_lower=false` 时"读列"这一档
    /// 自动退化成派生档 —— 老形态的 A/B 只能跑在仍是老 schema 的库上。
    fn load_variant(
        conn: &Connection,
        derive_name: bool,
        with_capacity: bool,
        has_name_lower: bool,
    ) -> Vec<IndexEntry> {
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM global_index", [], |r| r.get(0))
            .unwrap_or(0);
        let derive = derive_name || !has_name_lower;
        let sql = if derive {
            "SELECT path, size, is_dir, mtime FROM global_index"
        } else {
            "SELECT path, name_lower, size, is_dir, mtime FROM global_index"
        };
        let mut stmt = conn.prepare(sql).unwrap();
        let mut out: Vec<IndexEntry> = if with_capacity {
            Vec::with_capacity(total.max(0) as usize)
        } else {
            Vec::new()
        };
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let path: String = row.get(0).unwrap();
            let (name_lower, size, is_dir, mtime) = if derive {
                (
                    last_segment_lower(&path),
                    row.get::<_, i64>(1).unwrap(),
                    row.get::<_, i64>(2).unwrap() != 0,
                    row.get::<_, i64>(3).unwrap(),
                )
            } else {
                (
                    row.get::<_, String>(1).unwrap().into(),
                    row.get::<_, i64>(2).unwrap(),
                    row.get::<_, i64>(3).unwrap() != 0,
                    row.get::<_, i64>(4).unwrap(),
                )
            };
            out.push(IndexEntry::new(path, name_lower, size, is_dir, mtime));
        }
        out
    }

    /// P0-3a'：全局索引载入（310 万行）耗时拆解。
    /// 关注点：预分配 Vec 容量、少读列（name_lower 可否派生）、SQLite 页缓存/mmap。
    ///
    /// 变体逐轮交替执行（每档 2 轮）：单次测量会被并行编译/页缓存抖动放大数倍，
    /// 只看第 2 轮（稳定态）才可比。
    #[test]
    #[ignore = "benchmark: 依赖本机 cache_v2.db（310 万行索引）"]
    fn bench_load_global_index() {
        use std::time::Instant;

        let db = DiskCache::get_cache_path().expect("cache path");
        let cache = DiskCache::instance();

        // 本库当前 schema：schema 收拢后 name_lower / name / ext / drive / created_at 已不存在，
        // 依赖这些列的档位（A/C/E、p2/p4、一致性校验）自动退化或跳过。
        let cols = {
            let c = probe_conn(&db, 0, 2048);
            global_index_columns(&c).unwrap_or_default()
        };
        let has_name_lower = cols.iter().any(|c| c == "name_lower");
        eprintln!("[bench] global_index 列: {cols:?}");

        // 预热页缓存
        let _ = cache.load_global_index();

        // (标签, mmap_mb, cache_kb, 派生name_lower, 预分配容量)；mmap_mb<0 = 走生产实例
        let cfgs: [(&str, i64, i64, bool, bool); 6] = [
            ("生产路径(load_global_index)", -1, -1, false, true),
            ("A 预分配+全列 默认页缓存", 0, 2048, false, true),
            ("B 派生name_lower 默认页缓存", 0, 2048, true, true),
            ("C 预分配+全列 mmap2GB+cache256MB", 2048, 262144, false, true),
            ("D 派生name_lower mmap2GB+cache256MB", 2048, 262144, true, true),
            ("E 不预分配+全列 默认页缓存", 0, 2048, false, false),
        ];

        let mut n = 0usize;
        for round in 0..2 {
            for (label, mmap_mb, cache_kb, derive, cap) in cfgs {
                let t = Instant::now();
                let len = if mmap_mb < 0 {
                    let v = cache.load_global_index().unwrap_or_default();
                    let len = v.len();
                    drop(v);
                    len
                } else {
                    let conn = probe_conn(&db, mmap_mb, cache_kb);
                    let v = load_variant(&conn, derive, cap, has_name_lower);
                    let len = v.len();
                    drop(v);
                    drop(conn);
                    len
                };
                let ms = t.elapsed();
                n = n.max(len);
                eprintln!("[bench] round{} {:<34} {} 条 {:>9.3}s", round, label, len, ms.as_secs_f64());
            }
        }
        if n == 0 {
            eprintln!("[bench] 无持久化索引，跳过");
            return;
        }

        // 同一条**索引连接**上的 A/B：旧 SQL 形态（读 name_lower 列）vs 新形态（派生），
        // 排除"不同连接/不同页缓存"的干扰——这是 P0-3a' 唯一的有效前后对比。
        {
            let mut g = cache.index_conn.lock();
            if let Some(conn) = g.as_mut() {
                if has_name_lower {
                    for round in 0..2 {
                        let t = Instant::now();
                        let v = load_variant(conn, false, false, has_name_lower);
                        let old_s = t.elapsed().as_secs_f64();
                        drop(v);
                        let t = Instant::now();
                        let v = load_variant(conn, true, false, has_name_lower);
                        let new_s = t.elapsed().as_secs_f64();
                        drop(v);
                        eprintln!(
                            "[bench] 同一连接 A/B round{}: 旧(读 name_lower) {:.3}s vs 新(派生 name_lower) {:.3}s ({:+.1}%)",
                            round,
                            old_s,
                            new_s,
                            (new_s - old_s) * 100.0 / old_s
                        );
                    }
                } else {
                    eprintln!(
                        "[bench] 本库已收拢为新 schema（无 name_lower 列），跳过'读列 vs 派生' A/B"
                    );
                }
            }
        }

        // ── 成本拆解：3.1M 行的 3s 到底花在哪 ──
        // p1) 纯行遍历（不读任何列，只取列数）：SQLite VM + b-tree 遍历成本
        // p2) 只读 path 且不分配（ValueRef 取长度）：列解码成本
        // p3) 只读 path 并分配 String：分配成本
        // p4) 读 path+name_lower 并分配：生产路径的列成本
        // p5) COUNT(*)：全表 b-tree 扫描（不含行 payload 解码）
        // p6) 行 payload 字节量（length(path)+length(name_lower)+其它列）
        let conn = probe_conn(&db, 0, 2048);
        {
            let t = Instant::now();
            let mut stmt = conn.prepare("SELECT * FROM global_index").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let mut c = 0usize;
            while let Some(row) = rows.next().unwrap() {
                c += std::hint::black_box(row.as_ref().column_count());
            }
            eprintln!("[bench] p1 纯行遍历(不读列) {} 行: {:.3}s", c, t.elapsed().as_secs_f64());
        }
        if has_name_lower {
            let t = Instant::now();
            let mut stmt = conn.prepare("SELECT path, name_lower FROM global_index").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let mut bytes = 0usize;
            while let Some(row) = rows.next().unwrap() {
                for i in 0..2 {
                    if let rusqlite::types::ValueRef::Text(t) = row.get_ref(i).unwrap() {
                        bytes += t.len();
                    }
                }
            }
            eprintln!(
                "[bench] p2 读两列不分配 {} 行, 文本 {} MB: {:.3}s",
                n,
                bytes / 1024 / 1024,
                t.elapsed().as_secs_f64()
            );
        } else {
            eprintln!("[bench] p2 跳过（无 name_lower 列）");
        }
        {
            let t = Instant::now();
            let mut stmt = conn.prepare("SELECT path FROM global_index").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let mut acc = 0usize;
            let mut v: Vec<String> = Vec::with_capacity(n);
            while let Some(row) = rows.next().unwrap() {
                let s: String = row.get(0).unwrap();
                acc += s.len();
                v.push(s);
            }
            eprintln!(
                "[bench] p3 只读 path(分配) {} 行, {} MB: {:.3}s (acc={})",
                v.len(),
                acc / 1024 / 1024,
                t.elapsed().as_secs_f64(),
                acc
            );
            drop(v);
        }
        if has_name_lower {
            let t = Instant::now();
            let mut stmt = conn.prepare("SELECT path, name_lower FROM global_index").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let mut v: Vec<(String, String)> = Vec::with_capacity(n);
            while let Some(row) = rows.next().unwrap() {
                v.push((row.get(0).unwrap(), row.get(1).unwrap()));
            }
            eprintln!(
                "[bench] p4 读 path+name_lower(分配) {} 行: {:.3}s",
                v.len(),
                t.elapsed().as_secs_f64()
            );
            drop(v);
        } else {
            eprintln!("[bench] p4 跳过（无 name_lower 列）");
        }
        {
            let t = Instant::now();
            let c: i64 = conn
                .query_row("SELECT COUNT(*) FROM global_index", [], |r| r.get(0))
                .unwrap();
            eprintln!("[bench] p5 COUNT(*) {}: {:.3}s", c, t.elapsed().as_secs_f64());
        }
        {
            // p6) 行 payload 估算：对**当前实际存在的所有列**求和（+24B 行头），
            // 老/新 schema 同一口径，可直接对比表体积。列名来自 PRAGMA，不涉及外部输入。
            let sum_expr: String = cols
                .iter()
                .map(|c| format!("length({c})"))
                .collect::<Vec<_>>()
                .join(" + ");
            let t = Instant::now();
            let bytes: i64 = conn
                .query_row(
                    &format!("SELECT SUM({sum_expr} + 24) FROM global_index"),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            eprintln!(
                "[bench] p6 行 payload 估算 {:.1} MB（当前 {} 列 + 24B 行头）: {:.3}s",
                bytes as f64 / 1024.0 / 1024.0,
                cols.len(),
                t.elapsed().as_secs_f64()
            );
        }
        drop(conn);

        // 一致性：派生 name_lower 是否与库中存储值完全一致（不一致就不能派生）。
        // 新 schema 已无该列，派生本身就是唯一真值来源，跳过。
        let conn = probe_conn(&db, 0, 2048);
        if has_name_lower {
            let t = Instant::now();
            let mut mismatches = 0usize;
            let mut sample: Vec<String> = Vec::new();
            {
                let mut stmt = conn
                    .prepare("SELECT path, name_lower FROM global_index")
                    .unwrap();
                let mut rows = stmt.query([]).unwrap();
                while let Some(row) = rows.next().unwrap() {
                    let path: String = row.get(0).unwrap();
                    let stored: String = row.get(1).unwrap();
                    let derived = last_segment_lower(&path);
                    if derived != stored {
                        mismatches += 1;
                        if sample.len() < 3 {
                            sample.push(format!("{path} 库={stored:?} 派生={derived:?}"));
                        }
                    }
                }
            }
            eprintln!(
                "[bench] name_lower 一致性: 不一致 {} / {} 条, 全量校验耗时 {:?} 例: {:?}",
                mismatches,
                n,
                t.elapsed(),
                sample
            );
        } else {
            eprintln!("[bench] name_lower 一致性校验跳过（新 schema 已无该列）");
        }

        // 流式接口（供 owner 接线）与整份载入的一致性 + 耗时/峰值差异
        let t = Instant::now();
        let mut streamed = 0usize;
        let mut first: Option<IndexEntry> = None;
        let mut last: Option<IndexEntry> = None;
        cache
            .for_each_global_index_chunk(50_000, |chunk| {
                streamed += chunk.len();
                for e in chunk {
                    if first.is_none() {
                        first = Some(e.clone());
                    }
                    last = Some(e);
                }
                Ok(())
            })
            .unwrap();
        eprintln!(
            "[bench] 流式分块载入(50k/块): {} 条 {:?}（不建 500MB 中间 Vec）",
            streamed,
            t.elapsed()
        );
        assert_eq!(streamed, n, "流式与整份载入条目数必须一致");
        if let (Some(f), Some(l)) = (first, last) {
            eprintln!(
                "[bench] 流式首/末条: {} / {}",
                f.path,
                l.path
            );
        }
    }

    /// 与（迁移前的）`load_global_index` **完全同构**的探测载入：只读 4 列、不 COUNT(*)、
    /// 不预分配。前后两次测量用的是同一段代码，差别只有表里的列数/页数。
    fn probe_load_exact(conn: &Connection) -> Vec<IndexEntry> {
        let mut stmt = conn
            .prepare("SELECT path, size, is_dir, mtime FROM global_index")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut out: Vec<IndexEntry> = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let path: String = row.get(0).unwrap();
            let name_lower = last_segment_lower(&path);
            out.push(IndexEntry::new(
                path,
                name_lower,
                row.get(1).unwrap(),
                row.get::<_, i64>(2).unwrap() != 0,
                row.get(3).unwrap(),
            ));
        }
        out
    }

    fn copy_bench_rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM global_index", [], |r| r.get(0))
            .unwrap_or(0)
    }

    fn copy_bench_size(db: &std::path::Path) -> u64 {
        std::fs::metadata(db).map(|m| m.len()).unwrap_or(0)
    }

    /// 用同一个同构探测连跑 `rounds` 轮（第 1 轮预热页缓存，不计入），返回各轮耗时（秒）。
    ///
    /// 单次测量抖动可达 ±40%（见 `bench_load_global_index` 的注释），所以报告里用
    /// **各轮最小值**作为稳定估计。
    fn bench_load_rounds(db: &std::path::Path, rounds: usize) -> Vec<f64> {
        use std::time::Instant;
        let conn = probe_conn(db, 0, 2048);
        let _ = probe_load_exact(&conn); // 预热
        let mut out = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let t = Instant::now();
            let v = probe_load_exact(&conn);
            let s = t.elapsed().as_secs_f64();
            drop(v);
            out.push(s);
        }
        out
    }

    fn min_secs(secs: &[f64]) -> f64 {
        secs.iter().cloned().fold(f64::INFINITY, f64::min)
    }

    fn report_rounds(label: &str, secs: &[f64]) {
        let ms: Vec<u64> = secs.iter().map(|s| (s * 1000.0).round() as u64).collect();
        eprintln!("[bench] {label}: min {:.3}s | 各轮 {:?} ms", min_secs(secs), ms);
    }

    /// `global_index` 表自身占的字节数（best-effort：编译时没带 `dbstat` 就返回 None）。
    fn bench_table_bytes(conn: &Connection, table: &str) -> Option<i64> {
        conn.query_row(
            "SELECT SUM(pgsize) FROM dbstat WHERE name = ?1",
            params![table],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
    }

    /// 真实库**副本**上的 A/B：① 启动路径冻结（改造前 42s → 改造后？）；② 用户主动重建索引
    /// 这条路径把 schema 收拢成 4 列之后的库大小/列数/载入耗时。
    ///
    /// 改造前：`DiskCache::instance()`（main.rs 在**建窗之前**就调用它）会在
    /// `open_index_conn` → `migrate_global_index_schema` 里同步跑"重建 + VACUUM"；
    /// 改造后启动只建表，收拢改由 `save_global_index_stream` 承担。
    ///
    /// **只碰副本**，且第二份副本由本基准自己从第一份复制出来（绝不打开真实库的写路径）：
    /// ```text
    /// copy "$env:USERPROFILE\.flashdir\cache_v2.db*" D:\tmp\fd_ab\
    /// $env:FLASHDIR_CACHE_DB = 'D:\tmp\fd_ab\cache_v2.db'
    /// cargo test --release --lib -- --ignored --nocapture bench_legacy_startup_and_rebuild_real_db_copy
    /// ```
    #[test]
    #[ignore = "benchmark: 需要 FLASHDIR_CACHE_DB 指向真实库的副本"]
    fn bench_legacy_startup_and_rebuild_real_db_copy() {
        use std::sync::Arc;
        use std::time::Instant;

        let Some(db) = std::env::var_os(CACHE_DB_ENV).map(PathBuf::from) else {
            eprintln!("[bench] 未设置 {CACHE_DB_ENV}，跳过（拒绝在真实库上跑）");
            return;
        };
        assert!(db.exists(), "{CACHE_DB_ENV} 指向的文件不存在: {}", db.display());

        let checkpoint = |p: &std::path::Path| {
            if let Ok(conn) = Connection::open(p) {
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            }
        };
        checkpoint(&db);

        // 第二份副本：用来复现"被移出启动路径的那段工作"（跑完就是 4 列，不能再当老库用）
        let baseline_db = db.with_file_name(format!(
            "{}-legacy-baseline.db",
            db.file_stem().and_then(|s| s.to_str()).unwrap_or("copy")
        ));
        for suffix in ["", "-wal", "-shm"] {
            let from = PathBuf::from(format!("{}{}", db.display(), suffix));
            if from.exists() {
                std::fs::copy(&from, PathBuf::from(format!("{}{}", baseline_db.display(), suffix)))
                    .expect("复制副本");
            }
        }
        checkpoint(&baseline_db);
        eprintln!(
            "[bench] 副本: 主 {} | 基线 {}",
            db.display(),
            baseline_db.display()
        );

        let cols_before = global_index_columns(&Connection::open(&db).unwrap()).unwrap();
        let rows_before = copy_bench_rows(&Connection::open(&db).unwrap());
        let size_before = copy_bench_size(&db);
        eprintln!(
            "[bench] 改造前: 列 {} 个 {:?} | 行 {} | 文件 {:.1} MB",
            cols_before.len(),
            cols_before,
            rows_before,
            size_before as f64 / 1048576.0
        );
        if is_new_global_index_schema(&cols_before) {
            eprintln!("[bench] 警告：副本已是新 schema，收益对比无意义（请用未收拢的副本）");
        }
        if let Some(b) = bench_table_bytes(&Connection::open(&db).unwrap(), "global_index") {
            eprintln!("[bench] 改造前 global_index 表 {:.1} MB", b as f64 / 1048576.0);
        }

        const ROUNDS: usize = 4;
        let before = bench_load_rounds(&db, ROUNDS);
        report_rounds("改造前 载入(同构 4 列)", &before);

        // ── ①-a 复现"改造前"的启动路径工作：open_index_conn 里的那次迁移（含事务外 VACUUM）──
        let t = Instant::now();
        {
            let mut conn = Connection::open(&baseline_db).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
            let _ = conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024i64);
            migrate_global_index_schema(&mut conn).expect("基线副本上的迁移");
        }
        let old_startup_work = t.elapsed();
        let base_size = copy_bench_size(&baseline_db);
        let base_load = bench_load_rounds(&baseline_db, ROUNDS);
        eprintln!(
            "[bench] ①-a 改造前启动路径（含迁移 + VACUUM）: {:.1}s | 迁移后文件 {:.1} MB | 列 {}",
            old_startup_work.as_secs_f64(),
            base_size as f64 / 1048576.0,
            global_index_columns(&Connection::open(&baseline_db).unwrap())
                .unwrap()
                .len()
        );
        report_rounds("①-a 迁移后 载入(同构 4 列)", &base_load);

        // ── ①-b 改造后的启动路径：open_index_conn 只建表，DiskCache::open 全流程 ──
        let t_index = Instant::now();
        {
            let _ic = DiskCache::open_index_conn(&db).expect("索引连接");
        }
        let index_conn_cost = t_index.elapsed();
        let t_open = Instant::now();
        let cache = Arc::new(DiskCache::open(db.clone()).expect("打开副本"));
        let open_cost = t_open.elapsed();
        let cols_after_open = global_index_columns(&Connection::open(&db).unwrap()).unwrap();
        assert_eq!(
            cols_after_open.len(),
            9,
            "改造后的启动路径不得改变 schema: {cols_after_open:?}"
        );
        eprintln!(
            "[bench] ①-b 改造后 open_index_conn: {:?} | DiskCache::open 全程: {:?} | 列 {}（未被触碰）",
            index_conn_cost,
            open_cost,
            cols_after_open.len()
        );
        eprintln!(
            "[bench] ① 启动路径冻结: {:.1}s → {:.1}ms（{:.0}×，其中索引连接 {:?}）",
            old_startup_work.as_secs_f64(),
            open_cost.as_secs_f64() * 1000.0,
            old_startup_work.as_secs_f64() / open_cost.as_secs_f64().max(1e-6),
            index_conn_cost
        );

        // ── ② 用户主动重建索引：save_global_index_stream 顺带把 schema 收拢到 4 列 ──
        // 生产里这条路径的条目本来就在内存里（`finish_building` 持有整份索引），这里同样先载入。
        let t = Instant::now();
        let entries = cache.load_global_index().expect("载入全部条目");
        let load_before_rebuild = t.elapsed();
        assert_eq!(entries.len() as i64, rows_before, "载入条数必须与库一致");
        eprintln!(
            "[bench] 重建前 生产 load_global_index: {:?}（{} 条，老 schema）",
            load_before_rebuild,
            entries.len()
        );

        let (tx, rx) = std::sync::mpsc::channel::<Vec<IndexEntry>>();
        let saver = {
            let c = Arc::clone(&cache);
            std::thread::spawn(move || c.save_global_index_stream(rx))
        };
        let t = Instant::now();
        for chunk in entries.chunks(5000) {
            tx.send(chunk.to_vec()).expect("发送分块");
        }
        drop(tx);
        saver.join().unwrap().expect("save_global_index_stream 必须成功");
        // 计时覆盖：逐块发送 + 写入事务 + 提交 + 事务外 VACUUM（join 之后才停表）
        let rebuild_cost = t.elapsed();
        drop(entries); // 释放 ~500MB+，后面的载入计时才不受内存压力干扰

        let cols_after = global_index_columns(&Connection::open(&db).unwrap()).unwrap();
        let rows_after = copy_bench_rows(&Connection::open(&db).unwrap());
        let size_after = copy_bench_size(&db);
        assert_eq!(rows_after, rows_before, "重建不得改变行数");
        assert!(is_new_global_index_schema(&cols_after), "重建后必须是 4 列: {cols_after:?}");
        eprintln!(
            "[bench] ② 重建（save_global_index_stream，含收拢 + VACUUM）: {:.1}s",
            rebuild_cost.as_secs_f64()
        );
        eprintln!(
            "[bench] ② 重建后: 列 {} 个 {:?} | 行 {} | 文件 {:.1} MB（{:.1} MB → {:.1} MB, {:+.1}%）",
            cols_after.len(),
            cols_after,
            rows_after,
            size_after as f64 / 1048576.0,
            size_before as f64 / 1048576.0,
            size_after as f64 / 1048576.0,
            (size_after as f64 - size_before as f64) * 100.0 / size_before as f64
        );
        eprintln!(
            "[bench] ② 基线对照（旧迁移路径，①-a）: 文件 {:.1} MB（{:.1} MB → {:.1} MB, {:+.1}%）",
            base_size as f64 / 1048576.0,
            size_before as f64 / 1048576.0,
            base_size as f64 / 1048576.0,
            (base_size as f64 - size_before as f64) * 100.0 / size_before as f64
        );
        if let Some(b) = bench_table_bytes(&Connection::open(&db).unwrap(), "global_index") {
            eprintln!("[bench] ② 重建后 global_index 表 {:.1} MB", b as f64 / 1048576.0);
        }

        // ── ③ 重建后的载入（同构 + 生产）──
        let after = bench_load_rounds(&db, ROUNDS);
        report_rounds("③ 重建后 载入(同构 4 列)", &after);
        let mut prod = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let t = Instant::now();
            let v = cache.load_global_index().expect("生产载入");
            let s = t.elapsed().as_secs_f64();
            drop(v);
            prod.push(s);
        }
        report_rounds("③ 重建后 load_global_index(生产)", &prod);
        eprintln!(
            "[bench] ③ 载入提速: 同构 {:.3}s → {:.3}s ({:+.1}%)",
            min_secs(&before),
            min_secs(&after),
            (min_secs(&after) - min_secs(&before)) * 100.0 / min_secs(&before)
        );

        // 收拢后写入必须自动切到 4 列
        cache
            .upsert_global_index_entries(&[IndexEntry::new(
                "C:/__bench__/probe.bin".into(),
                "probe.bin".into(),
                1,
                false,
                1,
            )])
            .expect("新 schema 下 upsert");
        assert_eq!(copy_bench_rows(&Connection::open(&db).unwrap()), rows_after + 1);
        cache
            .remove_global_index_by_paths(&["C:/__bench__/probe.bin".to_string()])
            .expect("删除探针行");

        drop(cache);

        // ── ④ 复核：重建里那次 VACUUM 是否已经把空间还干净 ──
        let size_pre_vacuum2 = copy_bench_size(&db);
        let t = Instant::now();
        match Connection::open(&db).and_then(|c| c.execute_batch("VACUUM")) {
            Ok(()) => eprintln!(
                "[bench] ④ 二次 VACUUM 耗时 {:.3}s, 文件 {:.1} MB → {:.1} MB（重建内 VACUUM 已回收干净）",
                t.elapsed().as_secs_f64(),
                size_pre_vacuum2 as f64 / 1048576.0,
                copy_bench_size(&db) as f64 / 1048576.0
            ),
            Err(e) => eprintln!("[bench] ④ 二次 VACUUM 失败: {e:#}"),
        }
    }

    /// P0-3b：全局索引 I/O 期间目录缓存是否仍可用。
    ///
    /// 旧实现只有一把 `Mutex<Option<Connection>>`，`load_global_index`（310 万行全表 SELECT，
    /// 约 1~2s）与 `save_global_index_stream` 都在锁下执行，期间 `get_stats` / blob 读写全部
    /// 排队——这里用"采样线程的最大单次延迟"把它量化出来。
    #[test]
    #[ignore = "benchmark: 依赖本机 cache_v2.db"]
    fn bench_index_io_does_not_block_dir_cache() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let cache = DiskCache::instance();
        let small = match cache.smallest_blob_path_for_bench() {
            Some(p) => p,
            None => {
                eprintln!("[bench] 无 blob 缓存，跳过");
                return;
            }
        };
        eprintln!("[bench] 目录缓存读目标(最小 blob): {}", small);

        // 预热：blob 进页缓存、元数据页进 SQLite 缓存
        let _ = cache.get_stale_with_usn(&small);
        let _ = cache.get_stats();

        let stop = Arc::new(AtomicBool::new(false));
        let cnt_meta = Arc::new(AtomicU64::new(0));
        let sum_meta = Arc::new(AtomicU64::new(0));
        let max_meta = Arc::new(AtomicU64::new(0));
        let cnt_blob = Arc::new(AtomicU64::new(0));
        let max_blob = Arc::new(AtomicU64::new(0));

        let sampler = {
            let (s, cm, sm, mm, cb, mb, path) = (
                stop.clone(),
                cnt_meta.clone(),
                sum_meta.clone(),
                max_meta.clone(),
                cnt_blob.clone(),
                max_blob.clone(),
                small.clone(),
            );
            std::thread::spawn(move || {
                let cache = DiskCache::instance();
                let mut i = 0u64;
                while !s.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    let _ = cache.get_stats();
                    let _ = cache.verified_usn(&path);
                    let ns = t.elapsed().as_nanos() as u64;
                    sm.fetch_add(ns, Ordering::Relaxed);
                    cm.fetch_add(1, Ordering::Relaxed);
                    let prev = mm.load(Ordering::Relaxed);
                    if ns > prev {
                        mm.store(ns, Ordering::Relaxed);
                    }
                    // 每 32 轮做一次真正的 blob 读（小目录），单独统计
                    if i % 32 == 31 {
                        let t = Instant::now();
                        let _ = cache.get_stale_with_usn(&path);
                        let ns = t.elapsed().as_nanos() as u64;
                        cb.fetch_add(1, Ordering::Relaxed);
                        let prev = mb.load(Ordering::Relaxed);
                        if ns > prev {
                            mb.store(ns, Ordering::Relaxed);
                        }
                    }
                    i += 1;
                    std::thread::yield_now();
                }
            })
        };

        std::thread::sleep(Duration::from_millis(300));
        let t = Instant::now();
        let loaded = cache.load_global_index().map(|e| e.len()).unwrap_or(0);
        let idx_elapsed = t.elapsed();
        stop.store(true, Ordering::Relaxed);
        sampler.join().unwrap();

        let cm = cnt_meta.load(Ordering::Relaxed).max(1);
        eprintln!(
            "[bench] 索引 I/O: load_global_index {} 条, {:?}",
            loaded, idx_elapsed
        );
        eprintln!(
            "[bench] 同期目录缓存元数据操作 {} 次: 平均 {:.3}ms, 最大 {:.1}ms",
            cm,
            sum_meta.load(Ordering::Relaxed) as f64 / cm as f64 / 1e6,
            max_meta.load(Ordering::Relaxed) as f64 / 1e6
        );
        eprintln!(
            "[bench] 同期目录缓存 blob 读 {} 次: 最大 {:.1}ms",
            cnt_blob.load(Ordering::Relaxed),
            max_blob.load(Ordering::Relaxed) as f64 / 1e6
        );
    }

    /// P0-2：最大 blob 的落盘形态与反序列化耗时（依赖本机真实缓存，只读，不改任何文件）。
    ///
    /// 旧形态 v2 = `bincode(Vec<scan::Item>)`；新形态 v3 = `bincode(Vec<ItemDisk>)`。
    /// 若库里还是旧文件，本基准就用**同一批条目**编码出新形态并解码，给出可比的
    /// 字节数与耗时，同时校验往返条目数一致、派生 name 与旧存储值一致。
    ///
    /// 运行：cargo test --release --lib -- --ignored --nocapture bench_blob_disk_form
    #[test]
    #[ignore = "benchmark: 依赖本机 blob 缓存"]
    fn bench_blob_disk_form() {
        use std::time::Instant;

        let cache = DiskCache::instance();
        let t = Instant::now();
        let raw = match cache.raw_largest_blob_for_bench() {
            Some(b) => b,
            None => {
                eprintln!("[bench] 无 blob 缓存，跳过");
                return;
            }
        };
        let read_elapsed = t.elapsed();
        let path = cache.largest_blob_path_for_bench().unwrap_or_default();
        if raw.len() < 7 {
            eprintln!("[bench] blob 文件头异常，跳过");
            return;
        }
        let version = raw[4];
        let hdr = 7 + u16::from_le_bytes([raw[5], raw[6]]) as usize;
        eprintln!(
            "[bench] 最大 blob {}: {} B ({:.1} MB), 版本 v{}, 读取 {:?}",
            path,
            raw.len(),
            raw.len() as f64 / 1048576.0,
            version,
            read_elapsed
        );
        if raw.len() <= hdr {
            eprintln!("[bench] blob 头长度异常，跳过");
            return;
        }
        let payload_len = raw.len() - hdr;

        // 旧形态解码（只有库里还是 v2 文件时可得；旧值：370.6MB / 1,931,004 条 / 646ms）
        let mut old: Option<Vec<Item>> = if version == 2 {
            let t = Instant::now();
            let items: Vec<Item> = bincode::deserialize(&raw[hdr..]).unwrap();
            eprintln!(
                "[bench] 旧形态 v2 反序列化 {} 条: {:?}",
                items.len(),
                t.elapsed()
            );
            // 审计：name 是否恒等于 path 末段（决定能否只落盘 path）
            let mism = items
                .iter()
                .filter(|i| i.name.as_str() != crate::scan::name_from_path(&i.path).as_str())
                .count();
            let nonempty = items.iter().filter(|i| !i.size_formatted.is_empty()).count();
            eprintln!(
                "[bench] 审计: name≠path末段 {} 条, size_formatted 非空 {} 条（后者不参与 IPC 契约）",
                mism,
                nonempty
            );
            Some(items)
        } else {
            None
        };

        // 新形态：库里已是 v3 就直接量；否则用同一批条目就地转编码。
        // 注意 `raw` 与 `compact` 都保持存活（各 300~390MB），逐轮测量时不再重新编码。
        let (new_payload, new_bytes) = match &old {
            Some(items) => {
                let t = Instant::now();
                let compact = bincode::serialize(&ItemDiskSeq(items)).unwrap();
                eprintln!(
                    "[bench] 新形态 v3 编码: {} B ({:.1} MB, 比旧 payload {:.1}%), 编码 {:?}",
                    compact.len(),
                    compact.len() as f64 / 1048576.0,
                    compact.len() as f64 * 100.0 / payload_len as f64,
                    t.elapsed()
                );
                let n = compact.len();
                (compact, n)
            }
            None => {
                eprintln!(
                    "[bench] 磁盘上已是新形态 v3: {} B ({:.1} MB)",
                    payload_len,
                    payload_len as f64 / 1048576.0
                );
                (raw[hdr..].to_vec(), payload_len)
            }
        };

        // 正确性：条目数一致 + 派生 name 与旧存储值逐条一致
        let t = Instant::now();
        let back = decode_items_payload(&new_payload).expect("新形态必须能解码");
        eprintln!(
            "[bench] 新形态 v3 首次解码 {} 条: {:?} ({} B/item)",
            back.len(),
            t.elapsed(),
            new_bytes / back.len().max(1)
        );
        if let Some(old_items) = &old {
            assert_eq!(back.len(), old_items.len(), "往返必须保持条目数");
            let mism = back
                .iter()
                .zip(old_items.iter())
                .filter(|(a, b)| a.name != b.name)
                .count();
            assert_eq!(mism, 0, "派生 name 必须与旧存储值一致");
        }
        assert!(
            back.iter().all(|i| !i.name.is_empty() || i.path.ends_with('/')),
            "除根目录外，派生 name 不应为空"
        );
        drop(back);
        if let Some(items) = old.take() {
            drop(items);
        }

        // ── 逐轮交替测量 ──
        // 每轮都要分配/释放 ~500MB 的 Vec<Item>，先分配的测量会被后面的分配拖慢
        // （mimalloc 归还页 + 再次首次触碰），因此必须**交替**跑并取各轮最小值，
        // 否则单次测量能差出 ±40%（见 bench_load_global_index 同样的抖动）。
        // A = 旧形态 v2（Vec<Item>，直接从流里读 name）
        // B = 新形态 v3 只做 bincode（不含 name 派生）
        // C = 新形态 v3 生产路径（bincode + 并行派生 name）
        let mut best = [f64::MAX; 3];
        for round in 0..4 {
            let mut ms = [0.0f64; 3];
            if version == 2 {
                let t = Instant::now();
                let v: Vec<Item> = bincode::deserialize(&raw[hdr..]).unwrap();
                ms[0] = t.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(v.len(), 1_931_004, "C:/ blob 条目数基线");
                drop(v);
            }
            {
                let t = Instant::now();
                let v = bincode::deserialize::<ItemsFromBlob>(&new_payload).unwrap().0;
                ms[1] = t.elapsed().as_secs_f64() * 1000.0;
                drop(v);
            }
            {
                let t = Instant::now();
                let v = decode_items_payload(&new_payload).unwrap();
                ms[2] = t.elapsed().as_secs_f64() * 1000.0;
                drop(v);
            }
            for i in 0..3 {
                if ms[i] < best[i] {
                    best[i] = ms[i];
                }
            }
            eprintln!(
                "[bench] round{} 旧v2 {:>7.1}ms | 新v3 bincode {:>7.1}ms | 新v3 生产(含并行派生) {:>7.1}ms",
                round, ms[0], ms[1], ms[2]
            );
        }
        if version == 2 {
            eprintln!(
                "[bench] 最小值: 旧v2 {:.1}ms → 新v3 bincode {:.1}ms / 生产(含并行派生) {:.1}ms ({:+.1}%)",
                best[0],
                best[1],
                best[2],
                (best[2] - best[0]) * 100.0 / best[0]
            );
        } else {
            eprintln!(
                "[bench] 最小值(库里已是 v3): bincode {:.1}ms / 生产(含并行派生) {:.1}ms",
                best[1], best[2]
            );
        }

        // 派生阶段：并行 vs 顺序（说明单线程 rfind 为什么要摊到多核）
        {
            let mut v = bincode::deserialize::<ItemsFromBlob>(&new_payload).unwrap().0;
            let t = Instant::now();
            fill_derived_names(&mut v);
            let par = t.elapsed();
            let t = Instant::now();
            for it in v.iter_mut() {
                it.name = crate::scan::name_from_path(&it.path);
            }
            eprintln!(
                "[bench] 派生 name: 并行 {:?} vs 顺序 {:?}（{} 条）",
                par,
                t.elapsed(),
                v.len()
            );
            drop(v);
        }

        // 目录缓存完整加载路径（v2 文件在 bump 后会被判为未命中，属预期）
        let t = Instant::now();
        let loaded = cache.get_stale_with_usn(&path);
        eprintln!(
            "[bench] get_stale_with_usn({}) 完整加载: {:?} ({:?} 条)",
            path,
            t.elapsed(),
            loaded.as_ref().map(|(r, _)| r.items.len())
        );
    }
}
