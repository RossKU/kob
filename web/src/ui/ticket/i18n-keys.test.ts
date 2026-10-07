// Every i18n key the ticket and the confirmation screen use exists in the English dictionary: literal keys are scanned out of the sources, keys built
// from a variable (`ticket.note.${tag}`, `confirm.order.${type}` ...) are enumerated from the vocabularies that produce them.
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { allKeys, has, rawEntry, t } from '../../i18n';
import { planOrder } from '../../kob/plan';
import { errors } from '../../kob/plan-types';
import { SIGNING_ISSUE_CODES } from '../../kob/decode';
import { NOTE_REQUIRES, NOTE_TAGS, buildDisclosureModel } from './disclosure-model';
import { CASES, CTX, TOKEN, form, ticketEnv } from './ticket-fixtures';
import { FIELDS, ORDER_TYPES, TYPE_GROUPS, buildIntent, type FieldErrorCode } from './form-state';

const dir = (rel: string) => fileURLToPath(new URL(rel, import.meta.url));
const read = (p: string) => readFileSync(p, 'utf8');
const sources = (rel: string): { file: string; text: string }[] =>
  readdirSync(dir(rel))
    .filter((f) => /\.(ts|tsx)$/.test(f) && !/\.test\./.test(f))
    .map((f) => ({ file: f, text: read(dir(rel) + f) }));

const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]!);

/** literal `'ticket.x.y'` / `'confirm.x.y'` keys */
function literalKeys(prefix: string, files: { text: string }[]): string[] {
  const out = new Set<string>();
  for (const f of files) for (const m of f.text.matchAll(new RegExp(`'(${prefix}\\.[A-Za-z0-9_.-]+)'`, 'g'))) out.add(m[1]!);
  return [...out];
}

const both = (key: string) => {
  expect(rawEntry(key), key).toBeTruthy();
};

describe('ticket keys', () => {
  it('every literal key of the ticket sources exists', () => {
    const keys = literalKeys('ticket', [...sources('./'), ...sources('../confirm/')]);
    expect(keys.length).toBeGreaterThan(80);
    // a key that is a prefix of dynamic ones (`ticket.note.<tag>`) is not a key
    for (const k of keys.filter((k) => !/\.(note|carrier|time)\.?$/.test(k))) if (has(k) || !/^ticket\.(note|carrier|time|err|group|opt|type|label|help|ph|severity)$/.test(k)) both(k);
  });

  it('types, groups, labels, options, errors, carriers, times and notes all have sentences', () => {
    for (const g of TYPE_GROUPS) both(`ticket.group.${g.group}`);
    for (const type of ORDER_TYPES) {
      both(`ticket.type.${type}.name`);
      both(`ticket.type.${type}.help`);
    }
    for (const id of Object.keys(FIELDS)) {
      both(`ticket.label.${id}`);
      for (const o of FIELDS[id]!.options ?? []) both(`ticket.opt.${id}.${o}`);
    }
    for (const code of ['required', 'format', 'precision', 'positive', 'range', 'negative', 'noRef'] satisfies FieldErrorCode[]) both(`ticket.err.${code}`);
    for (const sev of ['error', 'warning', 'info']) both(`ticket.severity.${sev}`);
    for (const k of ['activates', 'gtc', 'renew', 'gtd', 'day', 'ioc', 'fok']) both(`ticket.time.${k}`);
    for (const tag of NOTE_TAGS) both(`ticket.note.${tag}`);
    // the carrier kinds the planners emit (`kind: '...', amount`)
    const kinds = new Set<string>();
    for (const f of sources('../../kob/orders/')) for (const m of f.text.matchAll(/kind: '([A-Za-z]+)', amount/g)) kinds.add(m[1]!);
    expect(kinds.size).toBeGreaterThanOrEqual(8);
    for (const k of kinds) both(`ticket.carrier.${k}`);
  });

  it('label overrides and placeholders refer to real fields and types', () => {
    for (const k of allKeys().filter((k) => /^ticket\.(label|ph)\./.test(k))) {
      const body = k.replace(/^ticket\.(label|ph)\./, '').replace(/\.inv$/, ''); // `.inv`: the wording of the inverted market
      const type = ORDER_TYPES.find((ty) => body.endsWith(`.${ty}`));
      const id = type ? body.slice(0, -(type.length + 1)) : body;
      expect(FIELDS[body] ?? FIELDS[id], `${k}: ${id} is not a field`).toBeDefined();
    }
  });

  it('every note tag of every order type renders without a hole', () => {
    for (const [name, type, side, values] of CASES) {
      const intent = buildIntent(form(type, side, values), CTX).intent!;
      const plan = planOrder(ticketEnv(), intent);
      expect(errors(plan)).toEqual([]);
      const m = buildDisclosureModel(plan, { ticker: 'EXKCC', decimals: 8, scale: TOKEN, clock: ticketEnv().clock })!;
      for (const n of m.notes) {
        const text = t(n.key, n.params);
        expect(text, `${name}/${n.tag}: ${text}`).not.toMatch(/\{\w+\}/);
        // the sentence quotes only what the plan supplies
        const allowed = new Set([...(NOTE_REQUIRES[n.tag] ?? []), 'ticker']);
        for (const p of placeholders(rawEntry(n.key)!)) expect(allowed, `${n.tag} quotes {${p}} which NOTE_REQUIRES does not list`).toContain(p);
      }
    }
  });
});

describe('confirm keys', () => {
  it('every literal key of the confirmation sources exists', () => {
    const keys = literalKeys('confirm', sources('../confirm/'));
    expect(keys.length).toBeGreaterThan(100);
    for (const k of keys.filter((k) => !/^confirm\.(order|outkind|stage|action|token|kind|issue\.finding)$/.test(k))) both(k);
  });

  it('kinds, order types, output kinds, stages, actions and issuance findings are translated', () => {
    for (const k of ['create', 'cancel', 'cancelReplace', 'cancelPosition', 'refund', 'send', 'other', 'issue']) {
      both(`confirm.kind.${k}`);
      both(`confirm.kind.${k}Intro`);
    }
    for (const c of ['limit', 'limitDay', 'market', 'marketFok', 'ioc', 'fok', 'twap', 'dca', 'dutch', 'rising', 'stop', 'trailingStop', 'takeProfit', 'oco', 'trailingOco', 'ifd', 'ifo', 'ifdStop', 'ifoStop', 'repeatIfd', 'repeatIfo', 'repeatIfdStop', 'repeatIfoStop']) both(`confirm.order.${c}`);
    for (const k of ['order', 'custody', 'token-change', 'token-out', 'kas-change', 'payment-out', 'unknown-covenant', 'unknown', 'issued']) both(`confirm.outkind.${k}`);
    for (const s of ['signing', 'finalizing', 'validating', 'submitting']) both(`confirm.stage.${s}`);
    for (const a of ['cancel', 'refund', 'fill', 'update', 'other']) both(`confirm.action.${a}`);
    for (const s of ['official', 'verified', 'unverified', 'delisted']) both(`confirm.token.${s}`);
    const src = read(dir('../confirm/issuance-model.ts'));
    const codes = new Set([...src.matchAll(/\b(?:block|warn|info)\('([A-Za-z]+)'/g)].map((m) => m[1]!));
    expect(codes.size).toBeGreaterThanOrEqual(15);
    for (const c of codes) both(`confirm.issue.finding.${c}`);
  });

  it('every signing finding code of the decoder has a translation (issues.*)', () => {
    for (const c of SIGNING_ISSUE_CODES) both(`issues.${c}`);
  });
});
