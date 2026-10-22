#!/usr/bin/env node
// Packages the release binaries of one network and target into the archive operators download (.github/workflows/release.yml).
//
//   node scripts/package-release.mjs --version 1.2.3 --network mainnet --target x86_64-pc-windows-msvc \
//        --bin-dir target/x86_64-pc-windows-msvc/release [--out dist]
//   node scripts/package-release.mjs --sums dist          write dist/SHA256SUMS over every archive in dist
//
// The archive `kob-executor-<version>-<network>-<target>.zip` (Windows targets) or `.tar.gz` (every other target) holds one
// directory of that name with kob-executor and kob-cli, QUICKSTART.md (docs/ops/executor-quickstart.md), LICENSE, NOTICE and a
// SHA256SUMS of those files. The archive is written here, not by zip / tar: the bytes depend only on the inputs and on
// SOURCE_DATE_EPOCH (default: the commit time of HEAD) as every entry's time, fixed modes and no owner names, so two builds of
// identical binaries give identical archives on any runner (docs/ops/release.md 6). Needs Node >= 22 (zlib.crc32).
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const NETWORKS = ['mainnet', 'testnet-10'];

function args(argv) {
  const o = {};
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    if (!k.startsWith('--')) throw new Error(`unexpected argument ${k}`);
    o[k.slice(2)] = argv[++i];
    if (o[k.slice(2)] === undefined) throw new Error(`${k} needs a value`);
  }
  return o;
}

const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest('hex');
const sumsText = (files) => files.map(([name, data]) => `${sha256(data)}  ${name}\n`).join('');

function epoch() {
  if (process.env.SOURCE_DATE_EPOCH) return Number(process.env.SOURCE_DATE_EPOCH);
  try {
    return Number(execFileSync('git', ['log', '-1', '--format=%ct'], { cwd: root, encoding: 'utf8' }).trim());
  } catch {
    return 0;
  }
}

// ---- zip (stored names in UTF-8, deflate, DOS time of `when`, Unix modes in the external attributes)
function dosTime(when) {
  const d = new Date(Math.max(when, 315532800) * 1000); // DOS time starts in 1980
  const time = (d.getUTCHours() << 11) | (d.getUTCMinutes() << 5) | (d.getUTCSeconds() >> 1);
  const date = ((d.getUTCFullYear() - 1980) << 9) | ((d.getUTCMonth() + 1) << 5) | d.getUTCDate();
  return { time, date };
}

function zip(entries, when) {
  const { time, date } = dosTime(when);
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const { name, data, mode } of entries) {
    const nameBuf = Buffer.from(name, 'utf8');
    const isDir = name.endsWith('/');
    const body = isDir ? Buffer.alloc(0) : zlib.deflateRawSync(data, { level: 9 });
    const crc = isDir ? 0 : zlib.crc32(data);
    const method = isDir ? 0 : 8;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4); // version needed
    local.writeUInt16LE(0x0800, 6); // UTF-8 names
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(time, 10);
    local.writeUInt16LE(date, 12);
    local.writeUInt32LE(crc >>> 0, 14);
    local.writeUInt32LE(body.length, 18);
    local.writeUInt32LE(isDir ? 0 : data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    local.writeUInt16LE(0, 28);
    locals.push(local, nameBuf, body);
    const c = Buffer.alloc(46);
    c.writeUInt32LE(0x02014b50, 0);
    c.writeUInt16LE((3 << 8) | 20, 4); // made by Unix, so the external attributes carry the mode
    c.writeUInt16LE(20, 6);
    c.writeUInt16LE(0x0800, 8);
    c.writeUInt16LE(method, 10);
    c.writeUInt16LE(time, 12);
    c.writeUInt16LE(date, 14);
    c.writeUInt32LE(crc >>> 0, 16);
    c.writeUInt32LE(body.length, 20);
    c.writeUInt32LE(isDir ? 0 : data.length, 24);
    c.writeUInt16LE(nameBuf.length, 28);
    c.writeUInt32LE(((((isDir ? 0o40000 : 0o100000) | mode) << 16) | (isDir ? 0x10 : 0)) >>> 0, 38);
    c.writeUInt32LE(offset, 42);
    centrals.push(c, nameBuf);
    offset += 30 + nameBuf.length + body.length;
  }
  const central = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(central.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, central, end]);
}

// ---- tar.gz (ustar, uid / gid 0 without names, gzip without a name or time in its header)
function tarHeader(name, size, mode, type, when) {
  if (Buffer.byteLength(name) > 100) throw new Error(`tar: name too long: ${name}`);
  const h = Buffer.alloc(512);
  const put = (s, off, len) => h.write(s, off, len, 'utf8');
  const oct = (n, len) => n.toString(8).padStart(len - 1, '0') + '\0';
  put(name, 0, 100);
  put(oct(mode, 8), 100, 8);
  put(oct(0, 8), 108, 8);
  put(oct(0, 8), 116, 8);
  put(oct(size, 12), 124, 12);
  put(oct(when, 12), 136, 12);
  put('        ', 148, 8);
  put(type, 156, 1);
  put('ustar\0', 257, 6);
  put('00', 263, 2);
  let sum = 0;
  for (const b of h) sum += b;
  put(oct(sum, 7) + ' ', 148, 8);
  return h;
}

function targz(entries, when) {
  const parts = [];
  for (const { name, data, mode } of entries) {
    const isDir = name.endsWith('/');
    parts.push(tarHeader(name, isDir ? 0 : data.length, mode, isDir ? '5' : '0', when));
    if (!isDir) {
      parts.push(data);
      const pad = (512 - (data.length % 512)) % 512;
      if (pad) parts.push(Buffer.alloc(pad));
    }
  }
  parts.push(Buffer.alloc(1024));
  // gzip: zlib writes MTIME 0 and no file name, so the bytes depend on the content only
  return zlib.gzipSync(Buffer.concat(parts), { level: 9 });
}

function packageOne(o) {
  for (const k of ['version', 'network', 'target', 'bin-dir']) if (!o[k]) throw new Error(`--${k} is required`);
  if (!NETWORKS.includes(o.network)) throw new Error(`--network must be one of ${NETWORKS.join(', ')}`);
  if (!/^[0-9A-Za-z.+_-]+$/.test(o.version)) throw new Error(`--version ${o.version}: letters, digits and . + _ - only`);
  const windows = o.target.includes('windows');
  const exe = windows ? '.exe' : '';
  const base = `kob-executor-${o.version}-${o.network}-${o.target}`;
  const read = (p) => {
    if (!fs.existsSync(p)) throw new Error(`missing ${p}`);
    return fs.readFileSync(p);
  };
  // text files with LF line endings whatever the checkout did, so the archive does not depend on core.autocrlf
  const text = (p) => Buffer.from(read(p).toString('utf8').replace(/\r\n/g, '\n'), 'utf8');
  const files = [
    [`kob-executor${exe}`, read(path.join(o['bin-dir'], `kob-executor${exe}`)), 0o755],
    [`kob-cli${exe}`, read(path.join(o['bin-dir'], `kob-cli${exe}`)), 0o755],
    ['QUICKSTART.md', text(path.join(root, 'docs/ops/executor-quickstart.md')), 0o644],
    ['LICENSE', text(path.join(root, 'LICENSE')), 0o644],
    ['NOTICE', text(path.join(root, 'NOTICE')), 0o644],
  ];
  files.push(['SHA256SUMS', Buffer.from(sumsText(files)), 0o644]);
  const entries = [{ name: `${base}/`, data: null, mode: 0o755 }, ...files.map(([n, d, m]) => ({ name: `${base}/${n}`, data: d, mode: m }))];
  const when = epoch();
  const out = path.resolve(o.out ?? 'dist');
  fs.mkdirSync(out, { recursive: true });
  const file = path.join(out, base + (windows ? '.zip' : '.tar.gz'));
  fs.writeFileSync(file, windows ? zip(entries, when) : targz(entries, when));
  console.log(`${sha256(fs.readFileSync(file))}  ${path.basename(file)}`);
}

function writeSums(dir) {
  const names = fs
    .readdirSync(dir)
    .filter((n) => n.startsWith('kob-executor-') && (n.endsWith('.zip') || n.endsWith('.tar.gz')))
    .sort();
  if (!names.length) throw new Error(`no release archives in ${dir}`);
  const text = sumsText(names.map((n) => [n, fs.readFileSync(path.join(dir, n))]));
  fs.writeFileSync(path.join(dir, 'SHA256SUMS'), text);
  process.stdout.write(text);
}

try {
  const o = args(process.argv.slice(2));
  if (o.sums) writeSums(path.resolve(o.sums));
  else packageOne(o);
} catch (e) {
  console.error(`package-release: ${e.message}`);
  process.exit(2);
}
