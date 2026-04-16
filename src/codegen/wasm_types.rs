use wasm_encoder::ValType;

use crate::types::WasmType;

pub fn wasm_results(ty: WasmType) -> Vec<ValType> {
    match ty.to_val_type() {
        Some(vt) => vec![vt],
        None => vec![],
    }
}

pub fn wasm_params(types: &[WasmType]) -> Vec<ValType> {
    types.iter().filter_map(|t| t.to_val_type()).collect()
}
