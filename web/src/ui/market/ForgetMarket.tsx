import { useEffect } from 'preact/hooks';
import { useServices } from '../../app/context';
import { clearLastMarket } from '../../app/last-market';

/** Rendered by a market page that found nothing (unknown token or pair): the header's Trade tab must not lead back to it. Draws nothing. */
export function ForgetMarket() {
  const { config } = useServices();
  useEffect(() => clearLastMarket(config.network), [config.network]);
  return null;
}
