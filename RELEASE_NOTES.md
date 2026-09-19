# FlashDir Release Notes

## Unreleased（二）—— 实机验证中发现的 USN 深层缺陷

> 这一批问题都是"编译通过 + 实机跑一遍"才暴露出来的：USN 增量此前从未真正成功过。

- **`FSCTL_READ_USN_JOURNAL` 常量少了 METHOD_NEITHER 位**：应为 `0x000900BB`，
  原值 `0x000900B8`（METHOD_BUFFERED）被驱动直接拒绝并返回
  `ERROR_INVALID_FUNCTION`，导致增量链路永远走不通。
- **`USN_JOURNAL_DATA.MaxUsn` 不是位置**：它是 Journal 的 USN 上限常量
  （NTFS 上恒为 2^63-2^16）。检查点字段改为 `NextUsn`（下一个待分配 USN），
  并新增范围校验：校验点低于 `LowestValidUsn` 或高于 `NextUsn` 时判为
  窗口失效 → 全量扫描（历史版本写入的错误值会被自动纠正）。
- **USN 判定"窗口失效"后必须真正跳过缓存**：此前只打日志、仍回落到 mtime 缓存，
  继续返回旧数据；现在置位 `force_full_scan` 直接走全量 MFT。
- **FRN→路径解析要还原"记录发生时"的祖先名称**：若父目录在同一增量窗口内被
  改名，早期记录（删除/修改）会解析到改名后的新路径而匹配不上缓存，删除被
  静默丢弃。现在按记录的 USN 回溯改名历史取旧名。
- **单条 MFT 记录解析补 `$DATA` 大小回退**：`$FILE_NAME.RealSize` 对常驻/小文件
  常为 0，全量解析器有回退而单条解析器没有，导致增量更新把文件大小写成 0。
- 旧库兼容：没有 `index_meta` 时按"全盘索引"处理，避免升级后误显示"部分目录"。
- 前端依赖：`tailwindcss@^3.4.29` 在 npm 上不存在（3.x 最新为 3.4.19），
  `npm ci` 必然 404；已修正版本并重新生成 lockfile（vite 7.3.6 / vue 3.5.43）。
- **`Cargo.toml` 缺少 `custom-protocol` feature**：Tauri 用它判定 dev/prod，
  直接 `cargo build --release` 会生成"dev 模式"二进制（只嵌 `devUrl`，
  打开后显示 localhost 拒绝连接）。已补上标准 feature 声明，
  并在 README 明确构建命令。

## Unreleased —— 正确性 / 新鲜度 / 性能修复

### 扫描新鲜度（重要）
- **USN 增量真正生效**：修正 `FSCTL_READ_USN_JOURNAL` 输出解析 —— 缓冲区前 8 字节
  是"下次读取起点"，之前从 offset 0 解析会把该值当成 `RecordLength`，导致增量被
  静默跳过、却把过期缓存当作最新结果回写。
- **按目录记录已校验 USN**：`scan_meta.verified_usn` / 内存缓存条目记录
  "这份数据已校验到哪个 USN"，只有非零时才做增量；Journal 回滚或变更 > 5000 条时
  回退全量 MFT 扫描，不再返回陈旧数据。
- **增量循环拉取**：单次读取不再只取一个 4MB 批次就推进检查点（那会永久丢失
  窗口之外的变更），改为循环追平 Journal。
- **只应用被扫描目录内的变更**，修复"扫 A 目录却混进 B 目录新增文件"的污染问题。
- **缓存路径格式统一为绝对路径**：修复 USN 增量与缓存 key 不一致导致的
  删除失效、重复条目、相对路径外泄。
- **子目录缓存推导加 mtime 约束**：父缓存写入时间早于子目录 mtime 时不再推导，
  避免子目录永远返回父快照里的旧数据。
- 新增工具栏 **刷新** 按钮：忽略内存/磁盘/USN 缓存强制重扫。

### 性能
- 分页/排序/过滤/目录树改为共享 `Arc<Vec<Item>>`，不再每次请求深拷贝全量条目。
- 磁盘缓存容量统计改为字节级真实记账，淘汰按"整份目录"回收至 75%，
  修复"计数只增不减 → 每次写入都淘汰缓存"的抖动。
- 全局搜索：候选集先做 O(n) 部分选择再排序；不再对百万级候选全量排序。
- 重命令（重复文件哈希、开发者分析、快照 diff、全局搜索、索引追加）下沉到
  `spawn_blocking`，不再阻塞 IPC 主线程。
- 名称排序改为免分配的 Unicode 小写比较。

### 其他修复
- 本地过滤真正支持 Everything 式语法（`ext:` / `size:` / `type:` / `dir:` /
  `mtime:` / `NOT` / 通配符），与工具栏提示、README 一致。
- 全局搜索不再把 `NOT 文本` 当作分桶关键字；未识别的 `key:value` 不再被丢弃；
  过滤条件为空时返回空结果，而不是退化成"返回全量中最大的若干项"。
- Top 5 大文件独立按大小挑选，不再受当前排序影响。
- 目录树前缀基于 canonicalize 路径构造，修复用户输入大小写不一致时目录树为空。
- 快照只序列化条目列表；移除"用当前页 100 条当全量快照"的错误回退。
- GUI 历史记录恢复写入（分页重构后曾丢失）。
- 全局索引区分"全盘构建"与"部分目录"（`partial`），恢复 `failed_drives` 统计。
- MFT 记录编号在短读时不再错位；目录遍历失败会告警而不是静默漏扫；
  移除 `FIND_FIRST_EX_CASE_SENSITIVE`。
- 取消改为"代（generation）"模型：新扫描不再清掉在飞扫描的取消请求。
- 提权重启成功后旧进程立即退出，避免双实例/双托盘。

## FlashDir v3.4.2 Release Notes

## What's New

- **New UI**: observability-dashboard layout with brand, overview cards, wider side panel
- **Directory tree**: lazy-loaded, starts collapsed, collapses with one click, bounded scroll area
- ~~**Treemap**~~: 已在后续重构中移除（README 已同步）
- **Duplicate file detection**: size + content hash grouping
- ~~**Directory change watching**~~: 已移除；USN 能力改为服务于"扫描时的增量校验"
- **Scan cancellation**: cancel long scans from the toolbar
- **Runtime diagnostics**: cache/index/USN/permission status panel
- **About dialog**: project info and links

## Performance

- Directory aggregation optimized from O(files × depth) to O(files + dirs)
- Disk cache switched from BLOB snapshots to item-level SQLite rows
- Subdirectory scans derived from upper-level memory/disk caches
- Frontend uses backend paging, no longer sends full item lists
- Global search index:
  - batch restore from SQLite
  - parallel MFT scanning across volumes
  - streamed persistence without full Vec clone
  - cached extension field for faster `ext:` filtering
  - incremental disk index updates

## Fixes

- Fixed frontend crash on large directory scans
- Filtered MFT `<record_xxx>` placeholders from results/tree
- Fixed global search status stuck at "0 items"
- Fixed history/checkpoint atomic writes
- Cleaned dead code and stale dependencies

## Build

- Platform: Windows
- Requires Windows 10/11
- Admin recommended for MFT direct scan
