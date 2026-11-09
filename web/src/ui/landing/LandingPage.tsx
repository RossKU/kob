// The landing page: what opens at the empty hash (config `home`, default `landing`) for a visitor arriving from outside. A full page in the app's frame
// (Frame.tsx: the same header row, here with GitHub, the network chip, "Open the app" -> `#/market` and the theme toggle, and the same footer) in place of
// the app's tabs, banners and views; every other route shows the app shell.
// The founder's approved design (prototype E), rebuilt as components: the motion is in use-reveal.ts, the look in landing.css (dark / light follow the
// app's theme, `data-theme` on <html>). No inline script or HTML string: CSP and Trusted Types as everywhere else in the app.
import type { ComponentChildren } from 'preact';
import { useEffect, useRef } from 'preact/hooks';
import { useServices } from '../../app/context';
import { appTitle, useDocumentTitle } from '../../app/page-title';
import { t } from '../../i18n';
import { Brand, GitHubIcon, Logo, SiteFooter, SiteHeader, SkipLink, ThemeToggle } from '../shell/Frame';
import { NetworkChip } from '../shell/NetworkBadge';
import { TIGHT_QUERY, useMedia } from '../shell/narrow';
import { APP_HASH, feedbackActions, landingNetworkText, REPO_URL, siteName } from './landing-model';
import { useHeroGo, useReveal } from './use-reveal';
import './landing.css';

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
        <GitHubIcon class="lp-ico" />
        {t('landing.github')}
      </a>
    </div>
  );
}

/** The landing page's header: the shared frame's row (the app's height, column, brand and controls) with GitHub, the network chip, "Open the app" and the
 *  theme toggle (the app's, one stored preference). A phone keeps the brand, the chip, "Open the app" and the toggle (GitHub is in the hero and the footer). */
function Nav() {
  const { config } = useServices();
  const tight = useMedia(TIGHT_QUERY);
  return (
    <SiteHeader context="landing" testid="landing-nav" brand={<Brand testid="landing-brand" onClick={toTop} label={t('landing.toTop')} />}>
      <nav class="header-tools" aria-label={t('landing.nav.label')}>
        <a class="btn btn-ghost btn-sm fh-wide" href={REPO_URL}>
          <GitHubIcon />
          {t('landing.github')}
        </a>
        <NetworkChip network={config.network} short={tight} />
        <a class="btn btn-sm btn-pill" href={APP_HASH} data-testid="landing-open-app">
          {t('landing.openApp')}
        </a>
        <ThemeToggle />
      </nav>
    </SiteHeader>
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

function Row(props: { k: string; v: string; vClass: string; children: ComponentChildren }) {
  return (
    <div class="lp-lr lp-rv">
      <span class="lp-k">{props.k}</span>
      <p>{props.children}</p>
      <span class={`lp-v ${props.vClass}`}>{props.v}</span>
    </div>
  );
}

/** The bots section's terminal for `network`: commands only (a dry run), the shape of a script's or an agent's loop. */
/**
 * The closing heading keeps its two lines ("The book" / "is open."), whatever font the viewer's system gives it: the page brings no font of its own, and a
 * wider fallback than Inter Display used to break each line in two. Each line is unwrapped (landing.css); where the widest one does not fit beside the
 * mark, the heading's type shrinks until it does.
 */
function useFitLines(head: { current: HTMLDivElement | null }) {
  useEffect(() => {
    const el = head.current;
    const h2 = el?.querySelector('h2');
    if (!el || !h2) return;
    let width = -1;
    const fit = (force: boolean) => {
      if (!force && el.clientWidth === width) return;
      width = el.clientWidth;
      h2.style.removeProperty('font-size');
      const room = h2.clientWidth;
      const need = Math.max(0, ...Array.from(h2.querySelectorAll<HTMLElement>('.lp-ln > span'), (s) => s.scrollWidth));
      if (room > 0 && need > room) h2.style.fontSize = `${Math.floor((parseFloat(getComputedStyle(h2).fontSize) * room) / need)}px`;
    };
    fit(true);
    const ro = typeof ResizeObserver === 'function' ? new ResizeObserver(() => fit(false)) : null;
    ro?.observe(el);
    void document.fonts?.ready.then(() => fit(true));
    return () => ro?.disconnect();
  }, []);
}

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
  const finalHead = useRef<HTMLDivElement>(null);
  useFitLines(finalHead);
  const site = siteName(typeof location === 'undefined' ? '' : location.hostname);
  const net = landingNetworkText(config.network, site);

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
    <>
      <SkipLink />
      <Nav />
      <div class="lp" ref={root} data-testid="landing" data-network={config.network}>
        <main class="lp-main" id="main" tabIndex={-1} data-testid="main" data-route="#/">
          <section class={`lp-hero${go ? ' lp-go' : ''}`} data-testid="landing-hero">
            <div class="lp-wrap">
              <p class="lp-kicker lp-fade">
                {t('landing.kicker')}{' '}
                <span class="lp-chip lp-hero-chip" title={net.chipTip} data-testid="landing-chip">
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
                  <p data-testid="landing-bleed-body">
                    <span class="lp-line">{t('landing.bleed.body.l1')}</span> <span class="lp-line">{t('landing.bleed.body.l2')}</span>
                  </p>
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
                  <div class="lp-tx-sec">{t('landing.tx.place')}</div>
                  <dl>
                    <dt>{t('landing.tx.token')}</dt>
                    <dd class="lp-s">{t('landing.tx.token.v')}</dd>
                    <dt>{t('landing.tx.kas')}</dt>
                    <dd class="lp-s">{t('landing.tx.kas.v')}</dd>
                    <dt>{t('landing.tx.locked')}</dt>
                    <dd>{t('landing.tx.locked.v')}</dd>
                    <dt>{t('landing.tx.fee')}</dt>
                    <dd>{t('landing.tx.fee.v')}</dd>
                  </dl>
                  <div class="lp-tx-sec">{t('landing.tx.order')}</div>
                  <dl>
                    <dt>{t('landing.tx.amount')}</dt>
                    <dd>{t('landing.tx.amount.v')}</dd>
                    <dt>{t('landing.tx.limit')}</dt>
                    <dd>{t('landing.tx.limit.v')}</dd>
                    <dt>{t('landing.tx.expires')}</dt>
                    <dd>{t('landing.tx.expires.v')}</dd>
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
                <Row k={t('landing.trust.offswitch')} v={t('landing.trust.none')} vClass="lp-none">
                  <span data-testid="landing-offswitch">{t('landing.trust.offswitch.body')}</span>
                </Row>
                <Row k={t('landing.trust.source')} v={t('landing.trust.none')} vClass="lp-none">
                  {t('landing.trust.source.body')} <a href={REPO_URL}>{t('landing.trust.source.link')}</a>
                </Row>
                <Row k={t('landing.trust.audit')} v={t('landing.trust.none')} vClass="lp-none">
                  <span data-testid="landing-audit">{net.auditBody}</span>
                  <span class="lp-links lp-lr-links">
                    {feedbackActions().map((a) => (
                      <a key={a.key} href={a.href} data-testid={a.testid}>
                        {t(a.key)}
                      </a>
                    ))}
                  </span>
                  <span class="lp-lr-note" data-testid="landing-audit-note">
                    {t('landing.trust.audit.note')}
                  </span>
                </Row>
              </div>
            </div>
          </section>

          <section class="lp-final">
            <div class="lp-wrap">
              <p class="lp-final-lead lp-rv" data-testid="landing-not-business">
                {t('landing.final.lead')}
              </p>
              <div class="lp-final-head" ref={finalHead} data-testid="landing-final-head">
                <h2>
                  <Ln>{t('landing.final.l1')}</Ln>
                  <Ln d={1} class="lp-accent">
                    {t('landing.final.l2')}
                  </Ln>
                </h2>
                <span class="lp-mini lp-rv" aria-hidden="true">
                  <Logo />
                </span>
              </div>
              <Ctas class="lp-rv lp-d2" />
            </div>
          </section>
        </main>
      </div>
      <SiteFooter context="landing" brandOnClick={toTop} brandLabel={t('landing.toTop')} />
    </>
  );
}
