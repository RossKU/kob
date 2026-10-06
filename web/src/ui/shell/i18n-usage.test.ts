// The dictionary completeness test (i18n.test.ts) checks the dictionary itself; THIS one checks that the UI never asks for a key that does not
// exist: every static `t('area.key')` in the shell / kit / market / orders / settings sources, and every member of the dynamic key families
// (`t(`orders.type.${key}`)` and friends).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { has } from '../../i18n';
import { UNTRADABLE_REASONS } from '../../kob/registry';
import { CANDLE_INTERVALS } from '../../data/indexer';
import { HEALTH_CODES } from './health';
import { ORDER_STATUSES, ORDER_TYPE_KEYS } from '../orders/orders-model';
import { POSITION_PHASES } from '../orders/position-model';
import { KNOWN_POWERS } from '../market/token-model';

const root = fileURLToPath(new URL('../..', import.meta.url));
const AREAS = ['ui/kit', 'ui/shell', 'ui/market', 'ui/orders', 'ui/settings', 'app'];

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((f) => {
    const p = `${dir}/${f}`;
    if (statSync(p).isDirectory()) return files(p);
    return /\.tsx?$/.test(f) && !/\.test\.tsx?$/.test(f) ? [p] : [];
  });
}

const sources = AREAS.flatMap((a) => files(`${root}${a}`));
const KEY = /\bt\(\s*'([a-z]+\.[A-Za-z0-9_.-]+)'/g;

describe('i18n key usage', () => {
  it('scans a meaningful set of sources', () => {
    expect(sources.length).toBeGreaterThan(30);
  });

  it('every static t() key of the UI-1 sources exists', () => {
    const missing: string[] = [];
    for (const f of sources) {
      const text = readFileSync(f, 'utf8');
      for (const m of text.matchAll(KEY)) {
        if (!has(m[1])) missing.push(`${m[1]}  (${f.replace(root, '')})`);
      }
    }
    expect(missing).toEqual([]);
  });

  const family = (prefix: string, members: readonly string[]) => {
    const missing = members.filter((m) => !has(`${prefix}.${m}`)).map((m) => `${prefix}.${m}`);
    expect(missing).toEqual([]);
  };

  it('dynamic families are complete', () => {
    family('orders.type', ORDER_TYPE_KEYS);
    family('market.chart.iv', CANDLE_INTERVALS);
    family('market.chart', ['empty', 'unsupported', 'off']);
    family('market.live', ['live', 'polling', 'off', 'liveHint', 'pollingHint', 'offHint']);
    family('shell.theme', ['toLight', 'toDark']);
    family('orders.status', ORDER_STATUSES);
    family('orders.source', ['node', 'nodeHint', 'record', 'recordHint', 'none', 'noneHint']);
    family('orders.flow.state', ['pending', 'confirming', 'submitted', 'cancelled', 'failed']);
    family('orders.position', ['single', 'ifd', 'repeat']);
    family('orders.position.first', ['buy', 'sell']);
    family('orders.position.phase', POSITION_PHASES);
    family('orders.position.status', ['open', 'partial', 'exits-only', 'waiting', 'closed']);
    family('orders.recover.status', ['live', 'spent', 'unknown', 'failed']);
    family('orders.recover.kind', ['kob-backup', 'indexer-recovery']);
    family('orders.recover.error', ['not-json', 'unknown-format']);
    family('orders.error.snapshot', ['state-unknown', 'no-current-utxo', 'no-extension-commitment', 'bad-state', 'not-live']);
    family('shell.health', HEALTH_CODES);
    family('shell.banner', HEALTH_CODES.filter((c) => !['ok', 'loading', 'not-configured', 'unreachable'].includes(c)));
    family('market.reason', UNTRADABLE_REASONS);
    family('market.badge', ['official', 'verified', 'unverified', 'delisted', 'pending-review', 'untradable', 'notInRegistry', 'issuerControl', 'officialHint', 'unverifiedHint']);
    family('market.state', ['official', 'verified', 'unverified', 'delisted']);
    family('market.power', KNOWN_POWERS);
    family('market.open', ['program-unknown', 'no-scale', 'no-extension', 'scale-invalid']);
    family('market.live', ['live', 'polling', 'off', 'liveHint', 'pollingHint', 'offHint']);
    family('market.trades', ['buy', 'sell']);
    family('market.tpl.problem', ['family-unsupported', 'program-unknown', 'hash-mismatch', 'prefix-mismatch', 'suffix-mismatch', 'state-len-mismatch', 'slots-mismatch']);
    family('market.indexerProblems', ['covenant-id-mismatch', 'template-hash-mismatch', 'extension-mismatch', 'decimals-mismatch', 'scale-mismatch']);
    family('settings.error', ['network', 'url', 'registry']);
    family('settings.local.kind', ['settings', 'records', 'tokens', 'other']);
  });

  it('every planner / amend finding code the orders UI can show has a translation', () => {
    // codes of kob/cancel.ts (prefix `orders.issue`) and of orders/amend.ts (prefix `orders.amend`)
    family('orders.issue', [
      'cancel.funding-added', 'cancel.stray-other-extension', 'cancel.strays-abandoned', 'cancel.replacement-restarts-unarmed', 'cancel.top-up', 'refund.strays-stay',
      'cancel.not-maker', 'cancel.custody-missing', 'cancel.strays-exceed-slots', 'cancel.build-failed', 'cancel.insufficient-funds', 'cancel.replacement-wrong-maker',
      'cancel.replacement-wrong-token', 'cancel.insufficient-tokens', 'cancel.nothing', 'refund.no-clock', 'refund.not-yet',
    ]);
    const amendSource = readFileSync(`${root}ui/orders/amend.ts`, 'utf8');
    const codes = [...amendSource.matchAll(/issue\('([a-z-]+)'/g)].map((m) => m[1]);
    expect(codes.length).toBeGreaterThan(10);
    family('orders.amend', codes);
    const cancelSource = readFileSync(`${root}kob/cancel.ts`, 'utf8');
    const planned = [...cancelSource.matchAll(/issue\('((?:cancel|refund)\.[a-z-]+)'/g)].map((m) => m[1]);
    family('orders.issue', [...new Set(planned)]);
  });
});
