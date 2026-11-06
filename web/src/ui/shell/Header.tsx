import { useServices } from '../../app/context';
import { navOf, routeToHash, type Route } from '../../app/router';
import { t } from '../../i18n';
import { Badge, Button } from '../kit';
import { useTheme } from './theme';
import { NotifyBell } from '../notify/NotificationPanel';
import { WalletButton } from './WalletButton';
import { NetworkChip } from './NetworkBadge';
import { TIGHT_QUERY, useBrandName, useMedia, useNarrow, useSqueezeCheck } from './narrow';
import { useRef } from 'preact/hooks';
import { Popover } from './Popover';
import { StatusDot, StatusLines, useStatusLevel } from './StatusBar';
import { useReorgNotice } from './reorg-notice';
import { useSystemStatus } from './StatusProvider';

const NAV = [
  { name: 'market', href: '#/market', key: 'shell.nav.market', testid: 'nav-market' },
  { name: 'orders', href: '#/orders', key: 'shell.nav.orders', testid: 'nav-orders' },
  { name: 'create', href: '#/create', key: 'shell.nav.create', testid: 'nav-create' },
  { name: 'settings', href: '#/settings', key: 'shell.nav.settings', testid: 'nav-settings' },
] as const;

/** Logo: the favicon's design (public/favicon.svg), fixed brand colours on a dark tile in both themes (not theme variables). */
export function Logo() {
  return (
    <svg class="brand-mark" viewBox="0 0 512 512" aria-hidden="true" focusable="false">
      <rect width="512" height="512" rx="96" fill="#ffffff" fill-opacity="0.6" />
      <rect x="24" y="24" width="464" height="464" rx="78" fill="#0f1d1c" />
      <rect x="44" y="46" width="424" height="43.75" rx="21.875" fill="#d9434b" />
      <rect x="126" y="99.75" width="342" height="43.75" rx="21.875" fill="#e3646a" />
      <rect x="208" y="153.5" width="260" height="43.75" rx="21.875" fill="#ec8a8e" />
      <rect x="290" y="207.25" width="178" height="43.75" rx="21.875" fill="#f4b4b6" />
      <rect x="290" y="261" width="178" height="43.75" rx="21.875" fill="#a6e3c9" />
      <rect x="208" y="314.75" width="260" height="43.75" rx="21.875" fill="#77d0ad" />
      <rect x="126" y="368.5" width="342" height="43.75" rx="21.875" fill="#4bb990" />
      <rect x="44" y="422.25" width="424" height="43.75" rx="21.875" fill="#2c9e77" />
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

/** The three bars of the menu button. */
function MenuIcon() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
      <path d="M4 7h16M4 12h16M4 17h16" />
    </svg>
  );
}

function ThemeToggle(props: { withLabel?: boolean }) {
  const [theme, setTheme] = useTheme();
  const dark = theme === 'dark';
  const label = t(dark ? 'shell.theme.toLight' : 'shell.theme.toDark');
  return (
    <Button small variant="ghost" data-testid="theme-toggle" data-theme={theme} aria-label={label} title={label} onClick={() => setTheme(dark ? 'light' : 'dark')}>
      <ThemeIcon dark={dark} />
      {props.withLabel ? <span>{label}</span> : null}
    </Button>
  );
}

function NavLinks(props: { active: ReturnType<typeof navOf> }) {
  return (
    <>
      {NAV.map((n) => (
        <a key={n.name} href={n.href} data-testid={n.testid} aria-current={props.active === n.name ? 'page' : undefined}>
          {t(n.key)}
        </a>
      ))}
    </>
  );
}

/** The phone menu: the four tabs, the theme toggle, what the network chip and the pre-audit word mean, and the status lines. It closes on every navigation. */
function Menu(props: { active: ReturnType<typeof navOf>; network: string }) {
  return (
    <Popover
      label={t('shell.menu')}
      anchor="header"
      closeOnLink
      triggerClass="menu-button"
      triggerAttrs={{ 'data-testid': 'menu-button' }}
      trigger={<MenuIcon />}
      panelTestId="menu-panel"
    >
      <nav class="menu-nav" aria-label={t('shell.nav.label')}>
        <NavLinks active={props.active} />
      </nav>
      <div class="menu-row">
        <ThemeToggle withLabel />
      </div>
      <p class="small muted menu-note" data-testid="menu-network">
        {t('shell.network.label', { network: props.network })}. {t('preaudit.badgeTip')}
      </p>
      <StatusLines />
    </Popover>
  );
}

/**
 * Top bar, ONE row. The wide row (when it fits): the brand, the four tabs, then the status dot, the network chip, the wallet buttons, the theme toggle and
 * the bell. The narrow row (a phone, or a row that does not fit): the logo, the shortened network chip with the status as a dot in it, one "Connect" button (or
 * the connected address), the bell and, last, a menu button (the tabs, the theme toggle, the network and status explanation).
 */
export function Header(props: { route: Route }) {
  const { config } = useServices();
  const narrow = useNarrow();
  const tight = useMedia(TIGHT_QUERY);
  const level = useStatusLevel();
  const inner = useRef<HTMLDivElement>(null);
  useSqueezeCheck(inner, narrow);
  const brandName = useBrandName(inner, narrow);
  const active = navOf(props.route);
  return (
    <header class="header" data-layout={narrow ? 'narrow' : 'wide'} data-brand={brandName ? 'name' : undefined}>
      <div class="container header-inner" ref={inner}>
        <a class="brand" href={routeToHash({ name: 'home' })} data-testid="brand">
          <Logo />
          <span>{t('common.appName')}</span>
        </a>
        {narrow ? null : (
          <nav class="nav" aria-label={t('shell.nav.label')}>
            <NavLinks active={active} />
          </nav>
        )}
        <div class="header-tools">
          <ReorgIndicator />
          {narrow ? null : <StatusDot />}
          <NetworkChip network={config.network} short={narrow || tight} {...(narrow ? { level } : {})} />
          <div class="header-wallet">
            <WalletButton />
          </div>
          {narrow ? null : <ThemeToggle />}
          <NotifyBell />
          {narrow ? <Menu active={active} network={config.network} /> : null}
        </div>
      </div>
    </header>
  );
}
