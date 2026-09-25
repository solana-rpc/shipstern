//! Decode every sampled account and instruction with both generated parser
//! sets: from the Rust-converted nodes and from the JS CLI's nodes.
//!
//! Reads `.cache/samples/<id>.json`, writes `.cache/decoded/<id>.json`.

use std::{fs, path::Path, str::FromStr};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

fn outcome<T: serde::Serialize>(result: Option<Result<T, String>>) -> Value {
    match result {
        None => json!({ "status": "no-parser" }),
        Some(Ok(value)) => json!({ "status": "ok", "value": value }),
        Some(Err(err)) => json!({ "status": "error", "error": err }),
    }
}

fn main() {
    let cache = Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache");
    let out_dir = cache.join("decoded");

    fs::create_dir_all(&out_dir).expect("create decoded dir");

    let mut files: Vec<_> = fs::read_dir(cache.join("samples"))
        .expect("samples dir; run js/sample.mjs first")
        .flatten()
        .map(|e| e.path())
        .collect();

    files.sort();

    for path in files {
        let sample: Value =
            serde_json::from_slice(&fs::read(&path).expect("read sample")).expect("sample JSON");
        let program = sample["programId"].as_str().unwrap_or_default().to_owned();

        // Built in batches: leave other batches' output alone.
        if !anchor_parity::PROGRAMS.contains(&program.as_str()) {
            continue;
        }

        let accounts: Vec<Value> = sample["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| {
                let data = STANDARD
                    .decode(a["data"].as_str().unwrap_or_default())
                    .unwrap_or_default();
                let shape = |r: Option<Result<(String, Value), String>>| {
                    r.map(|r| r.map(|(name, value)| json!({ "type": name, "fields": value })))
                };

                json!({
                    "address": a["address"],
                    "type": a["type"],
                    "rust": outcome(shape(anchor_parity::decode_account(&program, &data))),
                    "js": outcome(shape(anchor_parity::decode_account_js(&program, &data))),
                })
            })
            .collect();

        let instructions: Vec<Value> = sample["instructions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|ix| {
                let data = STANDARD
                    .decode(ix["data"].as_str().unwrap_or_default())
                    .unwrap_or_default();
                let keys: Vec<shipstern_core::Pubkey> = ix["accounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| {
                        k.as_str()
                            .and_then(|k| shipstern_core::Pubkey::from_str(k).ok())
                    })
                    .collect();

                json!({
                    "signature": ix["signature"],
                    "rust": outcome(anchor_parity::decode_instruction(&program, &keys, &data)),
                    "js": outcome(anchor_parity::decode_instruction_js(&program, &keys, &data)),
                })
            })
            .collect();

        let decoded =
            json!({ "programId": program, "accounts": accounts, "instructions": instructions });

        fs::write(
            out_dir.join(path.file_name().expect("file name")),
            serde_json::to_vec_pretty(&decoded).expect("serialize"),
        )
        .expect("write decoded");
    }
}
