// Regenerate the expected outputs for every `<name>.anchor.json` here.
//
//   npm install --no-save codama@1.11.0 @codama/nodes-from-anchor@1.5.6
//   node regenerate.mjs
//
// <name>.codama.json  what `codama convert` produces (rootNodeFromAnchor)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { rootNodeFromAnchor } from '@codama/nodes-from-anchor';

const dir = path.dirname(fileURLToPath(import.meta.url));
const write = (file, node) => fs.writeFileSync(path.join(dir, file), JSON.stringify(node, null, 2) + '\n');

for (const file of fs.readdirSync(dir).filter(f => f.endsWith('.anchor.json')).sort()) {
    const name = file.slice(0, -'.anchor.json'.length);
    const idl = JSON.parse(fs.readFileSync(path.join(dir, file), 'utf8'));

    write(`${name}.codama.json`, rootNodeFromAnchor(idl));
    console.log(`regenerated ${name}`);
}
