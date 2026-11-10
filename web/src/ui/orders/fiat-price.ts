// A KAS-per-token price (a state price: sompi per whole token of `scale` base units) as dollars, for the text cells of the Portfolio's tables (the Amount component does the
// same for the cells it draws). Pure.
import { kasPerTokenToUsd, usdText, type Fiat } from '../../app/fiat';
import { pow10, SOMPI_PER_KAS } from '../../kob/units';

export function priceUsdText(price: bigint, decimals: number, scale: bigint, fiat: Fiat): string {
  return usdText(kasPerTokenToUsd({ num: price * pow10(decimals), den: (scale > 0n ? scale : 1n) * SOMPI_PER_KAS }, fiat.rate));
}
