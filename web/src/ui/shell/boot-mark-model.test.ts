import { describe, expect, it } from 'vitest';
import { CORE, ORBIT, PERIOD, PLANETS, easeInOut, lightAt, nextPlanet, planetAt, tradeCurves } from './boot-mark-model';

const close = (a: number, b: number) => expect(Math.abs(a - b)).toBeLessThan(1e-9);

describe('boot mark', () => {
  it('puts the planets on one orbit, evenly spaced, the first at the top, turning once a period', () => {
    const [x0, y0] = planetAt(0, 0);
    close(x0, 0);
    close(y0, -ORBIT);
    for (let i = 0; i < PLANETS; i++) close(Math.hypot(...planetAt(i, 3.7)), ORBIT);
    const [x2, y2] = planetAt(PLANETS / 4, 0); // a quarter round, clockwise: the right
    close(x2, ORBIT);
    close(y2, 0);
    const [xp, yp] = planetAt(0, PERIOD);
    close(xp, 0);
    close(yp, -ORBIT);
  });

  it('sends a trade to another planet, never back to the one it left', () => {
    let seed = 1;
    const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
    for (let from = 0; from < PLANETS; from++) for (let n = 0; n < 200; n++) {
      const b = nextPlanet(from, rnd);
      expect(b).not.toBe(from);
      expect(b).toBeGreaterThanOrEqual(0);
      expect(b).toBeLessThan(PLANETS);
    }
  });

  it('runs the light from the token to the rim of KAS, then from the rim to the other token', () => {
    const c = tradeCurves(1, 5, 2);
    const start = lightAt(c, 0), mid = lightAt(c, 0.4999), end = lightAt(c, 1);
    close(start[0], planetAt(1, 2)[0]);
    close(end[1], planetAt(5, 2)[1]);
    expect(Math.hypot(...mid)).toBeLessThan(CORE + 4);
    expect(easeInOut(0)).toBe(0);
    expect(easeInOut(1)).toBe(1);
    close(easeInOut(0.5), 0.5);
  });
});
