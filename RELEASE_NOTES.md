# FlashDir v3.4.2 Release Notes

## What's New

- **New UI**: observability-dashboard layout with brand, overview cards, wider side panel
- **Directory tree**: lazy-loaded, starts collapsed, collapses with one click, bounded scroll area
- **Treemap**: first-level directory view with on-demand drill-down
- **Duplicate file detection**: size + content hash grouping
- **Directory change watching**: near-real-time USN-based monitoring
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
