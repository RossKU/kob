// A covenant id, shortened to 12 + 12 hex (96 bits), with an icon-only copy button. The picture
// beside it carries about 35 bits, and the name and ticker are free to copy, so this is the
// identity. The full id is the tooltip, the `data-value` and what the button copies. The token's
// details card prints the whole id.
import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { copyToClipboard } from '../kit/CopyText';
import { wideId } from '../../kob/registry';

/**
 * A covenant id with its copy button. `compact` is a phone's list row. It shows the 12 + 12 hex id,
 * as wide as the desktop row. A tap on it opens the whole id, wrapped over the lines it needs, and
 * a second tap closes it again. The copy button copies the whole id either way. `expanded` and
 * `onToggle` are the row's, so the row's line can grow with it.
 */
export function IdCopy(props: { value: string; 'data-testid'?: string; compact?: boolean; expanded?: boolean; onToggle?: () => void }) {
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
      {props.compact ? (
        <button type="button" class="ml-id-toggle" aria-expanded={props.expanded ? 'true' : 'false'} aria-label={t(props.expanded ? 'market.id.collapse' : 'market.id.expand')} onClick={props.onToggle} data-testid={props['data-testid'] ? `${props['data-testid']}-toggle` : undefined}>
          <code class="mono" title={props.value} data-value={props.value} data-testid={props['data-testid']}>{props.expanded ? props.value : wideId(props.value)}</code>
        </button>
      ) : (
        <code class="mono" title={props.value} data-value={props.value} data-testid={props['data-testid']}>{wideId(props.value)}</code>
      )}
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
