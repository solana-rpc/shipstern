// List every program invoked (outer or inner) in a sample of recent mainnet
// blocks, most-invoked first. Writes one program id per line.
//
//   node discover-programs.mjs <blocks> <stride> out.txt
import fs from 'node:fs';

const RPC = process.env.PARITY_RPC_URL;
if (!RPC) throw new Error('PARITY_RPC_URL is not set');

const [blocks = '40', stride = '750', out = 'discovered.txt'] = process.argv.slice(2);

const rpc = async (method, params) => {
    const res = await fetch(RPC, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
    });
    const body = await res.json();
    if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
    return body.result;
};

const tip = await rpc('getSlot', [{ commitment: 'finalized' }]);
const counts = new Map();
let scanned = 0;

for (let i = 0; i < Number(blocks); i++) {
    const slot = tip - 100 - i * Number(stride);
    let block;
    try {
        block = await rpc('getBlock', [slot, {
            encoding: 'json', transactionDetails: 'full', rewards: false, maxSupportedTransactionVersion: 1,
        }]);
    } catch { continue; } // skipped slot
    if (!block) continue;
    scanned++;

    for (const tx of block.transactions) {
        const keys = [
            ...tx.transaction.message.accountKeys,
            ...(tx.meta?.loadedAddresses?.writable ?? []),
            ...(tx.meta?.loadedAddresses?.readonly ?? []),
        ];
        const invoked = [
            ...tx.transaction.message.instructions,
            ...(tx.meta?.innerInstructions ?? []).flatMap(ii => ii.instructions),
        ];
        for (const ix of invoked) {
            const id = keys[ix.programIdIndex];
            if (id) counts.set(id, (counts.get(id) ?? 0) + 1);
        }
    }
}

const ranked = [...counts.entries()].sort((a, b) => b[1] - a[1]);
fs.writeFileSync(out, ranked.map(([id, n]) => `${id} ${n}`).join('\n') + '\n');
console.error(`${scanned} blocks scanned, ${ranked.length} distinct programs`);
