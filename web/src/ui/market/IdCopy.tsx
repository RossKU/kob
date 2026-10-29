// A covenant id, shortened (12 + 12 hex, 96 bits: the picture beside it carries about 35 bits and the name and ticker are free to copy, so this is the identity) with an icon-only copy
// button. The full id is the tooltip, the `data-value` and what the button copies.
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { copyToClipboard } from '../kit/CopyText';
import { wideId } from '../../kob/registry';

export function IdCopy(props: { value: string; 'data-testid'?: string }) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);
  const onCopy = async () => {
    setState((await copyToClipboard(props.value)) ? 'copied' : 'failed');
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setState('idle'), 1500);
  };
  const label = state === 'copied' ? t('common.copied') : state === 'failed' ? t('common.copyFailed') : t('market.id.copy');
  return (
    <span class="ml-id">
      <code class="mono" title={props.value} data-value={props.value} data-testid={props['data-testid']}>{wideId(props.value)}</code>
      <button type="button" class="copy-btn ml-copy" onClick={onCopy} aria-label={label} title={label} data-state={state} data-testid={props['data-testid'] ? `${props['data-testid']}-copy` : undefined}>
        {state === 'copied' ? (
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false"><path d="M3 8.5 6.5 12 13 4.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg>
        ) : (
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
            <rect x="5.5" y="5.5" width="8" height="8.5" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
            <path d="M10.5 3.5v-.5A1.5 1.5 0 0 0 9 1.5H3.5A1.5 1.5 0 0 0 2 3v6.5A1.5 1.5 0 0 0 3.5 11H4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
        )}
      </button>
    </span>
  );
}
