// A raw failure text (a server string, a fetch error, a wallet refusal) is for the Details fold, never the headline: this picks the plain sentence that goes first.
import { t } from '../../i18n';

/** The one sentence a viewer reads for a raw failure message: what failed in words, from its shape. The raw text stays available behind "Details". */
export function friendlyErrorText(raw: string | null | undefined): string {
  const m = raw ?? '';
  if (/indexer|\/v1\/|\bHTTP\b|status\s*\d{3}|\b[45]\d\d\b/i.test(m)) return t('common.error.indexer');
  if (/failed to fetch|networkerror|network error|load failed|timed? ?out|timeout|aborted|unreachable|econn|offline|websocket|node/i.test(m)) return t('common.error.network');
  if (/reject|denied|declined|cancel|user/i.test(m)) return t('common.error.refused');
  return t('common.error.generic');
}
