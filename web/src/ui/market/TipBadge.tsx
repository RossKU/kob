// A short warn badge with its full sentence behind it. A hover shows the sentence as a tooltip. A
// tap, or the keyboard (Enter or Space on the focused badge), opens it as a small panel, because a
// title tooltip does not exist on a touch screen. The panel is fixed to the viewport so that a
// list row, which cuts what overflows it, does not cut it. A tap on the badge closes it again, as
// do Escape, a tap elsewhere, a scroll and a resize. The behaviour is the same as the "High risk"
// chip.
import { useLayoutEffect, useRef, useState } from 'preact/hooks';

/**
 * `variant` 'warn' is the default warning badge. 'tag' is a small muted tag with a (?), such as
 * the summary cards' "Estimate (?)". It has the same panel and behaviour, and no space before it,
 * because the caller places it. 'bar' is the market bar's shared-ticker note: the warning sign alone
 * in a small warn-coloured pill that the bar's CSS places (.mkt-bar-note). The label and the
 * sentence are its accessible name, and the panel is the way to read them, so it has no title
 * tooltip.
 */
export function TipBadge(props: { label: string; detail: string; 'data-testid': string; 'data-detail-testid': string; variant?: 'warn' | 'tag' | 'bar' }) {
  const tag = props.variant === 'tag';
  const bar = props.variant === 'bar';
  const [pos, setPos] = useState<{ left: number; top: number; scrollY: number } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  // A layout effect, because the listeners must exist in the same frame the panel appears. A
  // passive effect runs after the next paint, and an Escape or tap in between would be lost.
  useLayoutEffect(() => {
    if (!pos) return;
    const close = () => setPos(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      close();
      button.current?.focus();
    };
    const onDown = (e: Event) => {
      const el = e.target as Node | null;
      if (el && (button.current?.contains(el) || (el as HTMLElement).closest?.('[data-tip-panel]'))) return;
      close();
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onDown);
    // A scroll closes it, because the panel is fixed to the viewport and would stay behind the
    // badge. The page's own scroll that already happened, an event still on its way when the badge
    // was tapped, is not one. Only a scroll that moved the page since the panel opened, or of
    // another scroller, closes it.
    const onScroll = (e: Event) => {
      if (e.target === document && Math.abs(window.scrollY - pos.scrollY) < 1) return;
      close();
    };
    window.addEventListener('scroll', onScroll, true);
    window.addEventListener('resize', close);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onDown);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('resize', close);
    };
  }, [pos]);
  const toggle = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    if (pos) return setPos(null);
    const b = button.current!.getBoundingClientRect();
    const width = Math.min(320, window.innerWidth - 24);
    setPos({ left: Math.max(12, Math.min(b.left, window.innerWidth - width - 12)), top: b.bottom + 6, scrollY: window.scrollY });
  };
  return (
    <>
      {tag || bar ? null : ' '}
      <button
        type="button"
        class={tag ? 'tip-btn tip-tag' : bar ? 'tip-btn mkt-bar-note' : 'badge badge-warn tip-btn'}
        ref={button}
        onClick={toggle}
        title={bar ? undefined : props.detail}
        aria-expanded={pos ? 'true' : 'false'}
        aria-label={`${props.label}: ${props.detail}`}
        data-testid={props['data-testid']}
      >
        {tag ? (
          <span class="chip-label"><span class="tip-word">{props.label} </span><span aria-hidden="true">(?)</span></span>
        ) : bar ? (
          <span aria-hidden="true">{'\u26A0\uFE0E'}</span>
        ) : (
          <span class="chip-label"><span aria-hidden="true">{'\u26A0\uFE0E'}</span> {props.label}</span>
        )}
      </button>
      {pos ? (
        <div class="tip-panel" data-tip-panel role="note" style={{ left: `${pos.left}px`, top: `${pos.top}px` }} data-testid={props['data-detail-testid']}>
          {props.detail}
        </div>
      ) : null}
    </>
  );
}
