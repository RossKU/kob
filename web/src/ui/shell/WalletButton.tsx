import { useState } from 'preact/hooks';
import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { visibilityCheckSupported } from '../../signer/seen';
import { Badge, Button, CopyText, Modal } from '../kit';
import { isPhoneBrowser } from './platform';

/** The address in the chip as a wallet address reads: its network prefix, then the head and tail of the body ("kaspatest:qq776p…wutfav"). */
export const chipAddress = (address: string): string => {
  const colon = address.indexOf(':');
  const prefix = colon >= 0 ? address.slice(0, colon + 1) : '';
  const body = address.slice(colon + 1);
  return body.length <= 14 ? address : `${prefix}${body.slice(0, 6)}…${body.slice(-6)}`;
};

/** How the wallet is connected, for the chip's wallet icon (never a label of the address: "Private key" next to it reads as a key). */
const connectedWith = (id: string, label: string): string => t('shell.wallet.connectedVia', { wallet: id === 'local-key' ? t('shell.wallet.viaLocalKey') : label });

const KASWARE_URL = 'https://chromewebstore.google.com/detail/kasware-wallet/hklhheigdmpoolooomdihmhlpjjdbklf';
const KASPIRE_URL = 'https://github.com/KaspaHUB21/Kaspire-Kaspa-Wallet';

/**
 * Wallet area of the header. Not connected: two buttons, "Connect wallet" and "Private key" (ConnectBar). Connected: one compact chip with the address
 * (shortened, copyable) as its text, a small wallet icon whose tooltip says how it is connected, the wallet's network ONLY when it differs from the app's
 * (the header's network badge already names the app's), and a disconnect button (an icon on a phone).
 */
export function WalletButton() {
  const w = useWallet();

  if (w.info && w.adapter) {
    return (
      <div class="wallet-chip" data-testid="wallet-connected" data-network={w.info.network} data-wallet={w.info.id}>
        <span class="small">
          <CopyText value={w.info.address} text={chipAddress(w.info.address)} data-testid="wallet-address" label={t('shell.wallet.address')} />
        </span>
        <span class="wallet-kind muted" data-testid="wallet-kind" role="img" aria-label={connectedWith(w.info.id, w.info.label)} title={connectedWith(w.info.id, w.info.label)}>
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M3 7h15a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM3 7l12-3v3M16 13.5h.01" />
          </svg>
        </span>
        {w.networkMismatch ? (
          <Badge tone="bad" data-testid="wallet-network" title={t('shell.wallet.networkOk', { network: w.info.network })}>
            {w.info.network}
          </Badge>
        ) : null}
        <Button small variant="ghost" class="wallet-disconnect" data-testid="wallet-disconnect" onClick={w.disconnect} aria-label={t('shell.wallet.disconnect')} title={t('shell.wallet.disconnect')}>
          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10" />
          </svg>
          <span class="wallet-disconnect-text">{t('shell.wallet.disconnect')}</span>
        </Button>
      </div>
    );
  }

  return <ConnectBar />;
}

/**
 * Not connected: exactly two buttons on one line, "Connect wallet" and "Private key"; nothing else stays in the bar. One detected extension wallet
 * connects at once; several are listed in a dialog; none (or a browser that cannot run the private key signer) says why in a dialog, with the install
 * links. The private key is typed into the sandboxed signer frame (SignerFrame.tsx), never into this page.
 */
function ConnectBar() {
  const { config } = useServices();
  const w = useWallet();
  const [dialog, setDialog] = useState<'wallets' | 'none' | 'key' | null>(null);
  const supported = visibilityCheckSupported();
  const phone = isPhoneBrowser();
  const only = w.detected.length === 1 ? w.detected[0]! : null;

  const onConnect = () => {
    if (only) void w.connect(only.id);
    else setDialog(w.detected.length > 1 ? 'wallets' : 'none');
  };
  // where the browser cannot check what the signer shows the button stays, looks off and explains why when pressed (a disabled button cannot be tapped)
  const onKey = () => {
    if (supported) w.openPrivateKey();
    else setDialog('key');
  };
  // an extension that appears while the "none" dialog is open (they inject late) takes its place
  const shown = dialog === 'none' && w.detected.length > 0 ? 'wallets' : dialog;

  return (
    <div class="wallet-actions" data-testid="wallet-connect-list" data-network={config.network}>
      <Button small variant="primary" data-testid={only ? `wallet-connect-${only.id}` : 'wallet-connect'} loading={w.connecting} onClick={onConnect}>
        {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connectButton')}
      </Button>
      <Button
        small
        data-testid="wallet-connect-local-key"
        disabled={w.connecting}
        data-state={supported ? undefined : 'unavailable'}
        class={supported ? undefined : 'btn-off'}
        onClick={onKey}
        title={supported ? t('signer.recommend') : t('signer.unsupported')}
      >
        {t('signer.option')}
      </Button>
      {shown === 'wallets' ? (
        <Modal title={t('shell.wallet.chooseTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <div class="stack-sm" data-testid="wallet-choose">
            {w.detected.map((a) => (
              <Button key={a.id} variant="primary" block data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => void w.connect(a.id).then(() => setDialog(null))}>
                {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connect', { wallet: a.label })}
              </Button>
            ))}
          </div>
        </Modal>
      ) : null}
      {shown === 'none' ? (
        <Modal title={t('shell.wallet.noneTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <div class="stack-sm" data-testid="wallet-install-hint">
            {/* a phone browser has no extension to install: the install links and "in this browser" would mislead */}
            {phone ? (
              <p data-testid="wallet-install-phone">{t('shell.wallet.nonePhone')}</p>
            ) : (
              <>
                <p>{t('shell.wallet.none')}</p>
                <p>
                  <a href={KASWARE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKasware')}</a>
                  {' / '}
                  <a href={KASPIRE_URL} target="_blank" rel="noopener noreferrer">{t('shell.wallet.installKaspire')}</a>
                </p>
              </>
            )}
            {supported ? (
              <p class="small muted">{t('shell.wallet.noneOrKey')}</p>
            ) : null}
          </div>
        </Modal>
      ) : null}
      {shown === 'key' ? (
        <Modal title={t('signer.unsupportedTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <p data-testid="wallet-key-unsupported">{t(phone ? 'signer.unsupportedWhyPhone' : 'signer.unsupportedWhy')}</p>
        </Modal>
      ) : null}
    </div>
  );
}
