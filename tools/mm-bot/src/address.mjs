// The Kaspa address of an x-only key (bech32, version 0).

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const GENERATORS = [0x98f2bc8e61n, 0x79b76d99e2n, 0xf33e5fb3c4n, 0xae2eabe2a8n, 0x1e4f43e470n];

function polymod(values) {
  let c = 1n;
  for (const d of values) {
    const c0 = c >> 35n;
    c = ((c & 0x07ffffffffn) << 5n) ^ BigInt(d);
    for (let i = 0; i < 5; i++) if ((c0 >> BigInt(i)) & 1n) c ^= GENERATORS[i];
  }
  return c ^ 1n;
}

function toWords(bytes) {
  const out = [];
  let acc = 0;
  let bits = 0;
  for (const b of bytes) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out.push((acc >> bits) & 31);
    }
  }
  if (bits > 0) out.push((acc << (5 - bits)) & 31);
  return out;
}

/** The address of a 64-hex x-only public key on `network` (mainnet: `kaspa:`, testnet-10: `kaspatest:`). */
export function addressOfXOnly(xOnlyHex, network) {
  if (!/^[0-9a-f]{64}$/.test(xOnlyHex)) throw new Error('an x-only key is 64 lower-case hex');
  const prefix = network === 'mainnet' ? 'kaspa' : 'kaspatest';
  const payload = toWords([0, ...Buffer.from(xOnlyHex, 'hex')]);
  const pre = [...prefix].map((ch) => ch.charCodeAt(0) & 31);
  const sum = polymod([...pre, 0, ...payload, 0, 0, 0, 0, 0, 0, 0, 0]);
  const check = [];
  for (let i = 0; i < 8; i++) check.push(Number((sum >> BigInt(5 * (7 - i))) & 31n));
  return `${prefix}:${[...payload, ...check].map((w) => CHARSET[w]).join('')}`;
}
