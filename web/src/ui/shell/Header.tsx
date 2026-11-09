import { useServices } from '../../app/context';
import { navOf, routeToHash, type Route } from '../../app/router';
import type { ConfigWarning, StoredOverride } from '../../config';
import { t } from '../../i18n';
import { Badge } from '../kit';
import { Brand, SiteHeader, ThemeToggle } from './Frame';
import { NotifyBell } from '../notify/NotificationPanel';
import { WalletButton } from './WalletButton';
import { NetworkChip } from './NetworkBadge';
import { TIGHT_QUERY, useHeaderLayout, useMedia, useNarrow, useSqueezeCheck } from './narrow';
import { useRef } from 'preact/hooks';
import { Popover } from './Popover';
import { StatusDot, StatusLines, useStatusLevel } from './StatusBar';
import { useReorgNotice } from './reorg-notice';
import { useSystemStatus } from './StatusProvider';

const NAV = [
  { name: 'market', href: '#/market', key: 'shell.nav.market', testid: 'nav-market' },
  { name: 'activity', href: '#/activity', key: 'shell.nav.activity', testid: 'nav-activity' },
  { name: 'portfolio', href: '#/portfolio', key: 'shell.nav.portfolio', testid: 'nav-portfolio' },
  { name: 'create', href: '#/create', key: 'shell.nav.create', testid: 'nav-create' },
  { name: 'settings', href: '#/settings', key: 'shell.nav.settings', testid: 'nav-settings' },
] as const;

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

/** The phone menu: the four tabs, the theme toggle, what the network chip and the words open review mean, and the status lines. It closes on every navigation. */
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
export function Header(props: { route: Route; warnings: ConfigWarning[]; storedOverrides: StoredOverride[] }) {
  const { config } = useServices();
  const narrow = useNarrow();
  const tight = useMedia(TIGHT_QUERY);
  const level = useStatusLevel();
  const inner = useRef<HTMLDivElement>(null);
  useSqueezeCheck(inner, narrow);
  const { brandName, centred, disconnectText } = useHeaderLayout(inner, narrow);
  const active = navOf(props.route);
  return (
    <SiteHeader
      context="app"
      layout={narrow ? 'narrow' : 'wide'}
      brandName={brandName}
      disconnectText={disconnectText}
      centred={centred}
      innerRef={inner}
      brand={<Brand href={routeToHash({ name: 'home' })} testid="brand" />}
    >
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
        <NotifyBell warnings={props.warnings} storedOverrides={props.storedOverrides} />
        {narrow ? <Menu active={active} network={config.network} /> : null}
      </div>
    </SiteHeader>
  );
}
