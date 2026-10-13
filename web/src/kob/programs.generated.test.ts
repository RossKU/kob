// The committed programs.generated.ts must be exactly what scripts/gen-programs.mjs renders from the Rust-side profile table.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { OUTPUT_URL, SOURCE_URL, render } from '../../scripts/gen-programs.mjs';
import { CODEC_OF_PROGRAM, PROGRAM_OF_REGISTRY_ID, TOKEN_PROGRAMS, isTokenProgram } from './programs.generated';

const json = JSON.parse(readFileSync(SOURCE_URL, 'utf8')) as { programs: { name: string; registry_id: string; codec: 'kcc20' | 'kron' }[] };

describe('programs.generated.ts', () => {
  it('is up to date with crates/kob-protocol/data/programs.json (run `npm run gen:programs`)', () => {
    const committed = readFileSync(OUTPUT_URL, 'utf8').replace(/\r\n/g, '\n');
    expect(committed, 'programs.generated.ts is stale: run `npm run gen:programs`').toBe(render(json));
  });

  it('lists every program of the table, in order, with its registry id and codec', () => {
    expect(TOKEN_PROGRAMS.map((p) => p.name)).toEqual(json.programs.map((p) => p.name));
    for (const p of json.programs) {
      expect(isTokenProgram(p.name)).toBe(true);
      expect(PROGRAM_OF_REGISTRY_ID[p.registry_id]).toBe(p.name);
      expect(CODEC_OF_PROGRAM[p.name as keyof typeof CODEC_OF_PROGRAM]).toBe(p.codec);
    }
    expect(isTokenProgram('KobAsk')).toBe(false);
    expect(isTokenProgram('toString')).toBe(false);
  });
});
