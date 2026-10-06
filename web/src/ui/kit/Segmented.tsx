import type { ComponentChildren } from 'preact';
import { useRef } from 'preact/hooks';

export interface SegmentedOption<V extends string = string> {
  value: V;
  label: ComponentChildren;
  /** `buy` / `sell` colour the selected segment green / red */
  tone?: 'buy' | 'sell';
  disabled?: boolean;
  'data-testid'?: string;
}

export interface SegmentedProps<V extends string = string> {
  options: SegmentedOption<V>[];
  value: V;
  onChange: (value: V) => void;
  /** accessible name of the group (required: there is no visible label) */
  'aria-label': string;
  block?: boolean;
  disabled?: boolean;
  'data-testid'?: string;
}

/** Radio-group toggle (buy / sell, order type families). Arrow keys move the selection; `data-testid` of each option goes on its button. */
export function Segmented<V extends string = string>(props: SegmentedProps<V>) {
  const ref = useRef<HTMLDivElement>(null);
  const enabled = props.options.filter((o) => !o.disabled);
  const move = (dir: 1 | -1) => {
    const i = enabled.findIndex((o) => o.value === props.value);
    const next = enabled[(i + dir + enabled.length) % enabled.length];
    if (next) {
      props.onChange(next.value);
      // focus follows the selection (roving tabindex)
      queueMicrotask(() => ref.current?.querySelector<HTMLButtonElement>('button[aria-checked="true"]')?.focus());
    }
  };
  return (
    <div
      ref={ref}
      class={`segmented${props.block ? ' block' : ''}`}
      role="radiogroup"
      aria-label={props['aria-label']}
      data-testid={props['data-testid']}
      onKeyDown={(e) => {
        if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
          e.preventDefault();
          move(1);
        } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
          e.preventDefault();
          move(-1);
        }
      }}
    >
      {props.options.map((o) => {
        const checked = o.value === props.value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={checked ? 'true' : 'false'}
            tabIndex={checked ? 0 : -1}
            class={o.tone ? `tone-${o.tone}` : ''}
            disabled={props.disabled || o.disabled}
            data-testid={o['data-testid']}
            onClick={() => props.onChange(o.value)}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
