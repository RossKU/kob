// The frame shared by the landing page and the app: one header row (SiteHeader) and one footer (SiteFooter), the brand lockup, the theme toggle and the
// skip link. Only the header's contents differ by page (the landing page: GitHub, the network chip, "Open the app", the theme toggle; the app: Header.tsx's
// tabs and tools). Its height, column, brand and controls are the same on both (styles.css, "frame"), so the logo stays put when "Open the app" is clicked.
import type { ComponentChildren, Ref } from 'preact';
import { useEffect, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { version as appVersion } from '../../../package.json';
import { indexerTrust } from '../../data/indexer-crosscheck';
import { Button } from '../kit';
import { landingNetworkText, siteName } from '../landing/landing-model';
import { supportAddressOf } from '../support/support-model';
import { footerLinks, showsChartNotice, type FrameContext } from './frame-model';
import { useFailoverState } from './failover';
import { SECURITY_POLICY_URL } from './preaudit';
import { useTheme } from './theme';

/** Logo: the favicon's design (public/favicon.svg), fixed brand colours on a dark tile in both themes (not theme variables). */
export function Logo() {
  return (
    <svg class="brand-mark" viewBox="0 0 512 512" aria-hidden="true" focusable="false">
      <rect width="512" height="512" rx="96" fill="#ffffff" fill-opacity="0.6" />
      <rect x="10" y="10" width="492" height="492" rx="87" fill="#0f1d1c" />
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

/** GitHub's mark (the landing page's header and its GitHub buttons). */
export function GitHubIcon(props: { class?: string }) {
  return (
    <svg class={props.class} width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        fill="currentColor"
        d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"
      />
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

/** The theme toggle: an icon button in the header row, an icon and its label in the phone menu. */
export function ThemeToggle(props: { withLabel?: boolean }) {
  const [theme, setTheme] = useTheme();
  const dark = theme === 'dark';
  const label = t(dark ? 'shell.theme.toLight' : 'shell.theme.toDark');
  return (
    <Button
      small
      variant="ghost"
      class={props.withLabel ? undefined : 'btn-icon'}
      data-testid="theme-toggle"
      data-theme={theme}
      aria-label={label}
      title={label}
      onClick={() => setTheme(dark ? 'light' : 'dark')}
    >
      <ThemeIcon dark={dark} />
      {props.withLabel ? <span>{label}</span> : null}
    </Button>
  );
}

/**
 * The brand lockup: the logo and the name. Where the row has no room for the name, styles.css clips it to 1 px (still read by screen readers, and measured
 * by narrow.ts), never `display: none`. `label` replaces the accessible name (the landing page's "KOB, back to the top").
 */
export function Brand(props: { testid: string; href?: string; onClick?: (e: Event) => void; label?: string; class?: string }) {
  return (
    <a class={props.class ? `brand ${props.class}` : 'brand'} href={props.href ?? '#/'} onClick={props.onClick} aria-label={props.label} data-testid={props.testid}>
      <Logo />
      <span>{t('common.appName')}</span>
    </a>
  );
}

/** "Skip to content": off screen until it has the keyboard focus; a hash link would be a route here, so it moves the focus itself. */
export function SkipLink() {
  return (
    <a
      class="skip-link"
      href="#/"
      data-testid="skip-link"
      onClick={(e) => {
        e.preventDefault();
        document.getElementById('main')?.focus();
      }}
    >
      {t('shell.skip')}
    </a>
  );
}

/** True once the page is scrolled more than `px`: the header gets its blurred bar (see-through at the top of the page). */
export function useScrolled(px = 8): boolean {
  const [scrolled, setScrolled] = useState(() => typeof window !== 'undefined' && window.scrollY > px);
  useEffect(() => {
    const on = () => setScrolled(window.scrollY > px);
    window.addEventListener('scroll', on, { passive: true });
    on();
    return () => window.removeEventListener('scroll', on);
  }, [px]);
  return scrolled;
}

/** The header row of every page: the brand first, then the page's own contents (`children`). See-through at the top, a blurred bar once scrolled. */
export function SiteHeader(props: {
  context: FrameContext;
  brand: ComponentChildren;
  children: ComponentChildren;
  innerRef?: Ref<HTMLDivElement>;
  layout?: 'narrow' | 'wide';
  brandName?: boolean;
  /** the wide row has room for the word beside the wallet's disconnect icon (narrow.ts) */
  disconnectText?: boolean;
  /** the app's wide row with its tabs in the true centre (narrow.ts useHeaderLayout) */
  centred?: boolean;
  testid?: string;
}) {
  const scrolled = useScrolled();
  return (
    <header
      class="header"
      data-context={props.context}
      data-scrolled={scrolled ? '' : undefined}
      data-layout={props.layout}
      data-brand={props.brandName ? 'name' : undefined}
      data-disconnect={props.disconnectText ? 'text' : undefined}
      data-centred={props.centred ? '' : undefined}
      data-testid={props.testid}
    >
      <div class="container header-inner" ref={props.innerRef}>
        {props.brand}
        {props.children}
      </div>
    </header>
  );
}

/** A version worth showing: a real release number, not the 0.0.0 placeholder of an unreleased build (nor an unknown one). */
export const releasedVersion = (v: string): boolean => /^\d+\.\d+\.\d+/.test(v) && !/^0\.0\.0(?![.\d])/.test(v);

const EXTERNAL = { target: '_blank', rel: 'noopener noreferrer' } as const;

/**
 * The footer of every page: the brand, the tagline and one row of links; the network's disclaimer (the site publishes software and information only) with
 * the security policy; the versions. The app adds its own links, the non-custodial statement, the indexer trust and the chart library's attribution
 * (TradingView's Apache-2.0 NOTICE, required on every page that can show its charts; the landing page has none).
 */
export function SiteFooter(props: { context: FrameContext; brandOnClick?: (e: Event) => void; brandLabel?: string }) {
  const services = useServices();
  const { config, kob, indexer } = services;
  useFailoverState(services.failover); // re-render on a switch: the verifiers are those of the indexer in use
  const app = props.context === 'app';
  const net = landingNetworkText(config.network, siteName(typeof location === 'undefined' ? '' : location.hostname));
  const links = footerLinks(props.context, config.indexerUrl, !!supportAddressOf(config.features));
  const link = (l: (typeof links)[number]) => (
    <a key={l.key} href={l.href} data-testid={l.testid} {...(l.external ? EXTERNAL : {})}>
      {t(l.key)}
    </a>
  );
  const trust = indexerTrust(!!indexer, services.verifiers?.length ?? 0);
  let kobVersion = '?';
  if (app) {
    try {
      kobVersion = kob.version();
    } catch {
      /* an unusable engine is reported by the boot screen, not here */
    }
  }
  return (
    <footer class="footer" data-testid="footer" data-context={props.context}>
      <div class="container">
        <div class="ff-top">
          <Brand class="ff-brand" testid={app ? 'footer-brand' : 'landing-footer-brand'} onClick={props.brandOnClick} label={props.brandLabel} />
          <span class="ff-tag">{t('shell.footer.protocol')}</span>
          <nav class="ff-links" aria-label={t('landing.footer.nav')}>
            {links.filter((l) => l.group === 'site').map(link)}
            {app ? <span class="ff-sep" aria-hidden="true" /> : null}
            {links.filter((l) => l.group === 'app').map(link)}
          </nav>
        </div>
        {/* the network comes from the build's config: no "testnet" on mainnet */}
        <p class="ff-disc" data-testid={app ? 'footer-preaudit' : 'landing-disclaimer'}>
          <strong>{net.discLead}</strong> {net.discBody}
          {app ? (
            <>
              {' '}
              <span data-testid="footer-statement">{t('shell.footer.statement')}</span>
            </>
          ) : null}{' '}
          <a href={SECURITY_POLICY_URL} {...EXTERNAL}>
            {t('preaudit.policy')}
          </a>
        </p>
        <p class="ff-meta">
          {/* an unreleased engine build reports 0.0.0: no version to show */}
          <span data-testid="footer-versions">
            {app && releasedVersion(kobVersion) ? t('shell.footer.versions', { app: appVersion, kob: kobVersion }) : t('shell.footer.version', { app: appVersion })}
          </span>
          {app ? (
            <span data-testid="footer-indexer-trust" data-trust={trust}>
              <a href="#/settings">{t(`shell.indexers.badge.${trust}`)}</a>
            </span>
          ) : null}
          {showsChartNotice(props.context) ? (
            <span data-testid="footer-chart-notice">
              {t('shell.footer.charts')}{' '}
              <a href="https://www.tradingview.com/" {...EXTERNAL}>
                TradingView Lightweight Charts™
              </a>{' '}
              © 2025 TradingView, Inc.
            </span>
          ) : null}
        </p>
      </div>
    </footer>
  );
}
