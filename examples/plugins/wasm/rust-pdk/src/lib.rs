//! WASM Plugin Example using Extism PDK
//! 
//! To compile:
//! cargo build --target wasm32-wasip1 --release

use extism_pdk::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct HookResult {
    pub prevent_default: bool,
    pub stop_propagation: bool,
    pub value: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct Hook {
    pub name: String,
    pub data: serde_json::Value,
}

#[plugin_fn]
pub fn on_scan_complete(Json(hook): Json<Hook>) -> FnResult<Json<HookResult>> {
    // Basic logic: Log scan completion to the gravityfile console
    // In WASM, we'd typically use specialized host functions
    // but here we just return a result.
    
    Ok(Json(HookResult {
        prevent_default: false,
        stop_propagation: false,
        value: Some(serde_json::json!({
            "status": "processed_by_wasm",
            "hook": hook.name
        })),
    }))
}
