// A stepped sell curve as a small step line: price against the share sold (kob/curve.ts
// `curveSteps`). The sold part is muted, and the rest carries the sell colour with a light area
// under it. It uses theme tokens only (curve-graph.css).
import type { CurveStep } from '../../kob/curve';
import { stepGeometry } from '../../kob/curve';
import './curve-graph.css';

export interface CurveGraphProps {
  steps: readonly CurveStep[];
  /** The accessible name, for example "Price by share sold". */
  label: string;
  /** Under the graph: the left and right ends of the x axis ("0% sold", "100%"). */
  axis?: [string, string];
  /** The viewBox size. The SVG scales to its container's width. */
  width?: number;
  height?: number;
  'data-testid'?: string;
}

export function CurveGraph(p: CurveGraphProps) {
  const w = p.width ?? 320;
  const h = p.height ?? 96;
  const g = stepGeometry(p.steps, w, h);
  return (
    <figure class="curve-graph" data-testid={p['data-testid']} data-steps={p.steps.length}>
      <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" role="img" aria-label={p.label}>
        <line class="curve-graph-base" x1="0" y1={h - 0.5} x2={w} y2={h - 0.5} />
        {g.area ? <path class="curve-graph-area" d={g.area} /> : null}
        {g.soldPath ? <path class="curve-graph-sold" d={g.soldPath} vector-effect="non-scaling-stroke" /> : null}
        {g.openPath ? <path class="curve-graph-line" d={g.openPath} vector-effect="non-scaling-stroke" /> : null}
        {g.soldX !== null ? <line class="curve-graph-now" x1={g.soldX} y1="0" x2={g.soldX} y2={h} vector-effect="non-scaling-stroke" /> : null}
      </svg>
      {p.axis ? (
        <figcaption class="curve-graph-axis small muted">
          <span>{p.axis[0]}</span>
          <span>{p.axis[1]}</span>
        </figcaption>
      ) : null}
    </figure>
  );
}
