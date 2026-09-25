// Layer 2: decode parity on live mainnet accounts and instruction data.
//
//   node layer2.mjs <cache-dir>
//
// 2a  converter parity: the parser generated from the Rust crate's nodes vs the
//     parser generated from the JS CLI's nodes, on the same bytes. Exact
//     equality, no normalization. This is the converter's own contract.
// 2b  reference parity: the Rust-converted parser vs @coral-xyz/anchor's
//     BorshCoder. This checks shipstern's decoding, so both sides are reduced
//     to one shape by symmetric, one-to-one rules:
//       - keys lowercased with `_` removed; tuple keys `item_N` read as `N`;
//         a key that then collides with an earlier one is numbered (`pad02`)
//       - an object whose only key is `kind` (shipstern's enum wrapper),
//         `items` (its nested-array wrapper) or `0` (Anchor's newtype tuple)
//         stands for its value
//       - an instruction struct argument that codama flattened into separate
//         arguments is compared field by field
//       - numbers by exact value; floats also equal after f32 rounding
//     Any other difference is a mismatch.
import fs from 'node:fs';
import path from 'node:path';

const [cache] = process.argv.slice(2);

const parseExact = text => JSON.parse(text, (_k, v, ctx) => (typeof v === 'number' ? { num: ctx.source } : v));
const isNum = v => v && typeof v === 'object' && !Array.isArray(v) && Object.keys(v).length === 1 && 'num' in v;
const isObj = v => v && typeof v === 'object' && !Array.isArray(v) && !isNum(v);
const normKey = k => { const n = k.toLowerCase().replaceAll('_', ''); const m = /^item(\d+)$/.exec(n); return m ? m[1] : n; };

// Reference values: plain numbers and {bn} become {num}.
function wrap(v) {
    if (typeof v === 'number') return { num: String(v) };
    if (Array.isArray(v)) return v.map(wrap);
    if (v && typeof v === 'object') {
        if (Object.keys(v).length === 1 && 'bn' in v) return { num: v.bn };
        return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, wrap(x)]));
    }
    return v;
}

// Every rule firing is recorded per side, with one raw example, for the audit.
const audit = { rules: {}, froundOnly: [] };
let context = '';
const hit = (rule, side, raw) => {
    const r = (audit.rules[rule] ??= { rust: 0, reference: 0, examples: {} });
    r[side]++;
    r.examples[side] ??= { at: context, raw: JSON.stringify(raw).slice(0, 160) };
};

function canon(v, side) {
    if (Array.isArray(v)) return v.map(x => canon(x, side));
    if (!isObj(v)) return v;
    const out = {};
    for (const [k, x] of Object.entries(v)) {
        let n = normKey(k);
        if (/^item\d+$/i.test(k.replaceAll('_', ''))) hit('item_N -> N', side, { [k]: '...' });
        // Two fields can normalize alike (`_pad0`, `_pad_0`). Keep both, and
        // number the later one the way shipstern disambiguates (`pad0_2`).
        if (n in out) {
            hit('colliding key numbered', side, { [k]: '...' });
            let i = 2;
            while (`${n}${i}` in out) i++;
            n = `${n}${i}`;
        }
        out[n] = canon(x, side);
    }
    const keys = Object.keys(out);
    if (keys.length === 1 && ['kind', 'items', '0'].includes(keys[0])) {
        hit(`unwrap {${keys[0]}}`, side, v);
        return out[keys[0]];
    }
    return out;
}

function sameNumber(a, b) {
    const int = s => (/^-?\d+$/.test(s) ? BigInt(s) : null);
    const [x, y] = [int(a), int(b)];
    if (x !== null && y !== null) return x === y;
    const [p, q] = [Number(a), Number(b)];
    if (p === q) return true;
    if (Math.fround(p) === Math.fround(q)) {
        audit.froundOnly.push({ at: context, rust: a, reference: b });
        return true;
    }
    return false;
}

function diff(p, a, b, out) {
    if (isNum(a) && isNum(b)) {
        if (!sameNumber(a.num, b.num)) out.push({ path: p, rust: a.num, reference: b.num });
        return;
    }
    if (Array.isArray(a) && Array.isArray(b)) {
        if (a.length !== b.length) out.push({ path: p, rust: `${a.length} items`, reference: `${b.length} items` });
        for (let i = 0; i < Math.min(a.length, b.length); i++) diff(`${p}[${i}]`, a[i], b[i], out);
        return;
    }
    if (isObj(a) && isObj(b)) {
        for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) {
            if (!(k in a)) out.push({ path: `${p}.${k}`, rust: '<absent>', reference: JSON.stringify(b[k])?.slice(0, 100) });
            else if (!(k in b)) out.push({ path: `${p}.${k}`, rust: JSON.stringify(a[k])?.slice(0, 100), reference: '<absent>' });
            else diff(`${p}.${k}`, a[k], b[k], out);
        }
        return;
    }
    if (JSON.stringify(a) !== JSON.stringify(b)) {
        out.push({ path: p, rust: JSON.stringify(a)?.slice(0, 100), reference: JSON.stringify(b)?.slice(0, 100) });
    }
}

// Codama flattens a struct argument into its fields; Anchor keeps the struct.
// Only splice when every field of that struct is present in the Rust args.
function flattenLike(rustArgs, refData) {
    const out = {};
    for (const [k, v] of Object.entries(refData)) {
        if (!(k in rustArgs) && isObj(v) && Object.keys(v).every(f => f in rustArgs)) {
            hit('flatten struct argument', 'reference', { [k]: v });
            Object.assign(out, v);
        } else {
            out[k] = v;
        }
    }
    return out;
}

const stable = v => JSON.stringify(v, (_k, x) => (isObj(x) ? Object.fromEntries(Object.entries(x).sort(([a], [b]) => (a < b ? -1 : 1))) : x));

const results = [];

for (const file of fs.readdirSync(path.join(cache, 'decoded')).sort()) {
    const decoded = parseExact(fs.readFileSync(path.join(cache, 'decoded', file), 'utf8'));
    const refFile = path.join(cache, 'reference', file);
    const ref = fs.existsSync(refFile) ? JSON.parse(fs.readFileSync(refFile, 'utf8')) : {};
    const r = { programId: decoded.programId, referenceError: ref.coderError, accounts: [], instructions: [] };

    for (const [kind, items] of [['accounts', decoded.accounts], ['instructions', decoded.instructions]]) {
        items.forEach((item, i) => {
            const refItem = ref[kind]?.[i]?.reference;
            const e = {
                id: item.address ?? item.signature,
                type: item.type,
                rust: item.rust.status,
                js: item.js.status,
                reference: refItem?.status ?? 'unavailable',
                // 2a: byte-for-byte agreement between the two generated parsers.
                converterParity: stable(item.rust) === stable(item.js),
            };

            context = `${decoded.programId} ${kind} ${item.address ?? item.signature}`;
            if (item.rust.status === 'ok' && refItem?.status === 'ok') {
                const d = [];
                if (kind === 'accounts') {
                    if (normKey(item.rust.value.type) !== normKey(item.type)) d.push({ path: '<type>', rust: item.rust.value.type, reference: item.type });
                    diff('', canon(item.rust.value.fields, 'rust'), canon(wrap(refItem.value), 'reference'), d);
                } else {
                    const [[variant, body]] = Object.entries(item.rust.value.instruction);
                    e.name = refItem.value.name;
                    if (normKey(variant) !== normKey(refItem.value.name)) d.push({ path: '<name>', rust: variant, reference: refItem.value.name });
                    const args = canon(body.args ?? {}, 'rust');
                    diff('', args, flattenLike(args, canon(wrap(refItem.value.data), 'reference')), d);
                }
                e.diffs = d;
            } else {
                e.errors = { rust: item.rust.error, reference: refItem?.error };
            }
            r[kind].push(e);
        });
    }
    results.push(r);
}

fs.writeFileSync(path.join(cache, 'layer2.json'), JSON.stringify(results, null, 2) + '\n');
fs.writeFileSync(path.join(cache, 'layer2-audit.json'), JSON.stringify(audit, null, 2) + '\n');

for (const kind of ['accounts', 'instructions']) {
    const items = results.flatMap(r => r[kind]);
    const n = pred => items.filter(pred).length;
    const parsed = e => e.rust !== 'no-parser' || e.js !== 'no-parser';
    console.log(`layer2 ${kind}: ${items.length} sampled | 2a converter parity: ${n(e => parsed(e) && e.converterParity)} identical, ${n(e => !e.converterParity)} DIFFER, ${n(e => !parsed(e))} no parser on either side` +
        ` | 2b vs anchor: ${n(e => e.diffs?.length === 0)} identical, ${n(e => e.diffs?.length > 0)} MISMATCH,` +
        ` ${n(e => e.rust !== 'ok' && e.reference === 'ok')} rust-fails-only, ${n(e => e.rust === 'ok' && e.reference !== 'ok')} anchor-fails-only,` +
        ` ${n(e => e.rust !== 'ok' && e.reference !== 'ok')} both-fail`);
}

console.log('normalizer audit (fires per side):');
for (const [rule, r] of Object.entries(audit.rules)) console.log(`  ${rule}: rust ${r.rust}, reference ${r.reference}`);
console.log(`  equal only after f32 rounding: ${audit.froundOnly.length}`);

// Both decoders failing means on-chain data outgrew its IDL; that is not a
// converter or decoder divergence, so it does not fail the run.
const items = results.flatMap(r => [...r.accounts, ...r.instructions]);
if (items.some(e => !e.converterParity || e.diffs?.length)) process.exitCode = 1;
