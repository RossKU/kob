// The landing page's motion: the hero's lines rise in once the page has painted, and each block below rises / grows in the first time it scrolls into
// view (an IntersectionObserver marks it `lp-seen`, once). Without IntersectionObserver everything is shown at once. prefers-reduced-motion is handled
// by landing.css (no transitions, everything in its final place), so the hooks do not need to know about it.
import type { RefObject } from 'preact';
import { useEffect, useState } from 'preact/hooks';

/** Elements of the page that reveal on scroll. */
export const REVEAL_SELECTOR = '.lp-rv, .lp-sec .lp-ln, .lp-final .lp-ln, .lp-bigbook';
export const SEEN = 'lp-seen';

/** Marks the reveal elements under `root` as seen when they enter the viewport (14 % above its bottom edge, as the prototype). */
export function useReveal(root: RefObject<HTMLElement>): void {
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const targets = [...el.querySelectorAll<HTMLElement>(REVEAL_SELECTOR)];
    if (typeof IntersectionObserver !== 'function') {
      for (const t of targets) t.classList.add(SEEN);
      return;
    }
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (!e.isIntersecting) continue;
          e.target.classList.add(SEEN);
          io.unobserve(e.target);
        }
      },
      { rootMargin: '0px 0px -14% 0px', threshold: 0.01 },
    );
    for (const t of targets) io.observe(t);
    return () => io.disconnect();
  }, [root]);
}

/** True two frames after the first render: the hero's start state has been painted, so its transition runs. */
export function useHeroGo(): boolean {
  const [go, setGo] = useState(false);
  useEffect(() => {
    let a = 0;
    let b = 0;
    a = requestAnimationFrame(() => {
      b = requestAnimationFrame(() => setGo(true));
    });
    return () => {
      cancelAnimationFrame(a);
      cancelAnimationFrame(b);
    };
  }, []);
  return go;
}

/** True once the page is scrolled more than `px` (the nav gets its backdrop). */
export function useScrolled(px = 20): boolean {
  const [scrolled, setScrolled] = useState(() => typeof window !== 'undefined' && window.scrollY > px);
  useEffect(() => {
    const on = () => setScrolled(window.scrollY > px);
    window.addEventListener('scroll', on, { passive: true });
    on();
    return () => window.removeEventListener('scroll', on);
  }, [px]);
  return scrolled;
}
