// The boot screen's mark (BootMark.tsx): KAS is the sun, eight tokens are planets on one slow orbit, and one trade at a time
// runs as a light from a token into KAS and out to another token; the token it reaches sends the next one (a relay).
// Coordinates are in units where the drawing spans SPAN across, centred on KAS; angle 0 is the top, clockwise.

export const SPAN = 170;
/** radius of the Kaspa icon */
export const CORE = 30;
/** radius of the planets' orbit */
export const ORBIT = 50;
export const PLANETS = 8;
/** seconds per revolution of the planets */
export const PERIOD = 24;
/** seconds one trade takes, token to KAS to token */
export const LEG = 1.05;
/** seconds between a trade landing and the next one leaving */
export const PAUSE = 0.12;

export type Point = [number, number];

/** Where planet `i` is at time `t` (seconds), at radius `r`. */
export function planetAt(i: number, t: number, r = ORBIT): Point {
  const a = (i / PLANETS) * 2 * Math.PI + (t / PERIOD) * 2 * Math.PI;
  return [Math.sin(a) * r, -Math.cos(a) * r];
}

/** The next planet a trade goes to from `from`: any other one, the one straight across less often. `rnd` returns [0, 1). */
export function nextPlanet(from: number, rnd: () => number): number {
  for (;;) {
    const b = Math.floor(rnd() * PLANETS) % PLANETS;
    if (b === from) continue;
    if (Math.abs(b - from) === PLANETS / 2 && rnd() < 0.3) continue;
    return b;
  }
}

export const easeInOut = (x: number): number => (x < 0.5 ? 2 * x * x : 1 - Math.pow(-2 * x + 2, 2) / 2);

const quad = (p0: Point, c: Point, p1: Point, u: number): Point => [
  (1 - u) * (1 - u) * p0[0] + 2 * (1 - u) * u * c[0] + u * u * p1[0],
  (1 - u) * (1 - u) * p0[1] + 2 * (1 - u) * u * c[1] + u * u * p1[1],
];
const BEND = 0.22;
const bent = (a: Point, b: Point): Point => {
  const mx = (a[0] + b[0]) / 2, my = (a[1] + b[1]) / 2, dx = b[0] - a[0], dy = b[1] - a[1];
  return [mx - dy * BEND, my + dx * BEND];
};
const rim = (p: Point): Point => [(p[0] / ORBIT) * (CORE + 3), (p[1] / ORBIT) * (CORE + 3)];

/** The two curves of a trade at time `t`: token `a` to the rim of KAS, and the rim to token `b` (each over u in [0, 1]). */
export function tradeCurves(a: number, b: number, t: number): [(u: number) => Point, (u: number) => Point] {
  const pa = planetAt(a, t), pb = planetAt(b, t), ia = rim(pa), ib = rim(pb);
  const c1 = bent(pa, ia), c2 = bent(ib, pb);
  return [(u) => quad(pa, c1, ia, u), (u) => quad(ib, c2, pb, u)];
}

/** Where the light of a trade is at eased progress `e` in [0, 1] (the first half goes in, the second goes out). */
export function lightAt(curves: [(u: number) => Point, (u: number) => Point], e: number): Point {
  return e < 0.5 ? curves[0](e * 2) : curves[1]((e - 0.5) * 2);
}
