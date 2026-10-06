// Minimal JSON facade over the node bindings of kob-wasm (web/wasm/node, built from crates/kob-wasm) for the mock server.
// The typed facade of the app lives in src/kob/wasm.ts (TypeScript); the mock is plain node ESM, so it wraps the raw string API itself.
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

export function loadKobMock() {
  const raw = require('../wasm/node/kob_wasm.js');
  raw.selfCheck();
  const j = JSON.parse;
  const call = (name, f) => {
    try {
      return f();
    } catch (e) {
      throw new Error(`kob-wasm ${name}: ${String(e?.message ?? e)}`);
    }
  };
  return {
    raw,
    templates: () => j(raw.templates()),
    keeperTips: () => j(raw.keeperTips()),
    validate: (signed) => call('validate', () => j(raw.validate(JSON.stringify(signed)))),
    masses: (tx) => call('masses', () => j(raw.masses(JSON.stringify(tx)))),
    recoverOrders: (tx) => call('recoverOrders', () => j(raw.recoverOrders(JSON.stringify(tx)))),
    // in-place amends (AMEND records) of a SIGNED tx: the previous state is proven by the order input's signature script
    recoverAmends: (tx) => call('recoverAmends', () => j(raw.recoverAmends(JSON.stringify(tx), ''))),
    decodePayload: (hex) => call('decodePayload', () => j(raw.decodePayload(hex))),
    decodeState: (kind, hex) => call('decodeState', () => j(raw.decodeState(kind, hex))),
    encodeState: (state) => call('encodeState', () => raw.encodeState(JSON.stringify(state))),
    scriptPublicKey: (state) => call('scriptPublicKey', () => raw.scriptPublicKey(JSON.stringify(state))),
    tokenScriptPublicKey: (program, state) => call('tokenScriptPublicKey', () => raw.tokenScriptPublicKey(program, JSON.stringify(state))),
  };
}
