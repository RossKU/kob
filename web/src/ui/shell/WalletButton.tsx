import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { Badge, Button, CopyText, shortAddress } from '../kit';

const KASWARE_URL = 'https://chromewebstore.google.com/detail/kasware-wallet/hklhheigdmpoolooomdihmhlpjjdbklf';
const KASPIRE_URL = 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet';

/**
 * Wallet area of the header. Not connected: one connect button per DETECTED wallet (extensions inject late, the list updates by itself),
 * or an install hint while none is found. Connected: the address (shortened, copyable), the wallet's network (red when it differs
 * from the app's) and a disconnect button.
 */
export function WalletButton() {
  const { config } = useServices();
  const w = useWallet();

  if (w.info && w.adapter) {
    return (
      <div class="row" data-testid="wallet-connected">
        <span class="small muted">{w.info.label}</span>
        <span title={t('shell.wallet.address')} class="small">
          <CopyText value={w.info.address} text={shortAddress(w.info.address)} data-testid="wallet-address" label={t('shell.wallet.address')} />
        </span>
        <Badge tone={w.networkMismatch ? 'bad' : 'ok'} data-testid="wallet-network" title={t('shell.wallet.networkOk', { network: w.info.network })}>
          {w.info.network}
        </Badge>
        <Button small variant="ghost" data-testid="wallet-disconnect" onClick={w.disconnect}>
          {t('shell.wallet.disconnect')}
        </Button>
      </div>
    );
  }

  if (w.detected.length === 0) {
    return (
      <div class="small muted" data-testid="wallet-install-hint" title={t('shell.wallet.none')}>
        <strong>{t('shell.wallet.noneTitle')}</strong>{' '}
        <a href={KASWARE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKasware')}</a>
        {' / '}
        <a href={KASPIRE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKaspire')}</a>
      </div>
    );
  }

  return (
    <div class="row" data-testid="wallet-connect-list" data-network={config.network}>
      {w.detected.map((a) => (
        <Button key={a.id} small variant="primary" data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => void w.connect(a.id)}>
          {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connect', { wallet: a.label })}
        </Button>
      ))}
    </div>
  );
}
