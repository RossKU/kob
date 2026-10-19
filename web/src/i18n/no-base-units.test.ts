// Founder decision 2026-10-17: "base units" never appears in front of a user. Every amount of a token is said in whole tokens (app/token-display.ts:
// the registry's decimals, this browser's issuance of it, the scale of its orders, else 8 assumed, marked); a raw integer only inside the collapsed
// technical details, labelled as the chain's own amount.
import { describe, expect, it } from 'vitest';
import { allKeys, t, tokenAmountText } from './index';

describe('no "base units" in any English string', () => {
  it('no string of the dictionary says "base unit"', () => {
    const bad = allKeys().filter((k) => /base[ -]?units?/i.test(t(k)));
    expect(bad).toEqual([]);
  });

  it('an amount of a token nothing names is said in whole tokens on the assumed 8 decimals', () => {
    expect(tokenAmountText(null, 4_500_000_000_000n)).toBe('45,000 tokens (decimals assumed 8)');
  });
});

describe('no "base units" in any user-facing message of the code', () => {
  it('no string of the app\'s code (the planners\' and the decoder\'s messages included) says "base unit"; comments may', async () => {
    const { readdirSync, readFileSync, statSync } = await import('node:fs');
    const { join } = await import('node:path');
    const root = join(__dirname, '..');
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const name of readdirSync(dir)) {
        const p = join(dir, name);
        if (statSync(p).isDirectory()) {
          if (name !== 'testing') walk(p);
        } else if (/\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name)) files.push(p);
      }
    };
    walk(root);
    const bad: string[] = [];
    for (const f of files) {
      // the code without its comments: block comments, then line comments (a "//" inside a string is rare enough in this code base)
      const code = readFileSync(f, 'utf8').replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:'"`])\/\/.*$/gm, '$1');
      for (const m of code.matchAll(/base[ -]units?\b/gi)) bad.push(`${f.slice(root.length + 1)}: ${code.slice(Math.max(0, m.index! - 40), m.index! + 20).replace(/\s+/g, ' ')}`);
    }
    expect(bad).toEqual([]);
  });
});
