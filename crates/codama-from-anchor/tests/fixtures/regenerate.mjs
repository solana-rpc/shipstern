// Regenerate the expected outputs for every `<name>.anchor.json` here.
//
//   npm install --no-save codama@1.11.0 @codama/nodes-from-anchor@1.5.6 @codama/visitors@1.11.0 @codama/nodes@1.11.0
//   node regenerate.mjs
//
// <name>.codama.json         what `codama convert` produces (rootNodeFromAnchor)
// <name>.parity.codama.json  the same pipeline without extractPdas,
//                            setInstructionAccountDefaultValues and
//                            deduplicateIdenticalDefinedTypes, which the Rust
//                            crate does not port
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertIsNode } from '@codama/nodes';
import { rootNodeFromAnchor, rootNodeFromAnchorWithoutDefaultVisitor } from '@codama/nodes-from-anchor';
import {
    flattenInstructionDataArgumentsVisitor,
    setFixedAccountSizesVisitor,
    transformU8ArraysToBytesVisitor,
    unwrapInstructionArgsDefinedTypesVisitor,
    visit,
} from '@codama/visitors';

// Same order as nodes-from-anchor's defaultVisitor.ts, minus the three passes.
const PORTED_PASSES = [
    setFixedAccountSizesVisitor,
    unwrapInstructionArgsDefinedTypesVisitor,
    flattenInstructionDataArgumentsVisitor,
    transformU8ArraysToBytesVisitor,
];

const dir = path.dirname(fileURLToPath(import.meta.url));
const write = (file, node) => fs.writeFileSync(path.join(dir, file), JSON.stringify(node, null, 2) + '\n');

for (const file of fs.readdirSync(dir).filter(f => f.endsWith('.anchor.json')).sort()) {
    const name = file.slice(0, -'.anchor.json'.length);
    const idl = JSON.parse(fs.readFileSync(path.join(dir, file), 'utf8'));

    write(`${name}.codama.json`, rootNodeFromAnchor(idl));

    let root = rootNodeFromAnchorWithoutDefaultVisitor(idl);
    for (const pass of PORTED_PASSES) {
        root = visit(root, pass());
        assertIsNode(root, 'rootNode');
    }
    // Parser parity covers pump_fun; a second 460 KB copy is not worth it.
    if (name !== 'pump_fun') write(`${name}.parity.codama.json`, root);

    console.log(`regenerated ${name}`);
}
