// Layer 2 reference: decode every sample with @coral-xyz/anchor's BorshCoder,
// built from the same on-chain IDL. Writes <cache>/reference/<id>.json.
//
//   node reference.mjs <cache-dir>
import fs from 'node:fs';
import path from 'node:path';
import anchor from '@coral-xyz/anchor';

const { BorshCoder, BN, web3 } = anchor;
const [cache] = process.argv.slice(2);

// BN, PublicKey and Buffer have no stable JSON form; flatten them.
const plain = v => {
    if (v === null || v === undefined) return null;
    if (BN.isBN(v)) return { bn: v.toString(10) };
    if (v instanceof web3.PublicKey) return v.toBase58();
    if (Buffer.isBuffer(v) || v instanceof Uint8Array) return [...v];
    if (typeof v === 'number') return Number.isNaN(v) ? { nan: true } : v;
    if (Array.isArray(v)) return v.map(plain);
    if (typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, plain(x)]));
    return v;
};

fs.mkdirSync(path.join(cache, 'reference'), { recursive: true });

for (const file of fs.readdirSync(path.join(cache, 'samples')).sort()) {
    const sample = JSON.parse(fs.readFileSync(path.join(cache, 'samples', file), 'utf8'));
    const idl = JSON.parse(fs.readFileSync(path.join(cache, 'idls', file), 'utf8'));

    let coder;
    try {
        coder = new BorshCoder(idl);
    } catch (e) {
        fs.writeFileSync(path.join(cache, 'reference', file), JSON.stringify({ coderError: String(e.message ?? e) }) + '\n');
        continue;
    }

    const attempt = fn => { try { return { status: 'ok', value: plain(fn()) }; } catch (e) { return { status: 'error', error: String(e.message ?? e).slice(0, 200) }; } };

    const accounts = sample.accounts.map(a => ({
        address: a.address,
        type: a.type,
        reference: attempt(() => coder.accounts.decode(a.type, Buffer.from(a.data, 'base64'))),
    }));

    const instructions = sample.instructions.map(ix => ({
        signature: ix.signature,
        reference: attempt(() => {
            const decoded = coder.instruction.decode(Buffer.from(ix.data, 'base64'));
            if (!decoded) throw new Error('no instruction matched');
            return decoded;
        }),
    }));

    fs.writeFileSync(path.join(cache, 'reference', file), JSON.stringify({ programId: sample.programId, accounts, instructions }, null, 1) + '\n');
}
