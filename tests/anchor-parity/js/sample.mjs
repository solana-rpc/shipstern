// Layer 2 input: live accounts and instruction data per program, taken from
// recent successful mainnet transactions.
//
//   node sample.mjs <cache-dir> [max-accounts=20] [max-instructions=20]
//
// Writes <cache>/samples/<id>.json. RPC comes from PARITY_RPC_URL.
import fs from 'node:fs';
import path from 'node:path';
import { getBase58Encoder } from '@solana/kit';

const RPC = process.env.PARITY_RPC_URL;
if (!RPC) throw new Error('PARITY_RPC_URL is not set');

const [cache, maxAccounts = '20', maxIxs = '20'] = process.argv.slice(2);
const MAX_ACCOUNTS = Number(maxAccounts);
const MAX_IXS = Number(maxIxs);
const PER_TYPE = 5;
const EVENT_TAG = Buffer.from('e445a52e51cb9a1d', 'hex');
const b58 = getBase58Encoder();

const rpc = async (method, params) => {
    for (let attempt = 0; attempt < 5; attempt++) {
        const res = await fetch(RPC, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
        });
        if (res.status === 429 || res.status >= 500) { await new Promise(r => setTimeout(r, 500 * 2 ** attempt)); continue; }
        const body = await res.json();
        if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error).slice(0, 200)}`);
        return body.result;
    }
    throw new Error(`${method}: gave up after retries`);
};

const pool = async (items, n, fn) => {
    const out = [];
    let next = 0;
    await Promise.all(Array.from({ length: n }, async () => {
        while (next < items.length) { const i = next++; out[i] = await fn(items[i]).catch(e => ({ error: String(e.message ?? e) })); }
    }));
    return out;
};

fs.mkdirSync(path.join(cache, 'samples'), { recursive: true });

for (const file of fs.readdirSync(path.join(cache, 'idls')).filter(f => f.endsWith('.json')).sort()) {
    const programId = file.slice(0, -5);
    const idl = JSON.parse(fs.readFileSync(path.join(cache, 'idls', file), 'utf8'));
    if (idl?.metadata?.spec !== '0.1.0') continue;

    const discriminators = (idl.accounts ?? []).map(a => ({ name: a.name, bytes: Buffer.from(a.discriminator) }));

    const sigs = (await rpc('getSignaturesForAddress', [programId, { limit: 100 }]))
        .filter(s => !s.err)
        .slice(0, 40)
        .map(s => s.signature);

    const txs = await pool(sigs, 8, sig =>
        rpc('getTransaction', [sig, { encoding: 'json', maxSupportedTransactionVersion: 1, commitment: 'finalized' }]));

    const instructions = [];
    const touched = new Set();

    for (const [i, tx] of txs.entries()) {
        if (!tx?.transaction) continue;
        const keys = [
            ...tx.transaction.message.accountKeys,
            ...(tx.meta?.loadedAddresses?.writable ?? []),
            ...(tx.meta?.loadedAddresses?.readonly ?? []),
        ];
        keys.forEach(k => touched.add(k));

        const invoked = [
            ...tx.transaction.message.instructions,
            ...(tx.meta?.innerInstructions ?? []).flatMap(ii => ii.instructions),
        ];
        for (const ix of invoked) {
            if (instructions.length >= MAX_IXS) break;
            if (keys[ix.programIdIndex] !== programId) continue;
            const data = Buffer.from(b58.encode(ix.data));
            if (data.subarray(0, 8).equals(EVENT_TAG)) continue;
            instructions.push({ signature: sigs[i], data: data.toString('base64'), accounts: ix.accounts.map(a => keys[a]) });
        }
    }

    const candidates = [...touched];
    const accounts = [];
    const perType = new Map();

    for (let i = 0; i < candidates.length && accounts.length < MAX_ACCOUNTS; i += 100) {
        const chunk = candidates.slice(i, i + 100);
        const res = await rpc('getMultipleAccounts', [chunk, { encoding: 'base64' }]);
        res.value.forEach((acct, j) => {
            if (!acct || acct.owner !== programId || accounts.length >= MAX_ACCOUNTS) return;
            const data = Buffer.from(acct.data[0], 'base64');
            const match = discriminators.find(d => d.bytes.length && data.subarray(0, d.bytes.length).equals(d.bytes));
            if (!match || (perType.get(match.name) ?? 0) >= PER_TYPE) return;
            perType.set(match.name, (perType.get(match.name) ?? 0) + 1);
            accounts.push({ address: chunk[j], type: match.name, data: data.toString('base64') });
        });
    }

    fs.writeFileSync(path.join(cache, 'samples', `${programId}.json`),
        JSON.stringify({ programId, name: idl.metadata.name, transactions: sigs.length, accounts, instructions }, null, 1) + '\n');
    console.log(`${idl.metadata.name} ${programId}: ${accounts.length} accounts, ${instructions.length} instructions from ${sigs.length} txs`);
}
