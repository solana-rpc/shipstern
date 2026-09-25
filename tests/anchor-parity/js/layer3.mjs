// Layer 3: do both converters accept and reject the same inputs?
//
//   node layer3.mjs <cache-dir>
//
// Synthetic edge cases only. JS side is rootNodeFromAnchor, what the CLI
// calls; Rust side is the `convert` binary. Writes <cache>/layer3.json.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { rootNodeFromAnchor } from '@codama/nodes-from-anchor';

const [cache] = process.argv.slice(2);
const dir = path.join(cache, 'layer3');
fs.rmSync(dir, { recursive: true, force: true });
fs.mkdirSync(path.join(dir, 'idls'), { recursive: true });

const base = extra => ({
    address: 'Dex1111111111111111111111111111111111111111',
    metadata: { name: 'edge', version: '0.1.0', spec: '0.1.0' },
    instructions: [],
    ...extra,
});
const struct = fields => ({ kind: 'struct', fields });
const ix = (args, accounts = []) => ({ instructions: [{ name: 'go', discriminator: [1], accounts, args }] });
const nest = (depth, leaf) => (depth === 0 ? leaf : { vec: nest(depth - 1, leaf) });

// [name, idl (object, or raw text for malformed JSON), note]
const cases = [
    ['baseline', base({}), 'minimal valid IDL'],
    ['spec-missing', { ...base({}), metadata: { name: 'edge', version: '0.1.0' } }, 'JS falls back to legacy'],
    ['spec-0.0.0', { ...base({}), metadata: { name: 'edge', version: '0.1.0', spec: '0.0.0' } }, 'JS falls back to legacy'],
    ['legacy-idl', { version: '0.1.0', name: 'legacy', instructions: [{ name: 'go', accounts: [], args: [] }] }, 'pre-0.30 format'],
    ['empty-enum', base({ types: [{ name: 'E', type: { kind: 'enum', variants: [] } }] }), ''],
    ['fields-null', base({ types: [{ name: 'S', type: { kind: 'struct', fields: null } }] }), ''],
    ['fields-mixed', base({ types: [{ name: 'S', type: struct([{ name: 'a', type: 'u8' }, 'u8']) }] }), 'named and tuple fields'],
    ['generics-empty-on-plain', base({ types: [{ name: 'P', type: struct([]) }, { name: 'U', type: struct([{ name: 'p', type: { defined: { name: 'P', generics: [] } } }]) }] }), ''],
    ['string-docs', base({ types: [{ name: 'S', docs: 'one line', type: struct([{ name: 'x', docs: 'field', type: 'u8' }]) }] }), ''],
    ['self-referential-generic', base({ types: [
        { name: 'W', generics: [{ kind: 'type', name: 'T' }], type: struct([{ name: 'i', type: { generic: 'T' } }]) },
        { name: 'O', generics: [{ kind: 'type', name: 'T' }], type: struct([{ name: 'w', type: { defined: { name: 'W', generics: [{ kind: 'type', type: { generic: 'T' } }] } } }]) },
        { name: 'U', type: struct([{ name: 'o', type: { defined: { name: 'O', generics: [{ kind: 'type', type: 'u8' }] } } }]) },
    ] }), 'JS overflows its stack'],
    ['nesting-100', base({ types: [{ name: 'S', type: struct([{ name: 'deep', type: nest(100, 'u8') }]) }] }), 'legal, non-recursive'],
    ['nesting-200', base({ types: [{ name: 'S', type: struct([{ name: 'deep', type: nest(200, 'u8') }]) }] }), 'legal, non-recursive'],
    ['nesting-2000', base({ types: [{ name: 'S', type: struct([{ name: 'deep', type: nest(2000, 'u8') }]) }] }), 'legal, non-recursive'],
    ['discriminator-missing', base({ instructions: [{ name: 'go', accounts: [], args: [] }] }), ''],
    ['discriminator-byte-300', base({ instructions: [{ name: 'go', discriminator: [300], accounts: [], args: [] }] }), ''],
    ['discriminator-empty', base({ instructions: [{ name: 'go', discriminator: [], accounts: [], args: [] }] }), ''],
    ['error-without-code', base({ errors: [{ name: 'E', msg: 'm' }] }), ''],
    ['error-without-msg', base({ errors: [{ code: 6000, name: 'E' }] }), ''],
    ['address-missing', (() => { const i = base({}); delete i.address; return i; })(), ''],
    ['name-missing', { ...base({}), metadata: { version: '0.1.0', spec: '0.1.0' } }, ''],
    ['instructions-missing', (() => { const i = base({}); delete i.instructions; return i; })(), ''],
    ['unknown-leaf-type', base(ix([{ name: 'a', type: 'u256' }])), ''],
    ['typedef-kind-type', base({ types: [{ name: 'T', type: { kind: 'type', alias: { array: ['u64', 8] } } }] }), 'as in Raydium CLMM'],
    ['account-type-missing', base({ accounts: [{ name: 'A', discriminator: [1] }] }), ''],
    ['account-type-not-struct', base({ accounts: [{ name: 'A', discriminator: [1] }], types: [{ name: 'A', type: { kind: 'alias', value: 'u64' } }] }), ''],
    ['generic-arg-unbound', base({ types: [{ name: 'S', type: struct([{ name: 'x', type: { generic: 'T' } }]) }] }), ''],
    ['seed-arg-missing', base(ix([], [{ name: 'v', pda: { seeds: [{ kind: 'arg', path: 'ghost' }] } }])), ''],
    ['seed-kind-unknown', base(ix([], [{ name: 'v', pda: { seeds: [{ kind: 'nonsense' }] } }])), ''],
    ['seed-kind-unknown-nested', base(ix([], [{ name: 'v', pda: { seeds: [{ kind: 'nonsense' }, { kind: 'account', path: 'a.b' }] } }])), 'nested path skips the PDA'],
    ['flatten-conflict', base({ instructions: [{ name: 'go', discriminator: [1], accounts: [], args: [{ name: 'p', type: { defined: { name: 'P' } } }] }], types: [{ name: 'P', type: struct([{ name: 'discriminator', type: 'u8' }]) }] }), ''],
    ['constant-u64-max', base({ constants: [{ name: 'C', type: 'u64', value: '18446744073709551615' }] }), 'accepted by both; value differs (documented)'],
    ['malformed-json', '{"address": "x", "metadata": ', ''],
];

// Documented in the crate README; anything else disagreeing is a regression.
const EXPECTED_DISAGREEMENTS = new Set([
    'spec-missing', 'spec-0.0.0', 'legacy-idl', // v01 only, by design
    'nesting-200', // NESTING_LIMIT
    'discriminator-byte-300', 'error-without-code', 'address-missing',
]);

const outcomes = new Map();

for (const [name, idl] of cases) {
    const text = typeof idl === 'string' ? idl : JSON.stringify(idl);
    fs.writeFileSync(path.join(dir, 'idls', `${name}.json`), text);

    let js;
    try {
        rootNodeFromAnchor(JSON.parse(text));
        js = { accepted: true };
    } catch (e) {
        js = { accepted: false, error: String(e?.message ?? e).slice(0, 140) };
    }
    outcomes.set(name, { js });
}

execFileSync('cargo', ['run', '--release', '-q', '--bin', 'convert', '--', path.join(dir, 'idls'), path.join(dir, 'rust')], { stdio: 'inherit' });

const results = cases.map(([name, , note]) => {
    const errFile = path.join(dir, 'rust', `${name}.error.txt`);
    const rust = fs.existsSync(errFile)
        ? { accepted: false, error: fs.readFileSync(errFile, 'utf8').slice(0, 140) }
        : { accepted: true };
    const { js } = outcomes.get(name);
    return { name, note, js, rust, agree: js.accepted === rust.accepted };
});

fs.writeFileSync(path.join(cache, 'layer3.json'), JSON.stringify(results, null, 2) + '\n');

console.log(`layer3: ${results.length} cases, ${results.filter(r => r.agree).length} agree, ${results.filter(r => !r.agree).length} DISAGREE`);
for (const r of results) {
    const fmt = o => (o.accepted ? 'accept' : `reject (${o.error})`);
    console.log(`  ${r.agree ? '  ' : '!!'} ${r.name.padEnd(26)} JS ${fmt(r.js).slice(0, 70).padEnd(72)} Rust ${fmt(r.rust).slice(0, 70)}`);
}

const unexpected = results.filter(r => r.agree === EXPECTED_DISAGREEMENTS.has(r.name));
for (const r of unexpected) console.log(`  unexpected: ${r.name} ${r.agree ? 'now agrees' : 'disagrees'}`);
if (unexpected.length) process.exitCode = 1;
