import type { ComponentChildren } from 'preact';

export type EmptyIcon = 'wallet' | 'list' | 'chart' | 'token' | 'plug';

const PATHS: Record<EmptyIcon, string> = {
  wallet: 'M4 7a2 2 0 0 1 2-2h11v3M4 7v10a2 2 0 0 0 2 2h13a1 1 0 0 0 1-1v-9a1 1 0 0 0-1-1H6a2 2 0 0 1-2-2zM16 13.5h.01',
  list: 'M5 6h14M5 12h14M5 18h9',
  chart: 'M4 19V5M4 19h16M8 15l3-4 3 2 4-6',
  token: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM9 12h6M12 9v6',
  plug: 'M9 3v5M15 3v5M7 8h10v3a5 5 0 0 1-10 0V8zM12 16v5',
};

export interface EmptyStateProps {
  /** a small line icon above the sentence (omitted: none) */
  icon?: EmptyIcon;
  /** the ONE sentence: what is not there and why (or what to do) */
  children: ComponentChildren;
  /** the primary action, when there is one (a Button) */
  action?: ComponentChildren;
  /** tight padding, for a panel that only holds this (the market page's orders panel) */
  compact?: boolean;
  class?: string;
  'data-testid'?: string;
}

/**
 * The one empty / disconnected state of the app (review M-12): an icon, one sentence, one primary action, centred on the same footprint everywhere (Portfolio, Activity, Create
 * token, the orders panel). `role=status` so a screen reader announces a state that appears after a load.
 */
export function EmptyState(props: EmptyStateProps) {
  return (
    <div class={`empty-state${props.compact ? ' empty-state-compact' : ''}${props.class ? ` ${props.class}` : ''}`} role="status" data-testid={props['data-testid']}>
      {props.icon ? (
        <svg class="empty-state-icon" viewBox="0 0 24 24" width="28" height="28" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
          <path d={PATHS[props.icon]} />
        </svg>
      ) : null}
      <p class="empty-state-text">{props.children}</p>
      {props.action ? <div class="empty-state-action">{props.action}</div> : null}
    </div>
  );
}
