import type { ComponentChildren } from 'preact';
import { t } from '../../i18n';
import { Button } from './Button';

export type BannerTone = 'info' | 'ok' | 'warn' | 'error';

export interface BannerProps {
  tone?: BannerTone;
  title?: ComponentChildren;
  children?: ComponentChildren;
  /** right-hand actions (e.g. a Retry button) */
  actions?: ComponentChildren;
  /** shows a dismiss button */
  onDismiss?: () => void;
  'data-testid'?: string;
  class?: string;
}

/** Inline message. Errors and warnings use `role=alert` (announced at once), the rest `role=status`. */
export function Banner(props: BannerProps) {
  const tone = props.tone ?? 'info';
  return (
    <div class={`banner banner-${tone}${props.class ? ` ${props.class}` : ''}`} role={tone === 'error' || tone === 'warn' ? 'alert' : 'status'} data-testid={props['data-testid']}>
      <div class="banner-body">
        {props.title ? <div class="banner-title">{props.title}</div> : null}
        {props.children}
      </div>
      {props.actions || props.onDismiss ? (
        <div class="banner-actions">
          {props.actions}
          {props.onDismiss ? (
            <Button small variant="ghost" onClick={props.onDismiss} aria-label={t('common.dismiss')}>
              {'×'}
            </Button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Error banner with a Retry button: the standard failure state of an async view. */
export function ErrorBanner(props: { error: Error | string | null | undefined; title?: string; onRetry?: () => void; 'data-testid'?: string }) {
  if (!props.error) return null;
  const message = typeof props.error === 'string' ? props.error : props.error.message;
  return (
    <Banner
      tone="error"
      title={props.title ?? t('common.error')}
      data-testid={props['data-testid']}
      actions={props.onRetry ? <Button small onClick={props.onRetry}>{t('common.retry')}</Button> : undefined}
    >
      <span class="wrap-anywhere">{message}</span>
    </Banner>
  );
}
