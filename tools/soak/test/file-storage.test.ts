// The tracker's file-backed storage: a file shared with other processes (holdings --seed, the CLIs, setup) must never be reverted by the
// bots' own next write.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, unlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { FileStorage } from '../src/file-storage.ts';

function tmp(): { dir: string; file: string; done: () => void } {
  const dir = mkdtempSync(join(tmpdir(), 'soak-fs-'));
  return { dir, file: join(dir, 'tracker-mm.json'), done: () => rmSync(dir, { recursive: true, force: true }) };
}
const onDisk = (file: string) => JSON.parse(readFileSync(file, 'utf8')) as Record<string, string>;
const store = (n: number) => JSON.stringify({ v: 1, items: Array.from({ length: n }, (_, i) => ({ i })) });
/** an external writer (a script): a plain write, not the storage's atomic one; the mtime is moved so the change is visible even on a coarse clock */
let bump = 0;
function external(file: string, content: unknown): void {
  writeFileSync(file, typeof content === 'string' ? content : JSON.stringify(content));
  const t = new Date(Date.now() + 5000 * ++bump);
  utimesSync(file, t, t);
}

test('basic round trip, a missing file reads as empty, a new instance sees the data', () => {
  const t = tmp();
  try {
    const a = new FileStorage(t.file);
    assert.equal(a.getItem('k'), null);
    a.setItem('k', 'v');
    assert.equal(a.getItem('k'), 'v');
    assert.equal(new FileStorage(t.file).getItem('k'), 'v');
    a.removeItem('k');
    assert.equal(a.getItem('k'), null);
    assert.deepEqual(onDisk(t.file), {});
    assert.deepEqual(readdirSync(t.dir), ['tracker-mm.json']); // no temp files left behind
  } finally {
    t.done();
  }
});

test('a key another process writes is picked up on the next read', () => {
  const t = tmp();
  try {
    const bots = new FileStorage(t.file);
    bots.setItem('mm', store(3));
    external(t.file, { mm: store(29), other: 'x' }); // holdings.cjs --seed
    assert.equal(bots.getItem('mm'), store(29));
    assert.equal(bots.getItem('other'), 'x');
  } finally {
    t.done();
  }
});

test('the bots next write does not revert what another process wrote: only the key being set is replaced', () => {
  const t = tmp();
  try {
    const bots = new FileStorage(t.file);
    bots.setItem('mm', store(3));
    bots.setItem('t1', store(2));
    // an operator re-seeds both keys and adds one while the bots process is running
    external(t.file, { mm: store(29), t1: store(45), t2: store(7) });
    bots.setItem('mm', store(30)); // the bots add a candidate to mm only
    const d = onDisk(t.file);
    assert.equal(d.mm, store(30));
    assert.equal(d.t1, store(45)); // not reverted to the bots' 2
    assert.equal(d.t2, store(7)); // not dropped
  } finally {
    t.done();
  }
});

test('two instances of one file (two processes) keep each other\'s keys', () => {
  const t = tmp();
  try {
    const a = new FileStorage(t.file);
    const b = new FileStorage(t.file);
    a.setItem('a', '1');
    b.setItem('b', '2');
    a.setItem('a', '3');
    b.setItem('c', '4');
    assert.deepEqual(onDisk(t.file), { a: '3', b: '2', c: '4' });
  } finally {
    t.done();
  }
});

test('removeItem merges too', () => {
  const t = tmp();
  try {
    const a = new FileStorage(t.file);
    a.setItem('a', '1');
    external(t.file, { a: '1', b: '2' });
    a.removeItem('a');
    assert.deepEqual(onDisk(t.file), { b: '2' });
  } finally {
    t.done();
  }
});

test('a deleted file reads as empty and the next write starts a fresh one', () => {
  const t = tmp();
  try {
    const a = new FileStorage(t.file);
    a.setItem('a', '1');
    unlinkSync(t.file);
    assert.equal(a.getItem('a'), null);
    a.setItem('b', '2');
    assert.deepEqual(onDisk(t.file), { b: '2' });
  } finally {
    t.done();
  }
});

test('an unreadable file is served from memory, and saved aside before it is replaced', () => {
  const t = tmp();
  try {
    const warns: string[] = [];
    const a = new FileStorage(t.file, { warn: (m) => warns.push(m) });
    a.setItem('mm', store(5));
    external(t.file, '{"mm": "torn');
    assert.equal(a.getItem('mm'), store(5));
    assert.equal(warns.filter((m) => /unreadable/.test(m)).length, 1);
    a.getItem('mm'); // looked at again, warned only once per file state
    assert.equal(warns.filter((m) => /unreadable/.test(m)).length, 1);
    a.setItem('mm', store(6));
    assert.deepEqual(onDisk(t.file), { mm: store(6) });
    const aside = readdirSync(t.dir).filter((f) => f.includes('.unreadable-'));
    assert.equal(aside.length, 1);
    assert.equal(readFileSync(join(t.dir, aside[0]!), 'utf8'), '{"mm": "torn');
  } finally {
    t.done();
  }
});

test('the store-shrank warning is kept (a tracker record losing more than half its candidates)', () => {
  const t = tmp();
  try {
    const warns: { msg: string; f?: Record<string, unknown> }[] = [];
    const a = new FileStorage(t.file, { warn: (msg, f) => warns.push({ msg, f }) });
    a.setItem('mm', store(10));
    a.setItem('mm', store(6)); // not below half
    a.setItem('mm', store(2)); // 6 -> 2
    a.setItem('small', store(3));
    a.setItem('small', store(0)); // under 4 before: never warned
    a.setItem('plain', 'not json');
    const shrank = warns.filter((w) => w.msg === 'token tracker store shrank');
    assert.equal(shrank.length, 1);
    assert.deepEqual([shrank[0]!.f!.before, shrank[0]!.f!.after], [6, 2]);
    // the shrink is measured against the file as it is NOW, not the process's older copy
    external(t.file, { mm: store(40) });
    a.setItem('mm', store(5));
    assert.equal(warns.filter((w) => w.msg === 'token tracker store shrank').length, 2);
  } finally {
    t.done();
  }
});

test('the file is never left half written: the temp file is gone and the content parses after many writes', () => {
  const t = tmp();
  try {
    const a = new FileStorage(t.file);
    for (let i = 0; i < 50; i++) a.setItem(`k${i % 5}`, store(i));
    assert.equal(Object.keys(onDisk(t.file)).length, 5);
    assert.equal(existsSync(t.file + '.tmp'), false);
    assert.deepEqual(readdirSync(t.dir), ['tracker-mm.json']);
  } finally {
    t.done();
  }
});
