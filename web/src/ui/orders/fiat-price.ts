// A KAS-per-token price as dollars, for the text cells of the Portfolio's tables. The price is a
// state price, in sompi per whole token of `scale` base units. The Amount component does the same
// for the cells it draws. This module is pure.
import { kasPerTokenToUsd, usdText, type Fiat } from '../../app/fiat';
import { pow10, SOMPI_PER_KAS } from '../../kob/units';

export function priceUsdText(price: bigint, decimals: number, scale: bigint, fiat: Fiat): string {
  return usdText(kasPerTokenToUsd({ num: price * pow10(decimals), den: (scale > 0n ? scale : 1n) * SOMPI_PER_KAS }, fiat.rate));
}
