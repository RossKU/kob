import { tIssue, t, type Params } from '../../i18n';
import { rawOf, withoutRaw } from '../../i18n/build-error';

/** A raw technical text (builder / node / wallet refusal) kept out of the sentence and shown only on request, behind "Details". */
export function RawDetails(props: { text: string | null | undefined; 'data-testid'?: string }) {
  if (!props.text) return null;
  return (
    <details class="raw-details small muted" data-testid={props['data-testid'] ?? 'raw-details'}>
      <summary>{t('common.details')}</summary>
      <code class="mono">{props.text}</code>
    </details>
  );
}

/** One translated finding of the planning layers: a raw builder text inside it is replaced by a plain sentence and kept behind "Details". */
export function IssueLine(props: { prefix: string; issue: { code: string; message: string; params?: Params } }) {
  return (
    <>
      {tIssue(props.prefix, withoutRaw(props.issue))}
      <RawDetails text={rawOf(props.issue)} />
    </>
  );
}
