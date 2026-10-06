// Page-wide checks used by the dictionary and layout specs.
import type { Page } from '@playwright/test';
import { expect } from '../fixtures';

/** The shape of a translation key that was rendered instead of its text: `ticket.type.limit.name`. */
export const KEY_SHAPE = /^[a-z]+\.[a-zA-Z.]+$/;
/** Every namespace of the app's dictionaries; a hyphenated key (`orders.type.limit-gtd`) is caught by this one. */
export const KEY_NAMESPACED = /^(common|shell|market|ticket|confirm|orders|settings|issue|issues|amend)\.[\w.-]+$/;

/** Texts (text nodes and the placeholder / title / aria-label / alt attributes) currently on the page that look like an untranslated key. */
export async function leakedKeys(page: Page): Promise<string[]> {
  return page.evaluate(
    ([shape, ns]) => {
      const a = new RegExp(shape);
      const b = new RegExp(ns);
      const found = new Set<string>();
      const check = (raw: string | null | undefined) => {
        const t = (raw ?? '').trim();
        if (t && (a.test(t) || b.test(t))) found.add(t);
      };
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      for (let n = walker.nextNode(); n; n = walker.nextNode()) {
        // the text of an external link is a domain name (kaspa.org), not a dictionary key
        if (n.parentElement?.closest('a[href^="http"]')) continue;
        check(n.textContent);
      }
      for (const el of document.body.querySelectorAll('[placeholder],[title],[aria-label],[alt]')) {
        for (const attr of ['placeholder', 'title', 'aria-label', 'alt']) check(el.getAttribute(attr));
      }
      // <option> text is part of the text nodes above; hidden <details> content too
      return [...found];
    },
    [KEY_SHAPE.source, KEY_NAMESPACED.source],
  );
}

export async function expectNoLeakedKeys(page: Page, where: string): Promise<void> {
  expect(await leakedKeys(page), `untranslated keys on ${where}`).toEqual([]);
}

/** Pixels the page is wider than the window (0 = no horizontal scroll). */
export async function horizontalOverflow(page: Page): Promise<number> {
  return page.evaluate(() => Math.max(0, document.documentElement.scrollWidth - window.innerWidth));
}
