// KAS amounts: sompi as BigInt inside, decimal KAS strings outside.

export const SOMPI_PER_KAS = 100_000_000n;

/** "12.5", 12.5 or "12.5KAS" to sompi; "1000sompi" as is. Throws on anything else. */
export function toSompi(x) {
  const s = String(x).trim();
  const m = /^(\d+)(?:\.(\d{1,8}))?(kas)?$/i.exec(s);
  if (m) return BigInt(m[1]) * SOMPI_PER_KAS + BigInt((m[2] ?? '').padEnd(8, '0') || '0');
  const sm = /^(\d+)sompi$/i.exec(s);
  if (sm) return BigInt(sm[1]);
  throw new Error(`not a KAS amount: ${JSON.stringify(x)}`);
}

/** Sompi to a KAS string without trailing zeros: 150000000n -> "1.5". */
export function kas(sompi) {
  const v = BigInt(sompi);
  const neg = v < 0n;
  const a = neg ? -v : v;
  const frac = (a % SOMPI_PER_KAS).toString().padStart(8, '0').replace(/0+$/, '');
  return `${neg ? '-' : ''}${a / SOMPI_PER_KAS}${frac ? `.${frac}` : ''}`;
}

/** A decimal string of the CLI (sompi, may be negative) to BigInt; null and junk to 0n. */
export function big(v) {
  try {
    return v == null ? 0n : BigInt(String(v));
  } catch {
    return 0n;
  }
}

/** The UTC day of a time in ms: "2026-11-17". */
export const utcDay = (ms) => new Date(ms).toISOString().slice(0, 10);
