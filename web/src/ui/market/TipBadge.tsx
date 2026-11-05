// A short warn badge with its full sentence behind it: a hover shows the sentence as a tooltip, and a tap or the keyboard (Enter / Space on the focused badge)
// opens it as a small panel (a title tooltip does not exist on a touch screen). The panel is fixed to the viewport so a list row, which cuts what overflows
// it, does not cut it. A tap on the badge closes it again, as do Escape, a tap elsewhere, a scroll and a resize. Same behaviour as the "High risk" chip.
import { useLayoutEffect, useRef, useState } from 'preact/hooks';

export function TipBadge(props: { label: string; detail: string; 'data-testid': string; 'data-detail-testid': string }) {
  const [pos, setPos] = useState<{ left: number; top: number; scrollY: number } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  // a layout effect: the listeners must exist in the same frame the panel appears (a passive effect runs after the next paint and an Escape or tap in between was lost)
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
    // a scroll closes it (the panel is fixed to the viewport and would stay behind the badge), but the page's own scroll that already happened (an event
    // still on its way when the badge was tapped) is not one: only a scroll that moved the page since the panel opened, or of another scroller, does
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
      {' '}
      <button
        type="button"
        class="badge badge-warn tip-btn"
        ref={button}
        onClick={toggle}
        title={props.detail}
        aria-expanded={pos ? 'true' : 'false'}
        aria-label={`${props.label}: ${props.detail}`}
        data-testid={props['data-testid']}
      >
        <span aria-hidden="true">{'\u26A0\uFE0E'}</span> {props.label}
      </button>
      {pos ? (
        <div class="tip-panel" data-tip-panel role="note" style={{ left: `${pos.left}px`, top: `${pos.top}px` }} data-testid={props['data-detail-testid']}>
          {props.detail}
        </div>
      ) : null}
    </>
  );
}
