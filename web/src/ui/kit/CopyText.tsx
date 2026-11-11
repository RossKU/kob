import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';
import { shortId } from './format';

export interface CopyTextProps {
  /** the full text that is copied (and shown in the tooltip) */
  value: string;
  /** shown text: default is the shortened id; `false` shows the full value */
  short?: boolean;
  /** shown text override (e.g. a shortened address that keeps its network prefix) */
  text?: string;
  head?: number;
  tail?: number;
  /** when set, the shown text is a link to this URL, opened in a new tab (the transaction on the block explorer) */
  href?: string | null;
  /** the whole text, ending with an ellipsis (CSS) where there is no room for it, never shortened in the middle (a transaction id in a list or its details); the title has it whole */
  ellipsis?: boolean;
  /** accessible name of the copy button */
  label?: string;
  'data-testid'?: string;
  class?: string;
}

/** Writes `text` to the clipboard; falls back to a hidden textarea + execCommand where the async API is blocked (http, older browsers). */
export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* fall through to the legacy path */
  }
  const before = typeof document !== 'undefined' ? (document.activeElement as HTMLElement | null) : null;
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    // the shim takes the focus for a moment: a popover that closes when the focus leaves it (shell/Popover.tsx) must not take that for a departure
    ta.setAttribute('data-copy-shim', '');
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand('copy');
    document.body.removeChild(ta);
    before?.focus?.({ preventScroll: true });
    return ok;
  } catch {
    return false;
  }
}

/** Shortened id / address with a copy button. The full value is in `title` and in `data-value` (for tests). */
export function CopyText(props: CopyTextProps) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);
  const shown = props.text ?? (props.short === false || props.ellipsis ? props.value : shortId(props.value, props.head ?? 6, props.tail ?? 6));
  const onCopy = async () => {
    const ok = await copyToClipboard(props.value);
    setState(ok ? 'copied' : 'failed');
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setState('idle'), 1500);
  };
  return (
    <span class={`copy${props.ellipsis ? ' copy-ellipsis' : ''}${props.class ? ` ${props.class}` : ''}`}>
      {props.href ? (
        <a href={props.href} target="_blank" rel="noopener noreferrer" class="explorer-link" title={t('common.explorer')} data-testid={props['data-testid'] ? `${props['data-testid']}-link` : undefined}>
          <code class="mono wrap-anywhere" title={props.value} data-value={props.value} data-testid={props['data-testid']}>{shown}</code>
        </a>
      ) : (
        <code class="mono wrap-anywhere" title={props.value} data-value={props.value} data-testid={props['data-testid']}>{shown}</code>
      )}
      <button type="button" class="copy-btn" onClick={onCopy} aria-label={props.label ?? t('common.copy')} data-state={state}>
        <span class="copy-btn-text">{state === 'copied' ? t('common.copied') : state === 'failed' ? t('common.copyFailed') : t('common.copy')}</span>
        {/* the icon stands in for the words where a table is folded (kit/data-table.css); nowhere else is it drawn */}
        <svg class="copy-btn-icon" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
          {state === 'copied' ? (
            <path d="M3 8.5 6.5 12 13 4.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
          ) : (
            <>
              <rect x="5.5" y="5.5" width="8" height="8.5" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
              <path d="M10.5 3.5v-.5A1.5 1.5 0 0 0 9 1.5H3.5A1.5 1.5 0 0 0 2 3v6.5A1.5 1.5 0 0 0 3.5 11H4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
            </>
          )}
        </svg>
      </button>
    </span>
  );
}
