// The released wallet extensions the real-wallet runs use, pinned by version and sha256. Shared by the wallet-gate drivers
// (test/wallets/*.mjs) and the web real-wallet runs (web/e2e-real/common.mjs), so the two always load the same builds.
// Node built-ins only (no package import): web/e2e-real imports this file without the wallet-gate node_modules.
import { createHash } from 'node:crypto';

/**
 * Where each wallet comes from. `vendorDir` is the unpacked copy inside the wallet-gate vendor cache (tools/wallet-gate/vendor/); the
 * download URLs are the Chrome Web Store update endpoint for the CRX builds and the release zip for Kaspire.
 *
 * Every wallet is pinned: `version` is the manifest version the runs use, and a download must have the sha256 below
 * (`crxSha256` for the CRX file, `zipSha256` for the zip). The Chrome Web Store endpoint always serves the newest release,
 * so when a wallet publishes a new version the download is refused until the pin is moved here (new version and sha256,
 * after looking at the release). An unpacked copy is used only when its manifest version equals the pin.
 */
export const EXTENSIONS = {
  kasware: {
    label: 'KasWare',
    vendorDir: 'ext-kasware/unpacked',
    id: 'hklhheigdmpoolooomdihmhlpjjdbklf',
    crxUrl: 'https://clients2.google.com/service/update2/crx?response=redirect&prodversion=153.0.0.0&acceptformat=crx3&x=id%3Dhklhheigdmpoolooomdihmhlpjjdbklf%26uc',
    version: '0.10.0',
    crxSha256: 'c2a9cf257f249653ff856eab02e5ab681a7590d8c58f58feecc4306b5354dcf0',
  },
  kaspire: {
    label: 'Kaspire',
    vendorDir: 'kaspire/ext',
    zipUrl: 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet/releases/download/v0.11.37/kaspire-extension-0.5.1.zip',
    version: '0.5.1',
    zipSha256: '8c44f8f9624e552bf7d75b07d981d4c8c7921e0679ea25b735195d4452cc5527',
  },
  kastle: {
    label: 'Kastle',
    vendorDir: 'kastle/ext',
    id: 'oambclflhjfppdmkghokjmpppmaebego',
    crxUrl: 'https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0.0.0&acceptformat=crx2,crx3&x=id%3Doambclflhjfppdmkghokjmpppmaebego%26uc',
    version: '2.61.0',
    crxSha256: '6b1220ceefb73636fc0b47e9a8e7e6c93463067e34457af4f5d473b5fda0ba37',
  },
};

export const sha256Hex = (buf) => createHash('sha256').update(buf).digest('hex');

/** The zip payload of a CRX2 / CRX3 file. */
export function crxToZip(buf) {
  const b = Buffer.from(buf);
  if (b.toString('latin1', 0, 4) !== 'Cr24') throw new Error('not a CRX file (missing Cr24 magic)');
  const version = b.readUInt32LE(4);
  if (version === 3) return b.subarray(12 + b.readUInt32LE(8));
  if (version === 2) return b.subarray(16 + b.readUInt32LE(8) + b.readUInt32LE(12));
  throw new Error(`unsupported CRX version ${version}`);
}

/** The manifest version inside an unzipped package (`{ path: bytes }`), or undefined. */
export function packageVersion(files) {
  const text = Buffer.from(files['manifest.json'] ?? []).toString('utf8').replace(/^﻿/, '');
  return text ? JSON.parse(text).version : undefined;
}

/** Throws unless `pkg` (the downloaded CRX / zip bytes) has the pinned sha256 of `spec`. Returns the sha256. */
export function checkPinnedPackage(spec, pkg) {
  const sha = sha256Hex(pkg);
  const want = spec.zipUrl ? spec.zipSha256 : spec.crxSha256;
  if (sha !== want) {
    throw new Error(`${spec.label} download sha256 ${sha} is not the pinned ${want} (${spec.version}): a different release is served; move the pin in EXTENSIONS (tools/wallet-gate/lib/extension-pins.mjs) after review`);
  }
  return sha;
}
