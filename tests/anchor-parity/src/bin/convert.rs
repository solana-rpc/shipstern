//! Convert every `<id>.json` Anchor IDL in a directory with the Rust crate.
//!
//! Writes `<out>/<id>.codama.json` on success, or `<out>/<id>.error.txt`
//! holding `error: ...` or `panic: ...`.

use std::{fs, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let [input, output] = args.as_slice() else {
        eprintln!("usage: convert <idl-dir> <out-dir>");
        return ExitCode::FAILURE;
    };

    let (input, output) = (PathBuf::from(input), PathBuf::from(output));

    if let Err(err) = fs::create_dir_all(&output) {
        eprintln!("cannot create {}: {err}", output.display());
        return ExitCode::FAILURE;
    }

    let Ok(entries) = fs::read_dir(&input) else {
        eprintln!("cannot read {}", input.display());
        return ExitCode::FAILURE;
    };

    let (mut ok, mut failed) = (0, 0);

    for entry in entries.flatten() {
        let path = entry.path();

        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };

        let Ok(bytes) = fs::read(&path) else {
            continue;
        };

        let result =
            std::panic::catch_unwind(|| codama_nodes_from_anchor::root_node_from_anchor(&bytes));

        let outcome = match result {
            Ok(Ok(root)) => serde_json::to_vec_pretty(&root)
                .map(|json| (format!("{id}.codama.json"), json))
                .map_err(|err| format!("error: serialize: {err}")),
            Ok(Err(err)) => Err(format!("error: {err}")),
            Err(_) => Err("panic: conversion panicked".to_owned()),
        };

        let written = match outcome {
            Ok((name, json)) => {
                ok += 1;
                fs::write(output.join(name), json)
            },
            Err(message) => {
                failed += 1;
                fs::write(output.join(format!("{id}.error.txt")), message)
            },
        };

        if let Err(err) = written {
            eprintln!("{id}: cannot write output: {err}");
        }
    }

    eprintln!("converted {ok}, failed {failed}");

    ExitCode::SUCCESS
}
