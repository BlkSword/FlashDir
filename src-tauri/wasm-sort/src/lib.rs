// WASM 排序模块
// 提供高性能的排序和过滤功能

use wasm_bindgen::prelude::*;
use serde::{Deserialize, Serialize};

/// 文件项结构（WASM 版本）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmItem {
    pub path: String,
    pub name: String,
    pub size: i64,
    pub size_formatted: String,
    pub is_dir: bool,
}

/// 排序配置
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortColumn {
    Name,
    Size,
    Type,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortDirection {
    Asc,
    Desc,
}

/// 初始化 WASM 模块
#[wasm_bindgen(start)]
pub fn start() {
    // 初始化 console panic hook
    console_error_panic_hook::set_once();
}

/// 排序项目列表
#[wasm_bindgen]
pub fn sort_items(items_js: JsValue, column: &str, direction: &str) -> JsValue {
    let mut items: Vec<WasmItem> = serde_wasm_bindgen::from_value(items_js)
        .unwrap_or_default();

    let column = match column {
        "name" => SortColumn::Name,
        "size" => SortColumn::Size,
        "type" => SortColumn::Type,
        _ => SortColumn::Size,
    };

    let direction = match direction {
        "asc" => SortDirection::Asc,
        "desc" => SortDirection::Desc,
        _ => SortDirection::Desc,
    };

    items.sort_unstable_by(|a, b| compare_items(a, b, column, direction));

    serde_wasm_bindgen::to_value(&items).unwrap_or(JsValue::NULL)
}

/// 过滤项目列表
#[wasm_bindgen]
pub fn filter_items(items_js: JsValue, keyword: &str) -> JsValue {
    let items: Vec<WasmItem> = serde_wasm_bindgen::from_value(items_js.clone())
        .unwrap_or_default();

    if keyword.is_empty() {
        return items_js;
    }

    let lower_keyword = keyword.to_lowercase();

    let filtered: Vec<WasmItem> = items
        .into_iter()
        .filter(|item| {
            item.name.to_lowercase().contains(&lower_keyword) ||
            item.path.to_lowercase().contains(&lower_keyword)
        })
        .collect();

    serde_wasm_bindgen::to_value(&filtered).unwrap_or(JsValue::NULL)
}

/// 排序并过滤
#[wasm_bindgen]
pub fn sort_and_filter_items(
    items_js: JsValue,
    column: &str,
    direction: &str,
    keyword: &str,
) -> JsValue {
    let filtered = filter_items(items_js, keyword);
    sort_items(filtered, column, direction)
}

/// 批量获取文件扩展名统计
#[wasm_bindgen]
pub fn get_extension_stats(items_js: JsValue) -> JsValue {
    let items: Vec<WasmItem> = serde_wasm_bindgen::from_value(items_js)
        .unwrap_or_default();

    use std::collections::HashMap;

    let mut stats: HashMap<String, (i64, usize)> = HashMap::new();

    for item in items {
        if !item.is_dir {
            let ext = item.name
                .split('.')
                .last()
                .unwrap_or("no-ext")
                .to_lowercase();

            stats.entry(ext)
                .and_modify(|(size, count)| {
                    *size += item.size;
                    *count += 1;
                })
                .or_insert((item.size, 1));
        }
    }

    // 按大小排序
    let mut sorted_stats: Vec<_> = stats.into_iter().collect();
    sorted_stats.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));

    serde_wasm_bindgen::to_value(&sorted_stats).unwrap_or(JsValue::NULL)
}

/// 获取 Top N 大文件
#[wasm_bindgen]
pub fn get_top_items(items_js: JsValue, n: usize) -> JsValue {
    let mut items: Vec<WasmItem> = serde_wasm_bindgen::from_value(items_js)
        .unwrap_or_default();

    // 按大小排序
    items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    // 取前 N 个
    let top_items: Vec<WasmItem> = items.into_iter().take(n).collect();

    serde_wasm_bindgen::to_value(&top_items).unwrap_or(JsValue::NULL)
}

/// 比较函数
#[inline]
fn compare_items(
    a: &WasmItem,
    b: &WasmItem,
    column: SortColumn,
    direction: SortDirection,
) -> std::cmp::Ordering {
    let ordering = match column {
        SortColumn::Name => {
            a.name.cmp(&b.name)
        }
        SortColumn::Size => {
            a.size.cmp(&b.size)
        }
        SortColumn::Type => {
            let a_type = if a.is_dir { 0 } else { 1 };
            let b_type = if b.is_dir { 0 } else { 1 };
            let type_ord = a_type.cmp(&b_type);
            if type_ord == std::cmp::Ordering::Equal {
                a.name.cmp(&b.name)
            } else {
                type_ord
            }
        }
    };

    match direction {
        SortDirection::Asc => ordering,
        SortDirection::Desc => ordering.reverse(),
    }
}

/// 获取版本信息
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 性能测试 - 排序指定数量的随机项
#[wasm_bindgen]
pub fn benchmark_sort(count: usize) -> f64 {
    use web_sys::console;

    let mut items: Vec<WasmItem> = (0..count)
        .map(|i| WasmItem {
            path: format!("path/to/file{}.txt", i),
            name: format!("file{}.txt", i),
            size: (i * 1024) as i64,
            size_formatted: format!("{} KB", i),
            is_dir: false,
        })
        .collect();

    let start = js_sys::Date::now();

    items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let end = js_sys::Date::now();

    let duration_ms = end - start;

    console::log_1(&format!("Sorted {} items in {} ms", count, duration_ms).into());

    duration_ms
}
