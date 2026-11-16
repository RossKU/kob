// The signing stepper: how far a chain of signatures is (Create token: Prepare -> Create token; Merge all: Merge 1 of n ...), as numbered
// circles joined by a line. A short run sits in a row, a long one in a column. It lives inside the flow's own card or dialog.
import type { ComponentChildren, Ref } from 'preact';
import { t } from '../../i18n';
import { stepGlyph, stepperLayout, type StepperItem } from './stepper-model';
import './stepper.css';

export interface StepperProps {
  steps: readonly StepperItem[];
  /** the list's accessible name */
  label: string;
  layout?: 'row' | 'column';
  /** more content under a step's label (a txid) */
  extra?: (index: number) => ComponentChildren;
  listRef?: Ref<HTMLOListElement>;
  'data-testid'?: string;
}

export function Stepper(props: StepperProps) {
  const layout = props.layout ?? stepperLayout(props.steps.length);
  return (
    <ol role="list" class={`stepper stepper-${layout}`} aria-label={props.label} data-testid={props['data-testid']} ref={props.listRef}>
      {props.steps.map((s, i) => (
        <li
          key={i}
          class={`stepper-step is-${s.state}${s.busy ? ' is-busy' : ''}`}
          data-testid={s.testid}
          data-state={s.data ?? s.state}
          aria-current={s.state === 'current' ? 'step' : undefined}
        >
          <span class="stepper-dot" aria-hidden="true">
            {s.busy ? null : stepGlyph(s, i)}
          </span>
          <span class="stepper-text">
            <span class="stepper-label">{s.label}</span>
            <span class="sr-only">, {t(`stepper.state.${s.state}`)}</span>
            {s.note ? <span class="stepper-note">{s.note}</span> : null}
            {props.extra?.(i)}
          </span>
        </li>
      ))}
    </ol>
  );
}
