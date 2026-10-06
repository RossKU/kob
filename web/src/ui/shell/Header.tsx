import { useServices } from '../../app/context';
import { navOf, type Route } from '../../app/router';
import { t } from '../../i18n';
import { Badge, Button } from '../kit';
import { useTheme } from './theme';
import { NotifyBell } from '../notify/NotificationPanel';
import { WalletButton } from './WalletButton';
import { NetworkBadge } from './NetworkBadge';
import { useReorgNotice } from './reorg-notice';
import { useSystemStatus } from './StatusProvider';

const NAV = [
  { name: 'market', href: '#/market', key: 'shell.nav.market', testid: 'nav-market' },
  { name: 'orders', href: '#/orders', key: 'shell.nav.orders', testid: 'nav-orders' },
  { name: 'issue', href: '#/issue', key: 'shell.nav.issue', testid: 'nav-issue' },
  { name: 'settings', href: '#/settings', key: 'shell.nav.settings', testid: 'nav-settings' },
] as const;

/** Logo (order-book bars), a mark that follows the theme through `currentColor`. */
function Logo() {
  return (
    <svg class="brand-mark" viewBox="0 0 32 32" aria-hidden="true" focusable="false">
      <rect x="1" y="1" width="30" height="30" rx="7" fill="none" stroke="currentColor" stroke-width="2" />
      <rect x="6" y="7" width="12" height="4" rx="1" fill="var(--sell)" />
      <rect x="6" y="13" width="18" height="4" rx="1" fill="var(--sell)" opacity="0.65" />
      <rect x="6" y="19" width="20" height="4" rx="1" fill="var(--buy)" opacity="0.65" />
      <rect x="6" y="25" width="14" height="3" rx="1" fill="var(--buy)" />
    </svg>
  );
}

/** Sun (switch to light) / moon (switch to dark) icon of the theme toggle. */
function ThemeIcon(props: { dark: boolean }) {
  return props.dark ? (
    <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
      <circle cx="12" cy="12" r="4.5" />
      <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
    </svg>
  ) : (
    <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round">
      <path d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11z" />
    </svg>
  );
}

/** Subtle mark next to the network badge after a DEEP chain re-organisation (a routine shallow one shows nothing); the tooltip says what happened. */
function ReorgIndicator() {
  const { deepReorgs } = useSystemStatus();
  const shown = useReorgNotice(deepReorgs.count);
  if (!shown) return null;
  const tip = t('shell.reorg.deepTip', { blocks: deepReorgs.blocks });
  return (
    <Badge tone="warn" title={tip} data-testid="reorg-indicator">
      {t('shell.reorg.deep')}
    </Badge>
  );
}

/** Top bar: brand, navigation, network badge, theme toggle, wallet area. */
export function Header(props: { route: Route }) {
  const { config } = useServices();
  const [theme, setTheme] = useTheme();
  const dark = theme === 'dark';
  const active = navOf(props.route);
  return (
    <header class="header">
      <div class="container header-inner">
        <a class="brand" href="#/market" data-testid="brand">
          <Logo />
          <span>{t('common.appName')}</span>
        </a>
        <nav class="nav" aria-label={t('shell.nav.label')}>
          {NAV.map((n) => (
            <a key={n.name} href={n.href} data-testid={n.testid} aria-current={active === n.name ? 'page' : undefined}>
              {t(n.key)}
            </a>
          ))}
        </nav>
        <div class="header-tools">
          <ReorgIndicator />
          <NetworkBadge network={config.network} />
          <Button
            small
            variant="ghost"
            data-testid="theme-toggle"
            data-theme={theme}
            aria-label={t(dark ? 'shell.theme.toLight' : 'shell.theme.toDark')}
            title={t(dark ? 'shell.theme.toLight' : 'shell.theme.toDark')}
            onClick={() => setTheme(dark ? 'light' : 'dark')}
          >
            <ThemeIcon dark={dark} />
          </Button>
          <NotifyBell />
          <WalletButton />
        </div>
      </div>
    </header>
  );
}
