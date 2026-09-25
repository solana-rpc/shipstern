// Build and run the decoder in batches, dropping any program whose generated
// parser does not compile. A program is dropped on both sides, and the report
// records which side failed with which errors, so a converter difference
// cannot hide here.
//
//   node build-decoder.mjs <cache-dir> [batch-size=16]
//
// Batches keep each compile small: ~200 generated parsers in one crate need
// more memory than a laptop has. Writes <cache>/decoded/ and
// <cache>/compile-failures.json.
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const [cache, batchSize = '16'] = process.argv.slice(2);
const skipFile = path.join(cache, 'skip.txt');
const ids = dir => fs.readdirSync(path.join(cache, dir)).filter(f => f.endsWith('.codama.json')).map(f => f.slice(0, -'.codama.json'.length));
const all = [...new Set([...ids('rust'), ...ids('jsnodes')])].sort();
const failures = {};

fs.rmSync(path.join(cache, 'decoded'), { recursive: true, force: true });

const run = (cmd, args, opts = {}) => spawnSync(cmd, args, { encoding: 'utf8', ...opts });

for (let start = 0; start < all.length; start += Number(batchSize)) {
    const batch = new Set(all.slice(start, start + Number(batchSize)));

    for (let round = 1; ; round++) {
        const keep = [...batch].filter(id => !(id in failures));
        if (!keep.length) break;

        fs.writeFileSync(skipFile, all.filter(id => !keep.includes(id)).join('\n') + '\n');
        if (run('node', ['js/gen-parsers.mjs', cache, skipFile], { stdio: 'inherit' }).status !== 0) process.exit(1);

        const build = run('cargo', ['build', '--bin', 'decode', '--message-format', 'short']);

        if (build.status === 0) {
            if (run('cargo', ['run', '-q', '--bin', 'decode'], { stdio: 'inherit' }).status !== 0) process.exit(1);
            console.log(`batch ${start / Number(batchSize) + 1}: decoded ${keep.length} programs`);
            break;
        }

        // Map each error line in generated.rs back to the module it falls in.
        const lines = fs.readFileSync(path.join(cache, 'generated.rs'), 'utf8').split('\n');
        const modules = [];
        lines.forEach((l, i) => {
            const m = /^pub mod ([pj])_(\w+) \{/.exec(l);
            if (m) modules.push({ line: i + 1, side: m[1] === 'p' ? 'rust' : 'js', id: m[2] });
        });

        let found = 0;
        for (const m of build.stderr.matchAll(/generated\.rs:(\d+):\d+: error(?:\[E\d+\])?: (.*)/g)) {
            const mod = modules.filter(x => x.line <= Number(m[1])).at(-1);
            if (!mod) continue;
            const entry = (failures[mod.id] ??= { rust: [], js: [] });
            const msg = m[2].replace(/`[^`]+`/g, '`_`').slice(0, 120);
            if (!entry[mod.side].includes(msg)) entry[mod.side].push(msg);
            found++;
        }

        if (!found) {
            process.stderr.write(build.stderr.slice(-4000));
            console.error('build failed outside the generated parsers');
            process.exit(1);
        }
        console.log(`batch ${start / Number(batchSize) + 1}, round ${round}: ${Object.keys(failures).length} programs dropped so far`);
    }
}

fs.writeFileSync(path.join(cache, 'compile-failures.json'), JSON.stringify(failures, null, 2) + '\n');
console.log(`decoded ${all.length - Object.keys(failures).length} programs; ${Object.keys(failures).length} dropped for compile errors`);
