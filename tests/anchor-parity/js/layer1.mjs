// Layer 1: IDL -> Codama nodes, JS vs Rust, per program.
//
//   node layer1.mjs <cache-dir>
//
// Reads <cache>/idls/*.json (spec 0.1.0 only) and <cache>/rust/<id>.codama.json
// (from `cargo run --bin convert`). Compares the Rust output against:
//   parity - the JS pipeline without the three passes the crate does not port;
//            must match exactly apart from documented divergences.
//   full   - what `codama convert` produces; every difference must be
//            attributable to one of the three unported passes.
import fs from 'node:fs';
import path from 'node:path';

import { assertIsNode } from '@codama/nodes';
import { rootNodeFromAnchor, rootNodeFromAnchorWithoutDefaultVisitor } from '@codama/nodes-from-anchor';
import {
    flattenInstructionDataArgumentsVisitor,
    setFixedAccountSizesVisitor,
    transformU8ArraysToBytesVisitor,
    unwrapInstructionArgsDefinedTypesVisitor,
    visit,
} from '@codama/visitors';

const PORTED_PASSES = [
    setFixedAccountSizesVisitor,
    unwrapInstructionArgsDefinedTypesVisitor,
    flattenInstructionDataArgumentsVisitor,
    transformU8ArraysToBytesVisitor,
];

const cache = process.argv[2];
const MAX_SAFE = 2n ** 53n;

// Keep Rust numbers as their source text so u64 values are not rounded on read.
const parseExact = text =>
    JSON.parse(text, (_key, value, ctx) => (typeof value === 'number' ? { num: ctx.source } : value));

// JS-side values are real JS numbers; wrap them the same way.
const wrapNumbers = value => {
    if (typeof value === 'number') return { num: String(value) };
    if (Array.isArray(value)) return value.map(wrapNumbers);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, wrapNumbers(v)]));
    return value;
};

const isNum = v => v && typeof v === 'object' && Object.keys(v).length === 1 && 'num' in v;

// Numbers are equal when they denote the same value exactly.
const sameNumber = (a, b) => {
    const exact = s => (/^-?\d+$/.test(s) ? BigInt(s) : null);
    const [x, y] = [exact(a.num), exact(b.num)];
    if (x !== null && y !== null) return x === y;
    // JS writes a whole double past 2^53 in exponent form; compare as BigInt.
    const whole = s => { const n = Number(s); return Number.isInteger(n) ? BigInt(n) : null; };
    const [p, q] = [x ?? whole(a.num), y ?? whole(b.num)];
    if (p !== null && q !== null) return p === q;
    return Number(a.num) === Number(b.num);
};

function diff(p, js, rs, out) {
    if (isNum(js) && isNum(rs)) {
        if (!sameNumber(js, rs)) out.push({ path: p, js: js.num, rust: rs.num });
        return;
    }
    if (Array.isArray(js) && Array.isArray(rs)) {
        if (js.length !== rs.length) out.push({ path: p, js: `${js.length} items`, rust: `${rs.length} items` });
        for (let i = 0; i < Math.min(js.length, rs.length); i++) diff(`${p}[${i}]`, js[i], rs[i], out);
        return;
    }
    if (js && rs && typeof js === 'object' && typeof rs === 'object' && !isNum(js) && !isNum(rs)) {
        for (const k of new Set([...Object.keys(js), ...Object.keys(rs)])) {
            if (!(k in js)) out.push({ path: `${p}.${k}`, js: '<absent>', rust: JSON.stringify(unwrap(rs[k])).slice(0, 120) });
            else if (!(k in rs)) out.push({ path: `${p}.${k}`, js: JSON.stringify(unwrap(js[k])).slice(0, 120), rust: '<absent>' });
            else diff(`${p}.${k}`, js[k], rs[k], out);
        }
        return;
    }
    const [a, b] = [JSON.stringify(unwrap(js)), JSON.stringify(unwrap(rs))];
    if (a !== b) out.push({ path: p, js: a.slice(0, 120), rust: b.slice(0, 120) });
}

const unwrap = v => (isNum(v) ? v.num : Array.isArray(v) ? v.map(unwrap) : v && typeof v === 'object' ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, unwrap(x)])) : v);

// The only divergence the README accepts against the parity pipeline.
function classifyParity(d) {
    if (/\.constants\[\d+\]\.value\.number$/.test(d.path)) {
        const big = s => { try { const n = BigInt(s); return n > MAX_SAFE || n < -MAX_SAFE; } catch { return Math.abs(Number(s)) >= 2 ** 53; } };
        if (big(d.rust) || big(d.js)) return 'documented: constant above 2^53';
    }
    return 'UNEXPLAINED';
}

// Differences against the full CLI output that the unported passes explain.
function classifyFull(d) {
    if (d.path.startsWith('.program.pdas')) return 'unported: extractPdas';
    if (/\.instructions\[\d+\]\.accounts\[\d+\]\.defaultValue/.test(d.path)) return 'unported: extractPdas / setInstructionAccountDefaultValues';
    if (/^\.program\.definedTypes/.test(d.path)) return 'check: deduplicateIdenticalDefinedTypes?';
    return classifyParity(d);
}

const idlDir = path.join(cache, 'idls');
const results = [];

for (const file of fs.readdirSync(idlDir).filter(f => f.endsWith('.json')).sort()) {
    const id = file.slice(0, -5);
    const idl = JSON.parse(fs.readFileSync(path.join(idlDir, file), 'utf8'));
    if (idl?.metadata?.spec !== '0.1.0') continue;

    const r = { programId: id, name: idl.metadata.name };

    let jsFull, jsParity;
    try {
        jsFull = rootNodeFromAnchor(idl);
        fs.mkdirSync(path.join(cache, 'jsnodes'), { recursive: true });
        fs.writeFileSync(path.join(cache, 'jsnodes', `${id}.codama.json`), JSON.stringify(jsFull));
        let root = rootNodeFromAnchorWithoutDefaultVisitor(idl);
        for (const pass of PORTED_PASSES) { root = visit(root, pass()); assertIsNode(root, 'rootNode'); }
        jsParity = root;
    } catch (e) {
        r.jsError = String(e?.message ?? e).slice(0, 200);
    }

    const rustFile = path.join(cache, 'rust', `${id}.codama.json`);
    const rustErrFile = path.join(cache, 'rust', `${id}.error.txt`);
    const rust = fs.existsSync(rustFile) ? parseExact(fs.readFileSync(rustFile, 'utf8')) : null;
    if (!rust) r.rustError = fs.existsSync(rustErrFile) ? fs.readFileSync(rustErrFile, 'utf8') : 'no output';

    if (r.jsError || r.rustError) {
        r.status = r.jsError && r.rustError ? 'BOTH-REJECT' : 'ACCEPT-MISMATCH';
    } else {
        const parity = [];
        diff('', wrapNumbers(jsParity), rust, parity);
        const full = [];
        diff('', wrapNumbers(jsFull), rust, full);

        r.parity = parity.map(d => ({ ...d, cause: classifyParity(d) }));
        r.full = full.map(d => ({ ...d, cause: classifyFull(d) }));
        const bad = [...r.parity, ...r.full].filter(d => d.cause === 'UNEXPLAINED' || d.cause.startsWith('check'));
        r.status = bad.length ? 'FAIL' : r.parity.length ? 'PASS-DOCUMENTED' : 'PASS';
    }
    results.push(r);
}

fs.writeFileSync(path.join(cache, 'layer1.json'), JSON.stringify(results, null, 2) + '\n');

const tally = results.reduce((t, r) => ((t[r.status] = (t[r.status] ?? 0) + 1), t), {});
console.log(`layer1: ${results.length} programs`, tally);
for (const r of results.filter(r => r.status !== 'PASS')) {
    console.log(`  ${r.status} ${r.name} ${r.programId} ${r.jsError ?? ''} ${r.rustError ?? ''}`);
    for (const d of [...(r.parity ?? []), ...(r.full ?? []).filter(d => d.cause !== 'unported: extractPdas' && !d.cause.startsWith('unported'))].slice(0, 6)) {
        console.log(`      [${d.cause}] ${d.path}: JS ${d.js} | Rust ${d.rust}`);
    }
}

// Only a clean PASS or a shared rejection is acceptable.
if (results.some(r => !['PASS', 'PASS-DOCUMENTED', 'BOTH-REJECT'].includes(r.status))) process.exitCode = 1;
