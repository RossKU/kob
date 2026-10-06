import type { ComponentChildren } from 'preact';

export type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

export interface BadgeProps {
  tone?: Tone;
  children?: ComponentChildren;
  /** tooltip (plain text) */
  title?: string;
  'data-testid'?: string;
  class?: string;
}

/** Small status pill. The tone is decorative: the text always carries the meaning (colour is never the only signal). */
export function Badge(props: BadgeProps) {
  return (
    <span class={`badge badge-${props.tone ?? 'neutral'}${props.class ? ` ${props.class}` : ''}`} title={props.title} data-testid={props['data-testid']}>
      {props.children}
    </span>
  );
}
