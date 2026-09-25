// Write <cache>/generated.rs: one shipstern parser per Rust-converted IDL, plus
// dispatch functions the decode binary calls.
//
//   node gen-parsers.mjs <cache-dir> [skip-list-file] [--stub]
//
// --stub writes the dispatch functions with no parsers, so the crate builds
// before anything has been converted.
//
// The module and type names below mirror crates/proc-macro/src/utils.rs
// (to_snake_case, to_pascal_case); a mismatch fails to compile rather than
// decoding the wrong thing.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

const args = process.argv.slice(2);
const stub = args.includes('--stub');
const [cache, skipFile] = args.filter(a => a !== '--stub');
const skip = new Set(skipFile && fs.existsSync(skipFile) ? fs.readFileSync(skipFile, 'utf8').split(/\s+/).filter(Boolean) : []);

const isUpper = c => c !== c.toLowerCase() && c === c.toUpperCase();
const isLower = c => c !== c.toUpperCase() && c === c.toLowerCase();

function toSnakeCase(s) {
    const chars = [...s];
    let out = '';
    chars.forEach((c, i) => {
        if (isUpper(c) && i > 0) {
            const prevUpper = isUpper(chars[i - 1]);
            const nextLower = i + 1 < chars.length && isLower(chars[i + 1]);
            if (!prevUpper || nextLower) out += '_';
        }
        out += c.replace(/[A-Z]/, m => m.toLowerCase());
    });
    return out;
}

function toPascalCase(s) {
    let out = '';
    let cap = true;
    for (const c of s) {
        if (c === '_') cap = true;
        else if (cap) { out += c.replace(/[a-z]/, m => m.toUpperCase()); cap = false; }
        else out += c;
    }
    return out;
}

// One parser set per converter: `p_` from the Rust crate, `j_` from the JS CLI.
// `input(id, file)` names what the macro reads, relative to the cache. The Rust
// set gets the raw on-chain Anchor IDL, so it goes through the macro's own
// in-process conversion.
function generate(dirName, prefix, input) {
    const dir = path.join(cache, dirName);
    fs.mkdirSync(dir, { recursive: true });
    const modules = [];
    const ids = [];
    const accountArms = [];
    const ixArms = [];

    for (const file of stub ? [] : fs.readdirSync(dir).filter(f => f.endsWith('.codama.json')).sort()) {
        const id = file.slice(0, -'.codama.json'.length);
        if (skip.has(id)) continue;

        const text = fs.readFileSync(path.join(dir, file), 'utf8');
        const inputFile = path.join(cache, input(id, file));
        // include_shipstern_parser! resolves paths from the crate root, where `just` runs.
        const macroPath = path.relative(process.cwd(), inputFile);
        const program = JSON.parse(text).program;
        // Cargo does not track the JSON the macro reads; a changed hash forces a rebuild.
        const hash = crypto.createHash('sha256').update(fs.readFileSync(inputFile)).digest('hex').slice(0, 16);
        const modIdent = toSnakeCase(program.name);
        const pascal = toPascalCase(program.name);
        const hasDiscriminated = (program.accounts ?? []).some(a => (a.discriminators ?? []).length > 0);
        const clash = (program.accounts ?? []).some(a => toPascalCase(a.name) === `${pascal}Account`);
        const wrapper = clash ? `${pascal}AccountOutput` : `${pascal}Account`;
        const m = `${prefix}_${id}`;
        ids.push(id);

        modules.push(`// input ${hash}\n#[allow(clippy::all, dead_code, unused, non_snake_case)]\npub mod ${m} {\n    shipstern_proc_macro::include_shipstern_parser!("${macroPath}");\n}`);

        if (hasDiscriminated) {
            // The match must be exhaustive, so every account gets an arm.
            const arms = (program.accounts ?? [])
                .map(a => `                ${m}::${modIdent}::account::Account::${toPascalCase(a.name)}(v) => ("${a.name}", serde_json::to_value(&v)),`)
                .join('\n');
            accountArms.push(`        "${id}" => Some(${m}::${modIdent}::${wrapper}::try_unpack(data).map_err(|e| e.to_string()).and_then(|w| {
            let (name, value) = match w.account {
${arms}
            };
            value.map(|v| (name.to_owned(), v)).map_err(|e| e.to_string())
        })),`);
        }

        if ((program.instructions ?? []).length > 0) {
            ixArms.push(`        "${id}" => Some(${m}::${modIdent}::resolve_instruction_default(accounts, data, &path)
            .map_err(|e| e.to_string())
            .and_then(|v| serde_json::to_value(&v).map_err(|e| e.to_string()))),`);
        }
    }

    return { modules, accountArms, ixArms, ids };
}

const dispatch = (suffix, set) => `/// \`None\` when the program has no parser or no discriminated accounts.
// A stub has no arms, which leaves the match trivial and data unused.
#[allow(unused_variables, clippy::match_single_binding)]
pub fn decode_account${suffix}(program: &str, data: &[u8]) -> Option<Result<(String, serde_json::Value), String>> {
    match program {
${set.accountArms.join('\n')}
        _ => None,
    }
}

/// \`None\` when the program has no parser or declares no instructions.
// A stub has no arms, which leaves the match trivial and data unused.
#[allow(unused_variables, clippy::match_single_binding)]
pub fn decode_instruction${suffix}(
    program: &str,
    accounts: &[shipstern_core::Pubkey],
    data: &[u8],
) -> Option<Result<serde_json::Value, String>> {
    let path = shipstern_core::instruction::Path::new_single(0);
    let _ = (&path, accounts, data);

    match program {
${set.ixArms.join('\n')}
        _ => None,
    }
}`;

const rust = generate('rust', 'p', id => `idls/${id}.json`);
const js = generate('jsnodes', 'j', (_id, file) => `jsnodes/${file}`);

const out = `// @generated by js/gen-parsers.mjs. Do not edit.

${[...rust.modules, ...js.modules].join('\n\n')}

${dispatch('', rust)}

${dispatch('_js', js)}

/// Programs with a parser in this build; the decoder skips every other sample.
pub const PROGRAMS: &[&str] = &[${[...new Set([...rust.ids, ...js.ids])].map(id => `"${id}"`).join(', ')}];
`;

fs.writeFileSync(path.join(cache, 'generated.rs'), out);
console.log(`generated ${rust.modules.length} Rust-converted and ${js.modules.length} JS-converted parsers`);
