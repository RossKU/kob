// The app's hints about an order, worded by the frame itself. Pure.
//
// The app sends codes and numbers only (protocol.ts `SignerNote`, validated by `sanitizeNotes`).
// Every sentence below is fixed text of this bundle, so the app cannot put its own words into the
// frame. The wording follows the app's own final confirmation (ticket.minValue.*,
// issues.PRICE_AGGRESSIVE_VS_MARKET).
import { t } from '../i18n';
import { formatKas, formatUnits } from '../kob/units';
import type { SignerNote } from './protocol';

/** Words the notes as one sentence per note, in the order given. */
export function noteSentences(notes: readonly SignerNote[]): string[] {
  return notes.map((n) =>
    n.kind === 'belowMin'
      ? t(`signer.hint.minValue.${n.source}`, { value: formatKas(BigInt(n.value), { group: ',' }), min: formatKas(BigInt(n.min), { group: ',' }) })
      : n.kind === 'congestion'
        ? t('signer.hint.congestion')
        : n.kind === 'straysLost'
          ? t('signer.hint.straysLost', { utxos: n.utxos })
          : n.kind === 'straysUnchecked'
            ? t('signer.hint.straysUnchecked')
        : t('signer.hint.farThrough', { percent: formatUnits(BigInt(n.bps), 2, { maxFraction: 1 }) }),
  );
}
