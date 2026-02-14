use serde::{Deserialize, Serialize};

pub struct BinarySerializer;

impl BinarySerializer {
    #[inline]
    pub fn serialize<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(value)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryPayload {
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub compressed: bool,
    pub original_size: usize,
}

impl BinaryPayload {
    pub fn from_data<T: Serialize>(value: &T, _compress_threshold: usize) -> anyhow::Result<Self> {
        let serialized = BinarySerializer::serialize(value)?;
        let original_size = serialized.len();

        #[cfg(feature = "zstd")]
        if original_size > compress_threshold {
            use std::io::Cursor;
            if let Ok(compressed) = zstd::stream::encode_all(Cursor::new(&serialized), 3) {
                if compressed.len() < original_size * 8 / 10 {
                    return Ok(Self {
                        data: compressed,
                        compressed: true,
                        original_size,
                    });
                }
            }
        }

        Ok(Self {
            data: serialized,
            compressed: false,
            original_size,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizedScanResult {
    pub path: String,
    pub total_size: i64,
    pub total_size_formatted: String,
    pub scan_time: f64,
    pub item_count: usize,
    pub has_timing: bool,
    pub timing_scan: f64,
    pub timing_compute: f64,
    pub timing_format: f64,
    pub timing_total: f64,
    #[serde(with = "serde_bytes")]
    pub items_data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizedItem {
    pub path: String,
    pub name: String,
    pub size: i64,
    pub size_formatted: String,
    pub is_dir: bool,
}

impl From<crate::scan::ScanResult> for OptimizedScanResult {
    fn from(result: crate::scan::ScanResult) -> Self {
        let items: Vec<OptimizedItem> = result.items.into_iter().map(|item| OptimizedItem {
            path: item.path.to_string(),
            name: item.name.to_string(),
            size: item.size,
            size_formatted: item.size_formatted.to_string(),
            is_dir: item.is_dir,
        }).collect();

        let items_data = BinarySerializer::serialize(&items).unwrap_or_default();
        let has_timing = result.timing.is_some();
        let timing = result.timing.unwrap_or_default();

        Self {
            path: result.path.to_string(),
            total_size: result.total_size,
            total_size_formatted: result.total_size_formatted.to_string(),
            scan_time: result.scan_time,
            item_count: items.len(),
            has_timing,
            timing_scan: timing.scan_phase,
            timing_compute: timing.compute_phase,
            timing_format: timing.format_phase,
            timing_total: timing.total,
            items_data,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfMetricsPayload {
    pub scan_id: String,
    pub duration_ms: u64,
    pub io_phase_ms: u64,
    pub compute_phase_ms: u64,
    pub serialize_phase_ms: u64,
    pub files_scanned: usize,
    pub dirs_scanned: usize,
    pub throughput_mbps: f64,
    pub memory_peak_mb: f64,
    pub cache_hit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    pub requests: Vec<SingleRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleRequest {
    pub id: String,
    pub path: String,
    pub force_refresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse {
    pub results: Vec<SingleResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleResponse {
    pub id: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub success: bool,
    pub error: Option<String>,
}
