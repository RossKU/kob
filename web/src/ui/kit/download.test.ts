import { describe, expect, it } from 'vitest';
import { exportFileName, jsonText } from './download';

describe('download helpers', () => {
  it('names export files from a fixed vocabulary and the UTC date', () => {
    expect(exportFileName('backup', 'testnet-10', Date.UTC(2026, 8, 9, 23, 59))).toBe('kob-backup-testnet-10-20260909.json');
    expect(exportFileName('Maker Recovery!', 'main net', Date.UTC(2026, 0, 2))).toBe('kob-maker-recovery-main-net-20260102.json');
    expect(exportFileName('///', '', Date.UTC(2026, 0, 1))).toBe('kob-x-x-20260101.json');
  });

  it('serialises bigint amounts as exact decimal strings', () => {
    const text = jsonText({ amount: 123_456_789_012_345_678_901n, nested: [1n] });
    expect(JSON.parse(text)).toEqual({ amount: '123456789012345678901', nested: ['1'] });
  });
});
