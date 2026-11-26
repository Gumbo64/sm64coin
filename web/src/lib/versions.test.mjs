import assert from 'node:assert/strict';
import { decodeRpcVersion, shortSha, commitUrl, describeNode, updateAvailable, versionText } from './versions.js';

assert.equal(decodeRpcVersion(196624), '3.16');
assert.equal(decodeRpcVersion(65536), '1.0');
assert.equal(decodeRpcVersion(0), null);
assert.equal(decodeRpcVersion('x'), null);
assert.equal(decodeRpcVersion(undefined), null);

assert.equal(shortSha('38fe704a1b2c3d4e5f60718293a4b5c6d7e8f901'), '38fe704');
assert.equal(shortSha(''), null);
assert.equal(shortSha('dev'), null);
assert.equal(commitUrl('38FE704a1b2c3d4e5f60718293a4b5c6d7e8f901'), 'https://github.com/Gumbo64/sm64coin/commit/38fe704a1b2c3d4e5f60718293a4b5c6d7e8f901');
assert.equal(commitUrl(undefined), null);

// a new node: version from get_info (also restricted), plus game build and rules
let d = describeNode({ sm64coin_version: '0.3.3', sm64_game_build: 'abc', sm64_rules: 'star-cycle', version: '' }, { version: 196624 });
assert.deepEqual([d.known, d.label, d.rpc, d.gameBuild, d.rules], [true, '0.3.3', '3.16', 'abc', 'star-cycle']);
// a local build
assert.equal(describeNode({ sm64coin_version: 'dev' }, null).label, 'dev');
// an older node: only the RPC version, decoded
d = describeNode({ version: '', sm64_rules: 'star-cycle' }, { version: 196624 });
assert.deepEqual([d.known, d.label, d.gameBuild, d.rules], [false, 'older than 0.3.3 (RPC 3.16)', null, 'star-cycle']);
assert.equal(describeNode({}, null).label, 'older than 0.3.3');
// get_version alone can carry it
assert.equal(describeNode({}, { version: 196624, sm64coin_version: '0.3.4' }).label, '0.3.4');

assert.equal(updateAvailable('0.3.4', '0.3.3'), true);
assert.equal(updateAvailable('0.3.3', '0.3.3'), false);
assert.equal(updateAvailable('0.3.3', 'dev'), false);
assert.equal(updateAvailable(null, '0.3.3'), false);
assert.equal(updateAvailable('0.3.4', 'v0.3.3'), true);
assert.equal(versionText('dev'), 'dev (local build)');
assert.equal(versionText('v0.3.3'), '0.3.3');
console.log('versions ok');
