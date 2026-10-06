// Catalog registry: loads and validates the suite catalogs.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));

export const SUITE_NAMES = ['kcc20-sell', 'kcc20-buy', 'kron-sell', 'kron-buy', 'pair', 'cond-pair', 'cond-pair-rpt', 'ifd-pair'];

// Suites whose catalogs are not run (name -> reason). `--suite all` skips them; naming one fails. None at present.
export const PENDING_SUITES = {};

const normId = s => String(s).trim();

function normalize(suite, m) {
  const where = `${suite.name}:${m.id}`;
  const fail = msg => {
    throw new Error(`catalog ${where}: ${msg}`);
  };
  if (!m.id || /\s/.test(normId(m.id))) fail('id must be a non-empty token without whitespace');
  if (!m.file) fail('missing file');
  if (!Array.isArray(m.edits) || !m.edits.length) fail('missing edits');
  // runs: testFn -> { expect: ids that must flip, hold: ids that must stay rejected }
  const byTest = new Map();
  const slot = t => byTest.get(t) ?? byTest.set(t, { testFn: t, expect: [], hold: [] }).get(t);
  if (m.test) for (const id of m.expect ?? []) slot(m.test).expect.push(normId(id));
  for (const [t, ids] of Object.entries(m.run ?? {})) slot(t).expect.push(...ids.map(normId));
  for (const [t, ids] of Object.entries(m.hold ?? {})) slot(t).hold.push(...ids.map(normId));
  const runs = [...byTest.values()];
  if (!runs.length || runs.every(r => !r.expect.length && !r.hold.length)) fail('need expect (test + expect, or run) and/or hold');
  const inputOnly = (m.inputOnly ?? []).map(normId);
  const also = m.also ? (Array.isArray(m.also) ? m.also : [m.also]) : [];
  for (const a of also) if (!a.file || !Array.isArray(a.edits) || !a.edits.length) fail('bad also entry');
  return {
    suite: suite.name,
    key: `${suite.name}:${normId(m.id)}`,
    id: normId(m.id),
    file: m.file,
    note: m.note ?? '',
    edits: m.edits,
    also,
    runs,
    holdOnly: runs.every(r => !r.expect.length),
    inputOnly,
  };
}

export async function loadSuite(name, root) {
  const mod = await import(pathToFileURL(path.join(HERE, '..', 'catalogs', `${name}.mjs`)).href);
  const suite = mod.suite;
  if (suite.name !== name) throw new Error(`catalog ${name}: suite.name is ${suite.name}`);
  for (const k of ['family', 'testBin', 'srcDir']) if (!suite[k]) throw new Error(`catalog ${name}: suite.${k} missing`);
  if (suite.templateMarker !== null && !(suite.templateMarker instanceof RegExp)) throw new Error(`catalog ${name}: templateMarker`);
  if (!Number.isInteger(suite.expectedTemplates) || suite.expectedTemplates < 1) throw new Error(`catalog ${name}: expectedTemplates`);
  const srcAbs = path.join(root, suite.srcDir);
  const silFiles = fs.readdirSync(srcAbs).filter(f => f.endsWith('.sil'));
  const mutations = mod.mutations.map(m => normalize(suite, m));
  const seen = new Set();
  for (const m of mutations) {
    if (seen.has(m.id)) throw new Error(`catalog ${name}: duplicate id ${m.id}`);
    seen.add(m.id);
    for (const f of [m.file, ...m.also.map(a => a.file)]) {
      if (!silFiles.includes(`${f}.sil`)) throw new Error(`catalog ${name}:${m.id}: ${f}.sil not in ${suite.srcDir}`);
    }
  }
  return { suite: { ...suite, srcAbs, silFiles }, mutations };
}
