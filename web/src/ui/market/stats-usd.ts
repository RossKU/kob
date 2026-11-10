// The market page's 24 h strip in USD (Settings, Display currency; app/fiat.ts): the last price, the 24 h high and low and the 24 h volume in KAS become dollars, as the
// strip of a token's KAS market (TOKEN/KAS, not flipped) reads them. The change, the spread, the trade count and the volume in the token stay as they are (ratios and
// token units). Each converted tile keeps its KAS figure in its tooltip, with the rate used. The order book, the chart and the ticket are not touched.
import { kasPerTokenToUsd, rateTitle, sompiToUsd, usdText, usdTextFit, type Fiat } from '../../app/fiat';
import { t } from '../../i18n';
import type { StatsView } from '../../data/indexer-types';
import { kasPerToken } from './orientation';
import { basisOf, bigOrNull, type StatsModel } from './market-model';

/** The last trade as the tape reads it (price in sompi per `basis` base units): the strip's last price when the stats have none. */
export interface TapeLast {
  price: bigint;
  basis: bigint;
}

export interface UsdStats {
  model: StatsModel;
  /** tooltips of the converted tiles: the KAS figure and the rate used */
  lastTitle: string | null;
  titles: { high: string | null; low: string | null; quoteVolume: string | null };
}

/** Characters of the volume tile before it turns compact (`$56.61M`). */
const VOLUME_CHARS = 10;

/**
 * `base` (the KAS strip, `statsModel`) with its KAS tiles in dollars. `ticker` names the token for the tooltips ("0.0123 KAS per TICKER"). A tile with no figure stays
 * empty. `tapeLast`: the last trade of the tape, used for the last price only when the stats carry none.
 */
export function usdStatsModel(base: StatsModel, v: StatsView | null, decimals: number, fiat: Fiat, ticker: string, tapeLast: TapeLast | null = null): UsdStats {
  const basis = v ? basisOf(v.price_basis) : null;
  const usdPrice = (price: bigint | null, b: bigint | null): string | null => (price !== null && price > 0n && b ? usdText(kasPerTokenToUsd(kasPerToken(price, decimals, b), fiat.rate)) : null);
  const unit = t('market.stats.unitOf', { quote: 'KAS', base: ticker });
  const rate = rateTitle(fiat);
  const kasTitle = (kasText: string | null): string | null => (kasText ? `${kasText} ${unit} · ${rate}` : null);

  const statsLast = v && basis ? bigOrNull(v.last) : null;
  const last = statsLast !== null ? usdPrice(statsLast, basis) : tapeLast ? usdPrice(tapeLast.price, tapeLast.basis) : null;
  const high = usdPrice(v ? bigOrNull(v.high_24h) : null, basis);
  const low = usdPrice(v ? bigOrNull(v.low_24h) : null, basis);
  const qv = v ? bigOrNull(v.quote_volume_24h) : null;
  const volume = qv === null ? null : sompiToUsd(qv, fiat.rate);
  return {
    model: {
      ...base,
      last,
      high,
      low,
      quoteVolume: volume ? usdTextFit(volume, VOLUME_CHARS) : null,
      quoteVolumeExact: volume ? usdText(volume) : null,
    },
    lastTitle: last ? kasTitle(base.last) : null,
    titles: {
      high: high ? kasTitle(base.high) : null,
      low: low ? kasTitle(base.low) : null,
      quoteVolume: base.quoteVolumeExact ? `${base.quoteVolumeExact} KAS · ${rate}` : null,
    },
  };
}
