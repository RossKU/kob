// A small "?" next to a label or a heading with its explanation behind it. It is the same panel as
// the warn badges' TipBadge (market/TipBadge.tsx), with any content.
// - Click, tap, Enter or Space opens and closes it.
// - Escape closes it, and only it, so a dialog around it stays open. A press elsewhere does too.
//   The focus goes back to the button.
// - Nothing is hover-only.
//
// The panel is fixed to the viewport, because a card that cuts what overflows it does not cut it.
// It is kept 16 px inside the window's sides, under the button, or above it when there is no room
// below, and it follows the button on a scroll.
import type { ComponentChildren } from 'preact';
import { useId, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { t } from '../../i18n';

export interface HelpTipProps {
  /**
   * What the help is about (a field's label, a heading). The button is named "More about {label}"
   * and the panel {label}.
   */
  label: string;
  /** The explanation: plain text or paragraphs. */
  children: ComponentChildren;
  /** The button's test id. The panel's is `<id>-panel`. */
  'data-testid'?: string;
  class?: string;
}

const GUTTER = 16;
const GAP = 6;

export function HelpTip(props: HelpTipProps) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const id = `help-${useId()}`;
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);

  // A layout effect, so it is placed and listening in the frame the panel appears. An Escape or a
  // tap right after the click is not lost.
  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return;
    }
    const place = () => {
      const b = button.current?.getBoundingClientRect();
      const p = panel.current;
      if (!b || !p) return;
      const w = p.offsetWidth;
      const h = p.offsetHeight;
      const left = Math.max(GUTTER, Math.min(b.left - 8, window.innerWidth - w - GUTTER));
      let top = b.bottom + GAP;
      if (top + h > window.innerHeight - 8 && b.top - GAP - h >= 8) top = b.top - GAP - h;
      setPos((old) => (old && old.left === left && old.top === top ? old : { left, top }));
    };
    place();
    const close = (focus: boolean) => {
      setOpen(false);
      if (focus) button.current?.focus();
    };
    // Capture, so it runs before a dialog's own Escape, which would close the dialog.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.stopPropagation();
      e.preventDefault();
      close(true);
    };
    const onDown = (e: Event) => {
      const el = e.target as Node | null;
      if (el && (button.current?.contains(el) || panel.current?.contains(el))) return;
      close(false);
    };
    let frame = 0;
    const follow = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(place);
    };
    document.addEventListener('keydown', onKey, true);
    document.addEventListener('pointerdown', onDown);
    window.addEventListener('scroll', follow, true);
    window.addEventListener('resize', follow);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener('keydown', onKey, true);
      document.removeEventListener('pointerdown', onDown);
      window.removeEventListener('scroll', follow, true);
      window.removeEventListener('resize', follow);
    };
  }, [open]);

  const testid = props['data-testid'];
  return (
    <>
      <button
        type="button"
        ref={button}
        class={`help-tip${props.class ? ` ${props.class}` : ''}`}
        aria-label={t('common.helpAbout', { topic: props.label })}
        aria-expanded={open ? 'true' : 'false'}
        aria-controls={id}
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        data-testid={testid}
      >
        <span aria-hidden="true">?</span>
      </button>
      {open ? (
        <div
          id={id}
          ref={panel}
          class="help-panel"
          role="note"
          aria-label={props.label}
          style={pos ? { left: `${pos.left}px`, top: `${pos.top}px` } : { left: '0px', top: '0px', visibility: 'hidden' }}
          data-testid={testid ? `${testid}-panel` : undefined}
        >
          {props.children}
        </div>
      ) : null}
    </>
  );
}
