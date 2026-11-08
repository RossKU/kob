// The boot screen's animated mark (boot-mark-model.ts): the Kaspa icon with eight tokens orbiting it and a relay of trades through KAS.
// Decorative (aria-hidden): the boot screen's text says what is happening. With reduced motion it draws one still frame.
import { useEffect, useRef } from 'preact/hooks';
import { CORE, LEG, ORBIT, PAUSE, PLANETS, SPAN, easeInOut, lightAt, nextPlanet, planetAt, tradeCurves } from './boot-mark-model';
import { KASPA_ICON_INNER, KASPA_ICON_PATH, KASPA_ICON_RADIUS } from './kaspa-icon';

const SIZE = 132; // css px
const TAU = Math.PI * 2;
const COLORS = ['kaspa', 'kaspa-in', 'planet', 'orbit', 'trail', 'buy'] as const;
type Colors = Record<(typeof COLORS)[number], string>;

function readColors(): Colors {
  const s = getComputedStyle(document.documentElement);
  return Object.fromEntries(COLORS.map((k) => [k, s.getPropertyValue(`--boot-${k}`).trim() || '#70c7ba'])) as Colors;
}

function withAlpha(color: string, a: number): string {
  const m = /^#([0-9a-f]{6})$/i.exec(color);
  if (!m) return color;
  const n = parseInt(m[1], 16);
  return `rgba(${n >> 16},${(n >> 8) & 255},${n & 255},${a})`;
}

interface Trade { a: number; b: number; t0: number; landed: boolean; beaten: boolean }

export function BootMark() {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const cv = ref.current;
    const g = cv?.getContext('2d');
    if (!cv || !g || typeof Path2D === 'undefined') return;
    const dpr = Math.min(window.devicePixelRatio || 1, 3);
    cv.width = SIZE * dpr;
    cv.height = SIZE * dpr;
    const icon = new Path2D(KASPA_ICON_PATH);
    const still = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
    let colors = readColors();
    const themeWatch = new MutationObserver(() => (colors = readColors()));
    themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

    const buy = new Array<number>(PLANETS).fill(0);
    const kick = new Array<number>(PLANETS).fill(0);
    let trades: Trade[] = [];
    let beat = 0, next = 0.4, from = Math.floor(Math.random() * PLANETS), last = 0, frame = 0;
    const t0 = performance.now();

    const draw = (t: number) => {
      const dt = Math.min(0.05, Math.max(0, t - last));
      last = t;
      if (!still && t >= next) {
        trades.push({ a: from, b: nextPlanet(from, Math.random), t0: t, landed: false, beaten: false });
        kick[from] = 1;
        next = Infinity; // the next leg leaves when this one lands
      }
      for (let i = 0; i < PLANETS; i++) {
        buy[i] = Math.max(0, buy[i] - dt * 1.1);
        kick[i] = Math.max(0, kick[i] - dt * 2.5);
      }
      beat = Math.max(0, beat - dt * 2.6);

      g.setTransform((dpr * SIZE) / SPAN, 0, 0, (dpr * SIZE) / SPAN, (SIZE * dpr) / 2, (SIZE * dpr) / 2);
      g.clearRect(-SPAN / 2, -SPAN / 2, SPAN, SPAN);
      g.strokeStyle = colors.orbit;
      g.lineWidth = 1;
      g.beginPath();
      g.arc(0, 0, ORBIT, 0, TAU);
      g.stroke();

      trades = trades.filter((tr) => {
        const curves = tradeCurves(tr.a, tr.b, t);
        const u = Math.min(1, (t - tr.t0) / LEG), e = easeInOut(u);
        if (!tr.beaten && e >= 0.5) { tr.beaten = true; beat = 1; }
        if (u >= 1 && !tr.landed) { tr.landed = true; buy[tr.b] = 1; from = tr.b; next = t + PAUSE; }
        const fade = 1 - Math.max(0, (t - tr.t0 - LEG) / 0.6);
        g.lineWidth = 1.4;
        g.lineCap = 'round';
        g.strokeStyle = withAlpha(colors.trail, 0.4 * fade);
        const reach = [Math.min(1, e * 2), Math.max(0, e * 2 - 1)];
        curves.forEach((c, s) => {
          if (reach[s] <= 0) return;
          g.beginPath();
          for (let i = 0; i <= 16; i++) { const [x, y] = c((i / 16) * reach[s]); if (i) g.lineTo(x, y); else g.moveTo(x, y); }
          g.stroke();
        });
        if (!tr.landed) for (let k = 6; k >= 0; k--) {
          const [x, y] = lightAt(curves, Math.max(0, e - k * 0.02));
          g.fillStyle = withAlpha(colors.trail, 1 - k / 7);
          g.beginPath();
          g.arc(x, y, 2.9 * (1 - k / 9), 0, TAU);
          g.fill();
        }
        return fade > 0;
      });

      for (let i = 0; i < PLANETS; i++) {
        const [x, y] = planetAt(i, t), s = 4.4 + buy[i] * 1.2 - kick[i] * 0.7;
        if (buy[i] > 0) {
          g.strokeStyle = withAlpha(colors.buy, buy[i] * 0.9);
          g.lineWidth = 1.6;
          g.beginPath();
          g.arc(x, y, s + 2.5 + (1 - buy[i]) * 5, 0, TAU);
          g.stroke();
        }
        g.fillStyle = buy[i] > 0.02 ? colors.buy : colors.planet;
        g.beginPath();
        g.arc(x, y, s, 0, TAU);
        g.fill();
      }

      if (beat > 0) {
        g.strokeStyle = withAlpha(colors.kaspa, beat * 0.55);
        g.lineWidth = 1.6;
        g.beginPath();
        g.arc(0, 0, CORE + 2 + (1 - beat) * 12, 0, TAU);
        g.stroke();
      }
      const k = (CORE / KASPA_ICON_RADIUS) * (1 + (beat > 0 ? 0.07 * Math.sin((1 - beat) * Math.PI) : 0));
      g.save();
      g.scale(k, k);
      g.translate(-KASPA_ICON_INNER.cx, -KASPA_ICON_INNER.cy);
      g.fillStyle = colors['kaspa-in'];
      g.beginPath();
      g.arc(KASPA_ICON_INNER.cx, KASPA_ICON_INNER.cy, KASPA_ICON_INNER.r, 0, TAU);
      g.fill();
      g.fillStyle = colors.kaspa;
      g.fill(icon);
      g.restore();
    };

    if (still) draw(0);
    else {
      const loop = (now: number) => { draw((now - t0) / 1000); frame = requestAnimationFrame(loop); };
      frame = requestAnimationFrame(loop);
    }
    return () => { cancelAnimationFrame(frame); themeWatch.disconnect(); };
  }, []);
  return <canvas ref={ref} class="boot-mark" width={SIZE} height={SIZE} aria-hidden="true" data-testid="boot-mark" />;
}
