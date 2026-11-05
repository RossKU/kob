// The landing page: what opens at the empty hash (config `home`, default `landing`) for a visitor arriving from outside. A full page of its own (its own
// nav: the brand, GitHub, "Open the app" -> `#/market`), in place of the app's header, banners and footer; every other route shows the app shell.
// The founder's approved design (prototype E), rebuilt as components: the motion is in use-reveal.ts, the look in landing.css (dark / light follow the
// app's theme, `data-theme` on <html>). No inline script or HTML string: CSP and Trusted Types as everywhere else in the app.
import type { ComponentChildren } from 'preact';
import { useEffect, useRef } from 'preact/hooks';
import { useServices } from '../../app/context';
import { appTitle, useDocumentTitle } from '../../app/page-title';
import { t } from '../../i18n';
import { Logo } from '../shell/Header';
import { APP_HASH, landingNetworkText, publicApiUrl, REPO_URL, siteName } from './landing-model';
import { useHeroGo, useReveal, useScrolled } from './use-reveal';
import './landing.css';

function GitHubIcon() {
  return (
    <svg class="lp-ico" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        fill="currentColor"
        d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"
      />
    </svg>
  );
}

function ArrowIcon() {
  return (
    <svg class="lp-ico" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
      <path d="M4 10h11m-4-4.5L15.5 10 11 14.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  );
}

/** Scrolls back to the top (the brand): a hash link would be a route here. */
function toTop(e: Event) {
  e.preventDefault();
  const smooth = !(typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches);
  window.scrollTo({ top: 0, behavior: smooth ? 'smooth' : 'auto' });
}

function Brand(props: { testid: string }) {
  return (
    <a class="lp-brand" href="#/" onClick={toTop} aria-label={t('landing.toTop')} data-testid={props.testid}>
      <Logo />
      {t('common.appName')}
    </a>
  );
}

/** One line of a big heading: it rises out of its own clip (`d` = its delay step). */
function Ln(props: { d?: 1 | 2 | 3; class?: string; children: ComponentChildren; hidden?: boolean }) {
  const cls = ['lp-ln', props.d ? `lp-d${props.d}` : '', props.class ?? ''].filter(Boolean).join(' ');
  return (
    <span class={cls} aria-hidden={props.hidden ? 'true' : undefined}>
      <span>{props.children}</span>
    </span>
  );
}

function Ctas(props: { class?: string }) {
  return (
    <div class={`lp-ctas${props.class ? ` ${props.class}` : ''}`}>
      <a class="lp-pill lp-pill-solid" href={APP_HASH} data-testid="landing-cta">
        {t('landing.openApp')} <ArrowIcon />
      </a>
      <a class="lp-pill lp-pill-line" href={REPO_URL}>
        <GitHubIcon />
        {t('landing.github')}
      </a>
    </div>
  );
}

function Nav() {
  const scrolled = useScrolled();
  return (
    <header class={`lp-nav${scrolled ? ' lp-scrolled' : ''}`} data-testid="landing-nav">
      <div class="lp-nav-bg" />
      <div class="lp-wrap">
        <Brand testid="landing-brand" />
        <nav class="lp-nav-r" aria-label={t('landing.nav.label')}>
          <a class="lp-gh" href={REPO_URL}>
            {t('landing.github')}
          </a>
          <a class="lp-pill lp-pill-solid" href={APP_HASH} data-testid="landing-open-app">
            {t('landing.openApp')}
          </a>
        </nav>
      </div>
    </header>
  );
}

function Step(props: { n: string; title: string; children: ComponentChildren }) {
  return (
    <div class="lp-st lp-rv">
      <span class="lp-n">{props.n}</span>
      <h3>{props.title}</h3>
      <p>{props.children}</p>
    </div>
  );
}

function Row(props: { k: string; v: string; vClass: string; warn?: boolean; children: ComponentChildren }) {
  return (
    <div class={`lp-lr lp-rv${props.warn ? ' lp-warn' : ''}`}>
      <span class="lp-k">{props.k}</span>
      <p>{props.children}</p>
      <span class={`lp-v ${props.vClass}`}>{props.v}</span>
    </div>
  );
}

/** The bots section's terminal for `network`: commands only (a dry run), the shape of a script's or an agent's loop. */
function botExample(network: string): string {
  return [
    `$ kob book TUSD --network ${network} --json`,
    '$ kob place limit --token TUSD --side buy \\',
    '    --amount 100TUSD --price 0.24KAS \\',
    `    --network ${network} --key-file maker.key \\`,
    '    --dry-run --json',
    '$ kob watch fills --token TUSD --json',
    '# one JSON event per line (NDJSON)',
  ].join('\n');
}

export function LandingPage() {
  const { config } = useServices();
  useDocumentTitle(appTitle());
  const root = useRef<HTMLDivElement>(null);
  useReveal(root);
  const go = useHeroGo();
  const site = siteName(typeof location === 'undefined' ? '' : location.hostname);
  const net = landingNetworkText(config.network, site);
  const api = publicApiUrl(config.indexerUrl);

  // the page's own background behind overscroll (the body is the app's otherwise); it opens at its top, and the app page a link leads to opens at
  // its top too (a hash change keeps the scroll position, and this page is much taller than the app's)
  useEffect(() => {
    document.body.classList.add('lp-body');
    window.scrollTo(0, 0);
    return () => {
      document.body.classList.remove('lp-body');
      window.scrollTo(0, 0);
    };
  }, []);

  return (
    <div class="lp" ref={root} data-testid="landing" data-network={config.network}>
      <Nav />
      <main class="lp-main" id="main" tabIndex={-1} data-testid="main" data-route="#/">
        <section class={`lp-hero${go ? ' lp-go' : ''}`} data-testid="landing-hero">
          <div class="lp-wrap">
            <p class="lp-kicker lp-fade">
              {t('landing.kicker')}{' '}
              <span class="lp-chip" title={net.chipTip} data-testid="landing-chip">
                {net.chip}
              </span>
            </p>
            <h1 aria-label={t('landing.h1')} data-testid="landing-h1">
              <Ln hidden>{t('landing.h1.l1')}</Ln>
              <Ln hidden d={1}>
                {t('landing.h1.l2')}
              </Ln>
              <Ln hidden d={2} class="lp-accent">
                {t('landing.h1.l3')}
              </Ln>
            </h1>
            <p class="lp-tagline lp-fade">{t('landing.tagline')}</p>
            <div class="lp-hero-foot lp-fade">
              <p>
                <b>{t('landing.hero.lead')}</b> {t('landing.hero.body')}
              </p>
              <Ctas />
            </div>
          </div>
        </section>

        <section class="lp-bleed" aria-label={t('landing.bleed.label')}>
          <div class="lp-bigbook" aria-hidden="true" data-testid="landing-bigbook">
            {[0, 1, 2, 3, 4, 5, 6, 7].map((i) => (
              <i key={i} class="lp-bb" />
            ))}
          </div>
          <div class="lp-bleed-cap">
            <div class="lp-wrap">
              <div class="lp-rv">
                <h2>{t('landing.bleed.title')}</h2>
                <p>{t('landing.bleed.body')}</p>
              </div>
            </div>
          </div>
        </section>

        <section class="lp-sec lp-sec-how">
          <div class="lp-wrap">
            <div class="lp-label lp-rv">{t('landing.how.label')}</div>
            <h2 class="lp-big">
              <Ln>{t('landing.how.l1')}</Ln>
              <Ln d={1} class="lp-dim">
                {t('landing.how.l2')}
              </Ln>
            </h2>
            <p class="lp-intro lp-rv">{t('landing.how.intro')}</p>
            <div class="lp-steps">
              <Step n="01" title={t('landing.how.s1.title')}>
                {t('landing.how.s1.body')}
              </Step>
              <Step n="02" title={t('landing.how.s2.title')}>
                {t('landing.how.s2.body')}
              </Step>
              <Step n="03" title={t('landing.how.s3.title')}>
                {t('landing.how.s3.body')} <a href={REPO_URL}>{t('landing.how.s3.link')}</a>
              </Step>
              <Step n="04" title={t('landing.how.s4.title')}>
                {t('landing.how.s4.body')}
              </Step>
            </div>
          </div>
        </section>

        <section class="lp-sec lp-sec-flush">
          <div class="lp-wrap lp-decode">
            <div>
              <div class="lp-label lp-rv">{t('landing.sign.label')}</div>
              <h2 class="lp-big">
                <Ln>{t('landing.sign.l1')}</Ln>
                <Ln d={1}>{t('landing.sign.l2')}</Ln>
              </h2>
              <p class="lp-intro lp-rv">{t('landing.sign.intro')}</p>
            </div>
            <div class="lp-rv lp-d1">
              <div class="lp-tx" role="img" aria-label={t('landing.tx.aria')}>
                <div class="lp-tx-h">
                  <span>{t('landing.tx.head')}</span>
                  <b>{t('landing.tx.decoded')}</b>
                </div>
                <dl>
                  <dt>{t('landing.tx.action')}</dt>
                  <dd>{t('landing.tx.action.v')}</dd>
                  <dt>{t('landing.tx.side')}</dt>
                  <dd>
                    <span class="lp-s">{t('landing.tx.side.sell')}</span> {t('landing.tx.side.v')}
                  </dd>
                  <dt>{t('landing.tx.limit')}</dt>
                  <dd>{t('landing.tx.limit.v')}</dd>
                  <dt>{t('landing.tx.filled')}</dt>
                  <dd>
                    <span class="lp-b">{t('landing.tx.filled.plus')}</span> {t('landing.tx.filled.v')}
                  </dd>
                  <dt>{t('landing.tx.creates')}</dt>
                  <dd>{t('landing.tx.creates.v')}</dd>
                  <dt>{t('landing.tx.tif')}</dt>
                  <dd>{t('landing.tx.tif.v')}</dd>
                </dl>
                <div class="lp-ok">
                  <i />
                  {t('landing.tx.ok')}
                </div>
              </div>
              <p class="lp-tx-note" data-testid="landing-tx-note">
                {net.txNote}
              </p>
            </div>
          </div>
        </section>

        <section class="lp-sec lp-sec-flush" data-testid="landing-bots">
          <div class="lp-wrap lp-decode">
            <div>
              <div class="lp-label lp-rv">{t('landing.bots.label')}</div>
              <h2 class="lp-big">
                <Ln>{t('landing.bots.l1')}</Ln>
                <Ln d={1} class="lp-dim">
                  {t('landing.bots.l2')}
                </Ln>
              </h2>
              <p class="lp-intro lp-rv">{t('landing.bots.intro')}</p>
              <p class="lp-intro lp-rv lp-links">
                <a href={`${REPO_URL}/tree/master/crates/kob-cli`}>{t('landing.bots.cli')}</a>
                <a href={`${REPO_URL}/blob/master/docs/api.md`}>{t('landing.bots.api')}</a>
              </p>
            </div>
            <div class="lp-rv lp-d1">
              <div class="lp-tx lp-code" role="img" aria-label={t('landing.bots.aria')}>
                <div class="lp-tx-h">
                  <span>{t('landing.bots.head')}</span>
                  <b>{t('landing.bots.json')}</b>
                </div>
                <pre>{botExample(config.network)}</pre>
              </div>
            </div>
          </div>
        </section>

        <section class="lp-sec lp-sec-flush">
          <div class="lp-wrap">
            <div class="lp-label lp-rv">{t('landing.trust.label')}</div>
            <h2 class="lp-big">
              <Ln>{t('landing.trust.l1')}</Ln>
              <Ln d={1} class="lp-dim">
                {t('landing.trust.l2')}
              </Ln>
            </h2>
            <div class="lp-ledger">
              <Row k={t('landing.trust.admin')} v={t('landing.trust.none')} vClass="lp-none">
                {t('landing.trust.admin.body')}
              </Row>
              <Row k={t('landing.trust.oracle')} v={t('landing.trust.none')} vClass="lp-none">
                {t('landing.trust.oracle.body')}
              </Row>
              <Row k={t('landing.trust.custody')} v={t('landing.trust.none')} vClass="lp-none">
                {t('landing.trust.custody.body')}
              </Row>
              <Row k={t('landing.trust.operator')} v={t('landing.trust.none')} vClass="lp-none">
                {t('landing.trust.operator.body', { site })}
              </Row>
              <Row k={t('landing.trust.source')} v={t('landing.trust.source.v')} vClass="lp-yes">
                {t('landing.trust.source.body')} <a href={REPO_URL}>{t('landing.trust.source.link')}</a>
              </Row>
              <Row k={t('landing.trust.audit')} v={t('landing.trust.audit.v')} vClass="" warn>
                <span data-testid="landing-audit">{net.auditBody}</span>
              </Row>
            </div>
          </div>
        </section>

        <section class="lp-final">
          <div class="lp-wrap">
            <span class="lp-mini lp-rv" aria-hidden="true">
              <Logo />
            </span>
            <h2>
              <Ln>{t('landing.final.l1')}</Ln>
              <Ln d={1} class="lp-accent">
                {t('landing.final.l2')}
              </Ln>
            </h2>
            <Ctas class="lp-rv lp-d2" />
          </div>
        </section>
      </main>

      <footer class="lp-footer" data-testid="landing-footer">
        <div class="lp-wrap">
          <div class="lp-f">
            <Brand testid="landing-footer-brand" />
            <span>{t('landing.footer.name')}</span>
            <nav aria-label={t('landing.footer.nav')}>
              <a href={APP_HASH}>{t('landing.footer.app')}</a>
              <a href={REPO_URL}>{t('landing.github')}</a>
              <a href={REPO_URL}>{t('landing.footer.executor')}</a>
              {api ? (
                <a href={api} data-testid="landing-api">
                  {t('landing.footer.api')}
                </a>
              ) : null}
            </nav>
          </div>
          <p class="lp-disc" data-testid="landing-disclaimer">
            <b>{net.discLead}</b> {net.discBody}
          </p>
        </div>
      </footer>
    </div>
  );
}
