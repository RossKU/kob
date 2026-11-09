import type { ComponentChildren, JSX } from 'preact';
import { useEffect, useState } from 'preact/hooks';
import { textOnly } from '../../app/translation-safe';
import { Spinner } from './Spinner';

export type ButtonVariant = 'primary' | 'secondary' | 'danger' | 'ghost';

export interface ButtonProps extends Omit<JSX.HTMLAttributes<HTMLButtonElement>, 'size' | 'loading'> {
  variant?: ButtonVariant;
  /**
   * a press is being worked on: clicks are ignored (`aria-busy`; the button is NOT disabled, so its colours do not flicker on a quick refresh), and when it lasts
   * longer than `SPINNER_DELAY_MS` a spinner takes the place of the label. The button never changes its size, so nothing beside it moves.
   */
  loading?: boolean;
  small?: boolean;
  block?: boolean;
  children?: ComponentChildren;
  disabled?: boolean;
  type?: 'button' | 'submit' | 'reset';
}

/** A refresh that finishes sooner than this shows no spinner at all (a flicker is worse than no cue). */
export const SPINNER_DELAY_MS = 300;

/** Button. `data-testid`, `aria-*` and every other attribute are forwarded. Default type is `button` (never submits a form by accident). */
export function Button(props: ButtonProps) {
  const { variant = 'secondary', loading = false, small = false, block = false, children, disabled, type = 'button', class: cls, className, onClick, ...rest } = props as ButtonProps & { class?: string; className?: string };
  // the spinner only after a while; it is drawn over the label (its space is the label's, kept), never beside it
  const [spin, setSpin] = useState(false);
  useEffect(() => {
    if (!loading) return setSpin(false);
    const id = setTimeout(() => setSpin(true), SPINNER_DELAY_MS);
    return () => clearTimeout(id);
  }, [loading]);
  const classes = ['btn', variant === 'secondary' ? '' : `btn-${variant}`, small ? 'btn-sm' : '', block ? 'btn-block' : '', loading ? 'btn-loading' : '', loading && spin ? 'btn-spin' : '', cls ?? '', className ?? '']
    .filter(Boolean)
    .join(' ');
  return (
    <button
      {...rest}
      type={type}
      class={classes}
      disabled={disabled}
      aria-busy={loading ? 'true' : (rest as { 'aria-busy'?: 'true' | 'false' })['aria-busy']}
      onClick={(e) => {
        // busy: the press is swallowed (a submit button must not submit either)
        if (loading) return e.preventDefault();
        onClick?.(e);
      }}
    >
      {loading && spin ? <Spinner /> : null}
      {/* a text label is its own element: it can be re-created while the page is machine-translated (app/translation-safe.ts), the button keeps its focus */}
      {textOnly(children) !== null ? <span class="btn-label">{children}</span> : children}
    </button>
  );
}
