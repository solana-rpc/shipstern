// Fetch each program's IDL from mainnet: the classic Anchor IDL account first,
// then Program Metadata. Writes idls/<programId>.json
// and a manifest line per program. RPC comes from PARITY_RPC_URL.
//
//   node fetch-idls.mjs programs.txt <cache-dir>
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { address, createAddressWithSeed, createSolanaRpc, getProgramDerivedAddress, isAddress } from '@solana/kit';
import { fetchMetadataContent } from '@solana-program/program-metadata';

const RPC = process.env.PARITY_RPC_URL;
if (!RPC) throw new Error('PARITY_RPC_URL is not set');

const [listFile, outDir] = process.argv.slice(2);
fs.mkdirSync(path.join(outDir, 'idls'), { recursive: true });

const rpc = async (method, params) => {
    for (let attempt = 0; attempt < 4; attempt++) {
        const res = await fetch(RPC, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
        });
        if (res.status === 429) { await new Promise(r => setTimeout(r, 1000 * (attempt + 1))); continue; }
        const body = await res.json();
        if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
        return body.result;
    }
    throw new Error(`${method}: rate limited`);
};

async function legacyIdl(programId) {
    const program = address(programId);
    const [base] = await getProgramDerivedAddress({ programAddress: program, seeds: [] });
    const idlAddress = await createAddressWithSeed({ baseAddress: base, programAddress: program, seed: 'anchor:idl' });
    const info = await rpc('getAccountInfo', [idlAddress, { encoding: 'base64' }]);
    if (!info?.value) return null;
    const data = Buffer.from(info.value.data[0], 'base64');
    // 8-byte discriminator, 32-byte authority, u32 length, zlib payload.
    const len = data.readUInt32LE(40);
    return JSON.parse(zlib.inflateSync(data.subarray(44, 44 + len)).toString('utf8'));
}

// The canonical `idl` entry in Program Metadata, which is what `anchor idl fetch`
// (Anchor 1.x) reads. Content is parsed here because entries stored without a
// declared JSON format come back as a string.
const metadataRpc = createSolanaRpc(RPC);
async function metadataIdl(programId) {
    try {
        return JSON.parse(await fetchMetadataContent(metadataRpc, address(programId), 'idl'));
    } catch {
        return null;
    }
}

const ids = fs.readFileSync(listFile, 'utf8').split('\n').map(l => l.split('#')[0].trim()).filter(Boolean);

// Only executable accounts are programs; everything else is dropped up front.
const executable = new Set();
const valid = ids.filter(id => isAddress(id));
async function markExecutable(chunk) {
    try {
        const res = await rpc('getMultipleAccounts', [chunk, { encoding: 'base64', dataSlice: { offset: 0, length: 0 } }]);
        res.value.forEach((acct, j) => { if (acct?.executable) executable.add(chunk[j]); });
    } catch (e) {
        // Split rather than drop, so one bad key cannot hide a whole chunk.
        if (chunk.length === 1) { console.error(`skipped ${chunk[0]}: ${e.message}`); return; }
        const mid = chunk.length >> 1;
        await markExecutable(chunk.slice(0, mid));
        await markExecutable(chunk.slice(mid));
    }
}
for (let i = 0; i < valid.length; i += 100) await markExecutable(valid.slice(i, i + 100));
console.error(`${ids.length - valid.length} candidates are not valid addresses`);
console.error(`${executable.size} of ${ids.length} candidates are executable`);
const manifest = [];

for (const programId of ids.filter(id => executable.has(id))) {
    let idl = null;
    let source = null;
    let error = null;
    try {
        idl = await legacyIdl(programId);
        source = idl && 'anchor-idl-account';
        if (!idl) { idl = await metadataIdl(programId); source = idl && 'program-metadata'; }
    } catch (e) { error = String(e.message ?? e).slice(0, 200); }

    const spec = idl?.metadata?.spec ?? null;
    const entry = {
        programId,
        source: source ?? 'none',
        spec: spec ?? (idl ? 'legacy' : null),
        name: idl?.metadata?.name ?? idl?.name ?? null,
        instructions: idl?.instructions?.length ?? 0,
        accounts: idl?.accounts?.length ?? 0,
        addressMatches: idl ? (idl.address ?? idl.metadata?.address ?? null) === programId : null,
        error,
    };
    if (idl) fs.writeFileSync(path.join(outDir, 'idls', `${programId}.json`), JSON.stringify(idl, null, 2) + '\n');
    manifest.push(entry);
    console.log(JSON.stringify(entry));
}

fs.writeFileSync(path.join(outDir, 'fetch-manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
