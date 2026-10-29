import { useState } from 'preact/hooks';
import { useWallet } from '../../app/context';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { visibilityCheckSupported } from '../../signer/seen';
import { Badge, Button, CopyText, Modal } from '../kit';
import { isPhoneBrowser } from './platform';
import { useNarrow } from './narrow';

/** The address in the chip as a wallet address reads: its network prefix, then the head and tail of the body ("kaspatest:qq776p…wutfav"). */
export const chipAddress = (address: string, narrow = false): string => {
  const colon = address.indexOf(':');
  const prefix = colon >= 0 ? address.slice(0, colon + 1) : '';
  const body = address.slice(colon + 1);
  // a phone: the head and the tail of the body alone ("qq77…tfav"), the network is the chip's word
  if (narrow) return body.length <= 10 ? body : `${body.slice(0, 4)}…${body.slice(-4)}`;
  return body.length <= 14 ? address : `${prefix}${body.slice(0, 6)}…${body.slice(-6)}`;
};

/** How the wallet is connected, for the chip's wallet icon (never a label of the address: "Private key" next to it reads as a key). */
const connectedWith = (id: string, label: string): string => t('shell.wallet.connectedVia', { wallet: id === 'local-key' ? t('shell.wallet.viaLocalKey') : label });

/**
 * Wallet area of the header. Not connected: one "Connect" button (ConnectBar). Connected: one compact chip with the address
 * (shortened, copyable) as its text, a small wallet icon whose tooltip says how it is connected, the wallet's network ONLY when it differs from the app's
 * (the header's network badge already names the app's), and a disconnect button (an icon on a phone).
 */
export function WalletButton() {
  const w = useWallet();
  const narrow = useNarrow();

  if (w.info && w.adapter) {
    return (
      <div class="wallet-chip" data-testid="wallet-connected" data-network={w.info.network} data-wallet={w.info.id}>
        <span class="small">
          <CopyText value={w.info.address} text={chipAddress(w.info.address, narrow)} data-testid="wallet-address" label={t('shell.wallet.address')} />
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
 * Not connected: ONE button, "Connect", on every width, and no menu in front of it.
 *  - one extension wallet detected (KasWare, Kaspire): it connects to it at once;
 *  - two or more: a minimal pick between those (a dialog with one button each);
 *  - none (every phone browser): it opens the private key entry, the sandboxed signer frame (SignerFrame.tsx), straight away, with its one line of caution
 *    there. There is no "no wallet detected" step. Where the browser cannot run the signer (it cannot check what the signer shows) the button says why.
 * The separate "Private key" button of the header is gone; on a desktop the private key mode is reachable from Settings too. The key is typed into the
 * signer frame, never into this page.
 */
function ConnectBar() {
  const { config } = useServices();
  const w = useWallet();
  const narrow = useNarrow();
  const [dialog, setDialog] = useState<'wallets' | 'key' | null>(null);
  const supported = visibilityCheckSupported();
  const phone = isPhoneBrowser();
  const only = w.detected.length === 1 ? w.detected[0]! : null;
  const noExtension = w.detected.length === 0;

  const connectOne = (id: Parameters<typeof w.connect>[0]) => void w.connect(id).then(() => setDialog(null));
  const onConnect = () => {
    if (only) void w.connect(only.id);
    else if (w.detected.length > 1) setDialog('wallets');
    else if (supported) w.openPrivateKey();
    else setDialog('key');
  };
  // no extension: the button opens the private key entry, and is named for it (the tests and the page's own scripts find it by that name)
  const testid = narrow ? 'wallet-connect' : only ? `wallet-connect-${only.id}` : w.detected.length > 1 ? 'wallet-connect' : 'wallet-connect-local-key';

  return (
    <div class="wallet-actions" data-testid="wallet-connect-list" data-network={config.network}>
      <Button
        small
        variant="primary"
        data-testid={testid}
        loading={w.connecting}
        onClick={onConnect}
        {...(noExtension && !supported ? { 'data-state': 'unavailable', class: 'btn-off', title: t('signer.unsupported') } : {})}
      >
        {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connectShort')}
      </Button>
      {dialog === 'wallets' ? (
        <Modal title={t('shell.wallet.chooseTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <div class="stack-sm" data-testid="wallet-choose">
            {w.detected.map((a) => (
              <Button key={a.id} variant="primary" block data-testid={`wallet-connect-${a.id}`} loading={w.connecting} onClick={() => connectOne(a.id)}>
                {w.connecting ? t('shell.wallet.connecting') : t('shell.wallet.connect', { wallet: a.label })}
              </Button>
            ))}
          </div>
        </Modal>
      ) : null}
      {dialog === 'key' ? (
        <Modal title={t('signer.unsupportedTitle')} onClose={() => setDialog(null)} data-testid="wallet-dialog">
          <p data-testid="wallet-key-unsupported">{t(phone ? 'signer.unsupportedWhyPhone' : 'signer.unsupportedWhy')}</p>
        </Modal>
      ) : null}
    </div>
  );
}
