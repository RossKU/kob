// Pure-JS (no wasm, no node built-ins) script + SilverScript ABI helpers.
// Mirrors kaspa-txscript ScriptBuilder (add_data / add_i64 / add_data_with_push_opcode) and
// silverscript-abi (encode_entry_sig_script, encode_runtime_state_script) for the ABI subset
// that KOB uses: int, bool, byte, bytes, pubkey, sig, fixed_bytes, arrays, struct arrays.
// Runs unchanged in node and in the browser.

export const hex = (u8) => Array.from(u8, (b) => b.toString(16).padStart(2, '0')).join('');
export function unhex(s) {
  if (typeof s !== 'string' || s.length % 2 || /[^0-9a-fA-F]/.test(s)) throw new Error('bad hex: ' + String(s).slice(0, 40));
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(2 * i, 2 * i + 2), 16);
  return out;
}
export function concat(...parts) {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
}
export const eq = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);

// ---- numbers -------------------------------------------------------------------------------
/** serialize_i64(v, size): little-endian sign-magnitude; size=undefined -> minimal script number. */
export function serializeI64(value, size) {
  let v = BigInt(value);
  const neg = v < 0n;
  let m = neg ? -v : v;
  const bytes = [];
  let lastSaturated = false;
  while (m > 0n) {
    const b = Number(m & 0xffn);
    lastSaturated = (b & 0x80) !== 0;
    bytes.push(b);
    m >>= 8n;
  }
  if (lastSaturated) bytes.push(0);
  if (size !== undefined) {
    if (bytes.length > size) throw new Error(`number ${value} does not fit ${size} bytes`);
    while (bytes.length < size) bytes.push(0);
  }
  if (neg) {
    if (bytes.length === 0) throw new Error('unreachable');
    bytes[bytes.length - 1] |= 0x80;
  }
  return Uint8Array.from(bytes);
}

// ---- pushes --------------------------------------------------------------------------------
const OP_0 = 0x00, OP_PUSHDATA1 = 0x4c, OP_PUSHDATA2 = 0x4d, OP_PUSHDATA4 = 0x4e, OP_1NEGATE = 0x4f, OP_1 = 0x51;

function pushWithDataOpcode(data) {
  const n = data.length;
  if (n === 0) return Uint8Array.of(OP_0);
  if (n <= 75) return concat(Uint8Array.of(n), data);
  if (n <= 0xff) return concat(Uint8Array.of(OP_PUSHDATA1, n), data);
  if (n <= 0xffff) return concat(Uint8Array.of(OP_PUSHDATA2, n & 0xff, n >> 8), data);
  return concat(Uint8Array.of(OP_PUSHDATA4, n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff, (n >>> 24) & 0xff), data);
}
/** ScriptBuilder::add_data (canonical: [] -> OP_0, [1..16] -> OP_N, [0x81] -> OP_1NEGATE). */
export function pushData(data) {
  if (data.length === 1) {
    if (data[0] === 0x81) return Uint8Array.of(OP_1NEGATE);
    if (data[0] >= 1 && data[0] <= 16) return Uint8Array.of(OP_1 - 1 + data[0]);
  }
  return pushWithDataOpcode(data);
}
/** ScriptBuilder::add_data_with_push_opcode (used for the covenant state span). */
export const pushDataExplicit = pushWithDataOpcode;
/** ScriptBuilder::add_i64. */
export function pushI64(v) {
  v = BigInt(v);
  if (v === 0n) return Uint8Array.of(OP_0);
  if (v === -1n || (v >= 1n && v <= 16n)) return Uint8Array.of(OP_1 - 1 + Number(v));
  return pushData(serializeI64(v));
}

/** Parses a push-only script into its data items (throws on non-push opcodes). */
export function parsePushes(script) {
  const out = [];
  let i = 0;
  while (i < script.length) {
    const op = script[i++];
    let len;
    if (op === OP_0) { out.push(new Uint8Array(0)); continue; }
    if (op >= 1 && op <= 75) len = op;
    else if (op === OP_PUSHDATA1) { len = script[i]; i += 1; }
    else if (op === OP_PUSHDATA2) { len = script[i] | (script[i + 1] << 8); i += 2; }
    else if (op === OP_PUSHDATA4) { len = (script[i] | (script[i + 1] << 8) | (script[i + 2] << 16) | (script[i + 3] << 24)) >>> 0; i += 4; }
    else if (op === OP_1NEGATE) { out.push(Uint8Array.of(0x81)); continue; }
    else if (op >= 0x51 && op <= 0x60) { out.push(Uint8Array.of(op - 0x50)); continue; }
    else throw new Error('non-push opcode 0x' + op.toString(16) + ' at ' + (i - 1));
    if (i + len > script.length) throw new Error('truncated push');
    out.push(script.slice(i, i + len));
    i += len;
  }
  return out;
}

// ---- ABI -----------------------------------------------------------------------------------
function expectHexBytes(v, len, what) {
  const b = typeof v === 'string' ? unhex(v) : v;
  if (!(b instanceof Uint8Array)) throw new Error(`${what}: expected bytes`);
  if (len !== undefined && b.length !== len) throw new Error(`${what}: expected ${len} bytes, got ${b.length}`);
  return b;
}

/** Payload encoding used inside array payloads / state (fixed width leaf types). */
function fixedPayload(ty, value, what) {
  switch (ty.kind) {
    case 'int': case 'temporal': return serializeI64(value, 8);
    case 'bool': return Uint8Array.of(value ? 1 : 0);
    case 'byte': return Uint8Array.of(Number(value));
    case 'pubkey': return expectHexBytes(value, 32, what);
    case 'sig': return expectHexBytes(value, 65, what);
    case 'datasig': return expectHexBytes(value, 64, what);
    case 'fixed_bytes': return expectHexBytes(value, ty.len, what);
    case 'fixed_array': return concat(...value.map((v) => fixedPayload(ty.item, v, what)));
    default: throw new Error(`unsupported fixed payload type ${ty.kind} (${what})`);
  }
}
function arrayPayload(item, values, what) {
  if (item.kind === 'struct') throw new Error('struct array payload must be flattened field-wise');
  return concat(new Uint8Array(0), ...values.map((v) => fixedPayload(item, v, what)));
}

export class Abi {
  /** @param artifact parsed silverc artifact JSON; @param contractName key in artifact.contracts */
  constructor(artifact, contractName) {
    this.artifact = artifact;
    this.name = contractName;
    this.contract = artifact.contracts[contractName];
    if (!this.contract) throw new Error('no contract ' + contractName);
    this.structs = artifact.structs || {};
  }
  get bytecode() { return Uint8Array.from(this.contract.compiled.bytecode); }
  get stateSpan() { return this.contract.compiled.state_span; }
  structFields(name) {
    if (name === 'State') return this.contract.runtime_state.fields;
    const s = this.structs[name];
    if (!s) throw new Error('unknown struct ' + name);
    return s.fields;
  }
  entry(name) {
    const e = this.contract.entries[name];
    if (!e) throw new Error('unknown entry ' + name);
    return e;
  }
  dispatchTag(entryName) { return unhex(this.entry(entryName).dispatch_tag); }

  pushArg(name, ty, value) {
    switch (ty.kind) {
      case 'int': case 'temporal': return pushI64(value);
      case 'bool': return pushI64(value ? 1 : 0);
      case 'byte': return pushData(Uint8Array.of(Number(value)));
      case 'bytes': return pushData(expectHexBytes(value, undefined, name));
      case 'pubkey': return pushData(expectHexBytes(value, 32, name));
      case 'sig': return pushData(expectHexBytes(value, 65, name));
      case 'datasig': return pushData(expectHexBytes(value, 64, name));
      case 'fixed_bytes': return pushData(expectHexBytes(value, ty.len, name));
      case 'struct': return concat(...this.structFields(ty.name).map((f) => this.pushArg(f.name, f.type, value[f.name])));
      case 'fixed_array': case 'dynamic_array': {
        if (ty.kind === 'fixed_array' && value.length !== ty.len) throw new Error(`${name}: expected ${ty.len} items`);
        if (ty.item.kind === 'struct') {
          // struct arrays are pushed field-wise: for each field, one dynamic array of that field
          return concat(...this.structFields(ty.item.name).map((f) =>
            this.pushArg(f.name, { kind: 'dynamic_array', item: f.type }, value.map((o) => o[f.name]))));
        }
        return pushData(arrayPayload(ty.item, value, name));
      }
      default: throw new Error('unsupported arg type ' + ty.kind);
    }
  }
  /** args (by ABI param order) -> the signature-script prefix: pushed args, then 4-byte dispatch tag.
   *  The caller appends pushData(redeemScript). */
  argsAndTag(entryName, args) {
    const e = this.entry(entryName);
    if (e.params.length !== args.length) throw new Error(`${entryName}: expected ${e.params.length} args, got ${args.length}`);
    const parts = e.params.map((p, i) => this.pushArg(p.name, p.type, args[i]));
    parts.push(pushData(this.dispatchTag(entryName)));
    return concat(...parts);
  }
  /** Full sigscript = pushed args + dispatch tag + redeem script push. */
  sigscript(entryName, args, redeemScript) {
    return concat(this.argsAndTag(entryName, args), pushData(redeemScript));
  }
  /** Runtime-state span bytes (each leaf pushed with an explicit OP_DATAn). values keyed by field name. */
  encodeState(values) {
    const parts = this.contract.runtime_state.fields.map((f) => {
      if (!(f.name in values)) throw new Error('missing state field ' + f.name);
      return pushDataExplicit(fixedPayload(f.type, values[f.name], f.name));
    });
    return concat(...parts);
  }
}

// ---- KCC-20 (reference template) -----------------------------------------------------------
export const OWNER_P2PK_SCHNORR = 0x00;
export const BORROW_DISABLED = 0x00;

/** Runtime-state object for a KCC-20 UTXO. */
export function kcc20State({ amount, owner, ownerScheme = OWNER_P2PK_SCHNORR, borrowScheme = BORROW_DISABLED, borrowGuard, ext }) {
  return {
    amount: BigInt(amount),
    owner: expectHexBytes(owner, 32, 'owner'),
    owner_scheme: ownerScheme,
    borrow_scheme: borrowScheme,
    borrow_guard: borrowGuard ? expectHexBytes(borrowGuard, 32, 'borrow_guard') : new Uint8Array(32),
    extension_commitment: expectHexBytes(ext, 32, 'ext'),
  };
}
/** Redeem script for a KCC-20 UTXO: template prefix + encoded state + template suffix. */
export function kcc20Redeem(abi, state) {
  const span = abi.stateSpan;
  const bc = abi.bytecode;
  return concat(bc.slice(0, span.offset), abi.encodeState(state), bc.slice(span.offset + span.len));
}
