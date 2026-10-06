import type { ComponentChildren, JSX } from 'preact';
import { Spinner } from './Spinner';

export type ButtonVariant = 'primary' | 'secondary' | 'danger' | 'ghost';

export interface ButtonProps extends Omit<JSX.HTMLAttributes<HTMLButtonElement>, 'size' | 'loading'> {
  variant?: ButtonVariant;
  /** shows a spinner and blocks clicks (the button keeps its width) */
  loading?: boolean;
  small?: boolean;
  block?: boolean;
  children?: ComponentChildren;
  disabled?: boolean;
  type?: 'button' | 'submit' | 'reset';
}

/** Button. `data-testid`, `aria-*` and every other attribute are forwarded. Default type is `button` (never submits a form by accident). */
export function Button(props: ButtonProps) {
  const { variant = 'secondary', loading = false, small = false, block = false, children, disabled, type = 'button', class: cls, className, onClick, ...rest } = props as ButtonProps & { class?: string; className?: string };
  const classes = ['btn', variant === 'secondary' ? '' : `btn-${variant}`, small ? 'btn-sm' : '', block ? 'btn-block' : '', loading ? 'btn-loading' : '', cls ?? '', className ?? '']
    .filter(Boolean)
    .join(' ');
  return (
    <button
      {...rest}
      type={type}
      class={classes}
      disabled={disabled || loading}
      aria-busy={loading ? 'true' : undefined}
      onClick={loading ? undefined : onClick}
    >
      {loading ? <Spinner /> : null}
      {children}
    </button>
  );
}
