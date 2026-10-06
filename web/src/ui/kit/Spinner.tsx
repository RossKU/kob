import { t } from '../../i18n';

export interface SpinnerProps {
  large?: boolean;
  /** accessible name (default: the translated "Loading...") */
  label?: string;
  'data-testid'?: string;
}

/** Inline busy indicator; `role=status` so screen readers announce it once. */
export function Spinner(props: SpinnerProps) {
  return <span class={`spinner${props.large ? ' spinner-lg' : ''}`} role="status" aria-label={props.label ?? t('common.loading')} data-testid={props['data-testid']} />;
}

/** A spinner with a text next to it, for "loading ..." rows. */
export function Loading(props: { text?: string; 'data-testid'?: string }) {
  return (
    <div class="loading-row" data-testid={props['data-testid']}>
      <Spinner />
      <span>{props.text ?? t('common.loading')}</span>
    </div>
  );
}
