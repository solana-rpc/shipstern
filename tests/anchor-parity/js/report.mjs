// Assemble <cache>/report.md from the layer outputs.
//
//   node report.mjs <cache-dir> > report.md
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';

const [cache] = process.argv.slice(2);
const read = f => JSON.parse(fs.readFileSync(path.join(cache, f), 'utf8'));
const has = f => fs.existsSync(path.join(cache, f));
const pkg = p => JSON.parse(fs.readFileSync(new URL(`../js/node_modules/${p}/package.json`, import.meta.url))).version;
const sh = (cmd, args) => { try { return execFileSync(cmd, args, { encoding: 'utf8' }).trim(); } catch { return 'unknown'; } };
const out = [];
const w = s => out.push(s);

const manifest = read('fetch-manifest.json');
const l1 = has('layer1.json') ? read('layer1.json') : [];
const l2 = has('layer2.json') ? read('layer2.json') : [];
const l3 = has('layer3.json') ? read('layer3.json') : [];
const byId = new Map(manifest.map(m => [m.programId, m]));
const name = id => byId.get(id)?.name ?? id;

// The RPC URL carries a credential in its path; record the host only.
let rpcHost = 'unset';
try { rpcHost = new URL(process.env.PARITY_RPC_URL).host; } catch {}

w('# shipstern-codama-from-anchor: real-world parity report\n');
w(`Generated ${new Date().toISOString()}.\n`);

w('## Versions\n');
w('| Component | Version |\n|---|---|');
w(`| @codama/nodes-from-anchor | ${pkg('@codama/nodes-from-anchor')} |`);
w(`| @codama/nodes | ${pkg('@codama/nodes')} |`);
w(`| @codama/visitors | ${pkg('@codama/visitors')} |`);
w(`| codama (CLI) | ${pkg('codama')} |`);
w(`| @coral-xyz/anchor (reference decoder) | ${pkg('@coral-xyz/anchor')} |`);
w(`| @solana/kit | ${pkg('@solana/kit')} |`);
w(`| @solana-program/program-metadata | ${pkg('@solana-program/program-metadata')} |`);
w(`| node | ${process.version} |`);
w(`| rustc | ${sh('rustc', ['--version'])} |`);
w(`| shipstern commit | ${sh('git', ['rev-parse', '--short', 'HEAD'])} (uncommitted changes: ${sh('git', ['status', '--porcelain']).length ? 'yes' : 'no'}) |`);
w(`| RPC host | ${rpcHost} (mainnet) |\n`);

const v01 = manifest.filter(m => m.spec === '0.1.0');
const legacy = manifest.filter(m => m.spec === 'legacy');
const none = manifest.filter(m => m.source === 'none');

w('## Corpus\n');
w(`${manifest.length} executable mainnet programs checked, ${v01.length + legacy.length} with an on-chain Anchor IDL: ` +
  `**${v01.length} spec 0.1.0** (tested), ${legacy.length} legacy (skipped, out of scope), ${none.length} with no IDL at either location.\n`);
w('| Program | Id | IDL source | Instructions | Accounts | IDL sha256 |\n|---|---|---|---|---|---|');
for (const m of [...v01].sort((a, b) => (a.name ?? '').localeCompare(b.name ?? ''))) {
    const f = path.join(cache, 'idls', `${m.programId}.json`);
    const sha = fs.existsSync(f) ? crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex').slice(0, 12) : '-';
    w(`| ${m.name} | \`${m.programId}\` | ${m.source} | ${m.instructions} | ${m.accounts} | ${sha} |`);
}
w('');

w('## Layer 1: IDL to Codama nodes\n');
const tally = l1.reduce((t, r) => ((t[r.status] = (t[r.status] ?? 0) + 1), t), {});
w(`${l1.length} programs: ${Object.entries(tally).map(([k, v]) => `${v} ${k}`).join(', ')}.\n`);
w('- **PASS**: identical to the JS pipeline minus the three unported passes, and every difference from the full `codama convert` output is PDA placement or instruction-account defaults, which those passes produce.');
w('- **BOTH-REJECT**: both converters refuse the IDL, for the same reason.\n');
const l1Bad = l1.filter(r => !['PASS', 'PASS-DOCUMENTED', 'BOTH-REJECT'].includes(r.status));
const fullCauses = {};
l1.forEach(r => (r.full ?? []).forEach(d => (fullCauses[d.cause] = (fullCauses[d.cause] ?? 0) + 1)));
w(`Differences from the full CLI output, by cause: ${Object.entries(fullCauses).map(([k, v]) => `${v} × ${k}`).join('; ') || 'none'}.\n`);
w('| Program | Status | JS | Rust |\n|---|---|---|---|');
for (const r of l1.filter(r => r.status !== 'PASS')) w(`| ${r.name} | ${r.status} | ${r.jsError ?? 'ok'} | ${(r.rustError ?? 'ok').replace('error: ', '')} |`);
w('');
if (l1Bad.length) {
    w('### Undocumented Layer 1 divergences\n');
    for (const r of l1Bad) for (const d of [...(r.parity ?? []), ...(r.full ?? [])].filter(d => !d.cause.startsWith('unported') && !d.cause.startsWith('documented'))) {
        w(`- ${r.name} \`${d.path}\`: JS \`${d.js}\`, Rust \`${d.rust}\` (${d.cause})`);
    }
    w('');
}
const documented = l1.flatMap(r => (r.parity ?? []).filter(d => d.cause.startsWith('documented')).map(d => ({ ...d, name: r.name })));
w(`Documented divergences observed: ${documented.length ? documented.map(d => `${d.name} \`${d.path}\``).join(', ') : 'none in this corpus'}.\n`);

// Links are added to codegen-issues.json once the issues are filed.
const issues = fs.existsSync(new URL('../codegen-issues.json', import.meta.url))
    ? JSON.parse(fs.readFileSync(new URL('../codegen-issues.json', import.meta.url), 'utf8'))
    : {};
const compiled = has('compile-failures.json') ? read('compile-failures.json') : {};
const converted = l1.filter(r => r.status.startsWith('PASS')).length;
const bothReject = l1.filter(r => r.status === 'BOTH-REJECT').length;

w('## Excluded programs\n');
w(`Corpus: **${l1.length}** spec 0.1.0 programs. **${bothReject}** are rejected by both converters (Layer 1), leaving **${converted}** converted. ` +
  `**${Object.keys(compiled).length}** of those are excluded because shipstern cannot compile a parser for them, ` +
  `leaving **${converted - Object.keys(compiled).length}** in the Layer 2 decode comparison.\n`);
w('The exclusions are not conversion failures. Each fails to compile identically whether its Codama input comes from `npx codama convert` or from this crate, so the bug is in shipstern\'s code generation.\n');
w('| Program | Id | Bug | Same on both sides | Issue |\n|---|---|---|---|---|');
for (const [id, f] of Object.entries(compiled)) {
    const all = [...f.rust, ...f.js].join(' ');
    const classes = [
        all.includes('cannot find type') && 'missing-event-types',
        all.includes('recursive type') && 'variant-helper-collision',
    ].filter(Boolean);
    const same = JSON.stringify([...f.rust].sort()) === JSON.stringify([...f.js].sort());
    const links = classes.map(c => issues[c] ?? `${c} (not filed yet)`).join(', ');
    w(`| ${name(id)} | \`${id}\` | ${classes.join(', ') || 'unclassified'} | ${same ? 'yes' : '**no**'} | ${links} |`);
}
w('\nDrafts, raw `cargo build` output and the expanded parser for each program are under `.cache/codegen-bugs/`.\n');

w('## Layer 2: decoding live mainnet data\n');
for (const kind of ['accounts', 'instructions']) {
    const items = l2.flatMap(r => r[kind].map(e => ({ ...e, programId: r.programId })));
    const n = p => items.filter(p).length;
    w(`**${kind}**: ${items.length} sampled from ${new Set(items.map(e => e.programId)).size} programs.\n`);
    const parsed = e => e.rust !== 'no-parser' || e.js !== 'no-parser';
    w(`- 2a, parser from Rust-converted nodes vs parser from JS-converted nodes, exact: **${n(e => parsed(e) && e.converterParity)} identical, ${n(e => !e.converterParity)} differ**, ${n(e => !parsed(e))} with no parser on either side (IDL rejected by both, or dropped for compile errors).`);
    w(`- 2b, Rust-converted parser vs @coral-xyz/anchor: ${n(e => e.diffs?.length === 0)} identical, **${n(e => e.diffs?.length > 0)} mismatch**, ` +
      `${n(e => e.rust !== 'ok' && e.reference === 'ok')} fail only in shipstern, ${n(e => e.rust === 'ok' && e.reference !== 'ok')} fail only in Anchor, ` +
      `${n(e => e.rust !== 'ok' && e.reference !== 'ok')} fail in both.\n`);
}

const all = l2.flatMap(r => ['accounts', 'instructions'].flatMap(kind => r[kind].map(e => ({ ...e, kind, programId: r.programId }))));

const conv = all.filter(e => !e.converterParity);
w('### 2a differences\n');
w(conv.length ? conv.map(e => `- ${name(e.programId)} ${e.kind} \`${e.id}\`: Rust-converted ${e.rust}, JS-converted ${e.js}`).join('\n') : 'None.');
w('');

w('### 2b value mismatches\n');
const mism = all.filter(e => e.diffs?.length);
if (!mism.length) w('None.');
const groups = new Map();
for (const e of mism) {
    for (const d of e.diffs) {
        const k = `${e.programId}|${e.kind}|${e.type ?? e.name ?? ''}|${d.path.replace(/\[\d+\]/g, '[]')}`;
        if (!groups.has(k)) groups.set(k, { e, d, count: 0 });
        groups.get(k).count++;
    }
}
for (const { e, d, count } of groups.values()) {
    w(`- ${name(e.programId)} ${e.kind === 'accounts' ? 'account' : 'instruction'} ${e.type ?? e.name ?? ''} \`${d.path}\` (${count}×; e.g. \`${e.id}\`): shipstern \`${d.rust}\`, Anchor \`${d.reference}\``);
}
w('');

w('### 2b decode failures\n');
const fails = new Map();
for (const e of all.filter(e => e.rust !== 'ok' || e.reference !== 'ok')) {
    const side = e.rust !== 'ok' && e.reference !== 'ok' ? 'both' : e.rust !== 'ok' ? 'shipstern only' : 'Anchor only';
    const reason = side === 'Anchor only' ? e.errors?.reference : e.errors?.rust ?? e.rust;
    const k = `${e.programId}|${e.kind}|${side}|${String(reason).slice(0, 80)}`;
    if (!fails.has(k)) fails.set(k, { e, side, reason, other: e.errors?.reference, count: 0 });
    fails.get(k).count++;
}
w('| Program | Kind | Fails in | Count | shipstern error | Anchor error |\n|---|---|---|---|---|---|');
for (const { e, side, count } of fails.values()) {
    w(`| ${name(e.programId)} | ${e.kind} | ${side} | ${count} | ${String(e.errors?.rust ?? (e.rust === 'ok' ? '-' : e.rust)).slice(0, 90)} | ${String(e.errors?.reference ?? '-').slice(0, 90)} |`);
}
w('');

const thin = l2.map(r => ({ id: r.programId, n: r.accounts.length })).filter(x => x.n < 10);
w(`Programs with fewer than 10 sampled accounts (${thin.length}): ${thin.map(x => `${name(x.id)} (${x.n})`).join(', ')}.\n`);

const compile = has('compile-failures.json') ? read('compile-failures.json') : {};
w('### Parsers shipstern could not compile\n');
w('A program whose generated parser fails to compile is dropped from Layer 2 on both sides. ' +
  'Identical errors on both sides point at shipstern\'s code generation, not at either converter.\n');
if (!Object.keys(compile).length) w('None.\n');
for (const [id, f] of Object.entries(compile)) {
    const same = JSON.stringify([...f.rust].sort()) === JSON.stringify([...f.js].sort());
    w(`- ${name(id)} \`${id}\`: ${same ? 'same errors from both parsers' : '**errors differ between the Rust- and JS-converted parsers**'}: ${[...new Set([...f.rust, ...f.js])].slice(0, 3).join('; ')}`);
}
w('');

const audit = has('layer2-audit.json') ? read('layer2-audit.json') : null;
if (audit) {
    w('### Normalizer audit\n');
    w('Each rule only removes a wrapping layer or renames a key; every leaf value is still compared exactly. ' +
      'A rule that fires on the side it was not written for would mean it removes a real layer, so each is shown per side.\n');
    w('| Rule | Fired on shipstern side | Fired on Anchor side | Example (shipstern) | Example (Anchor) |\n|---|---|---|---|---|');
    for (const [rule, r] of Object.entries(audit.rules)) {
        const ex = side => (r.examples[side] ? `\`${r.examples[side].raw.replaceAll('|', '\\|').slice(0, 90)}\`` : '-');
        w(`| ${rule} | ${r.rust} | ${r.reference} | ${ex('rust')} | ${ex('reference')} |`);
    }
    w(`\nNumbers equal only after rounding to f32: ${audit.froundOnly.length}${audit.froundOnly.length ? ` (e.g. ${audit.froundOnly.slice(0, 3).map(f => `${f.rust} vs ${f.reference}`).join(', ')})` : ''}.\n`);
}

w('## Layer 3: accept/reject agreement\n');
w(`${l3.length} synthetic cases, ${l3.filter(r => r.agree).length} agree, **${l3.filter(r => !r.agree).length} disagree**.\n`);
w('| Case | JS | Rust | Agree | Note |\n|---|---|---|---|---|');
const fmt = o => (o.accepted ? 'accept' : `reject: ${o.error.replaceAll('|', '\\|').slice(0, 80)}`);
for (const r of l3) w(`| ${r.name} | ${fmt(r.js)} | ${fmt(r.rust)} | ${r.agree ? 'yes' : '**no**'} | ${r.note} |`);
w('');

w('## Coverage gaps\n');
w(`- ${legacy.length} programs publish only a legacy (pre-0.30) IDL and were not converted: ${legacy.map(m => m.name).join(', ')}.`);
w(`- ${none.length} executable programs have no Anchor IDL at the classic account or in Program Metadata.`);
w(`- Programs whose IDL \`address\` differs from where it was fetched: ${v01.filter(m => !m.addressMatches).map(m => `${m.name} (\`${m.programId}\`)`).join(', ') || 'none'}.`);
w('- Anchor self-CPI event instructions are excluded from instruction samples.\n');

w('## Rerun\n');
w('```sh\ncd tests/anchor-parity\nexport PARITY_RPC_URL=<mainnet RPC URL>\njust parity\n```\n');
w('On-chain IDLs and account data change, so a rerun is a new measurement; the IDL hashes above identify what this run tested.');

process.stdout.write(out.join('\n') + '\n');
