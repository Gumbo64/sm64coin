import test from 'node:test';
import assert from 'node:assert/strict';
import { asPort, effectivePublicPort, portHint, validListenPort, validPublicPort } from './nodeports.js';

test('port parsing', () => {
  assert.equal(asPort('16480'), 16480);
  assert.equal(asPort('nope', 7), 7);
  assert.equal(asPort('70000', 7), 7);
  assert.equal(asPort('12.5', 7), 7);
  assert.equal(validListenPort('16480'), true);
  assert.equal(validListenPort('80'), false);
  assert.equal(validListenPort('0'), false);
  assert.equal(validPublicPort('0'), true);
  assert.equal(validPublicPort('443'), true);
  assert.equal(validPublicPort(''), true);
  assert.equal(validPublicPort('99999'), false);
});

test('the advertised port falls back to the listening port', () => {
  assert.equal(effectivePublicPort({ p2p_port: 16480, p2p_external_port: 0 }), 16480);
  assert.equal(effectivePublicPort({ p2p_port: 16480, p2p_external_port: 32123 }), 32123);
});

test('the VPN hint only appears when outgoing works but incoming does not', () => {
  assert.equal(portHint({ answering: true, incoming_peers: 0, outgoing_peers: 2, p2p_port: 16480, p2p_external_port: 32123 }),
    'No incoming peers yet: if you use a VPN or router port forward, make sure public port 32123 reaches this machine\'s port 16480.');
  assert.equal(portHint({ answering: true, incoming_peers: 1, outgoing_peers: 2, p2p_port: 16480 }), null);
  assert.equal(portHint({ answering: false, incoming_peers: 0, outgoing_peers: 2 }), null);
});
