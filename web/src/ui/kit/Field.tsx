import type { ComponentChildren } from 'preact';
import { useId } from 'preact/hooks';

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface FieldShellProps {
  label?: ComponentChildren;
  /** at the right end of the label row (a balance, a shortcut); kept out of the `<label>` so it is not part of the control's name */
  labelAside?: ComponentChildren;
  hint?: ComponentChildren;
  /** an error text puts the control into the invalid state (`aria-invalid`) and shows the message under it */
  error?: ComponentChildren | null;
  /** id of the control (generated when missing) */
  id?: string;
  class?: string;
  /** render prop: put `id`, `aria-describedby` and `aria-invalid` on the control */
  children: (a: { id: string; describedBy: string | undefined; invalid: boolean }) => ComponentChildren;
}

/** Label + control + hint + error scaffolding for controls the kit does not render itself (checkboxes, custom pickers). */
/**
 * A text in an element of its own, keyed by its content: a change renders a new element (a page translator such as Chrome's rewrites text nodes
 * in place, so a text node updated in place would keep the old translation). Anything but a string passes as it is.
 */
const keyed = (c: ComponentChildren): ComponentChildren => (typeof c === 'string' ? <span key={c}>{c}</span> : c);

export function FieldShell(props: FieldShellProps) {
  const auto = useId();
  const id = props.id ?? `f${auto}`;
  const hintId = props.hint ? `${id}-hint` : undefined;
  const errId = props.error ? `${id}-err` : undefined;
  const describedBy = [hintId, errId].filter(Boolean).join(' ') || undefined;
  return (
    <div class={`field${props.class ? ` ${props.class}` : ''}`}>
      {props.label && props.labelAside ? (
        <div class="field-label-row">
          <label for={id}>{keyed(props.label)}</label>
          {props.labelAside}
        </div>
      ) : props.label ? <label for={id}>{keyed(props.label)}</label> : null}
      {props.children({ id, describedBy, invalid: !!props.error })}
      {props.hint ? <div class="field-hint" id={hintId}>{keyed(props.hint)}</div> : null}
      {props.error ? <div class="field-error" id={errId} role="alert">{keyed(props.error)}</div> : null}
    </div>
  );
}

export interface FieldProps {
  label?: ComponentChildren;
  labelAside?: ComponentChildren;
  hint?: ComponentChildren;
  error?: ComponentChildren | null;
  /** control kind (default `input`) */
  as?: 'input' | 'textarea' | 'select';
  /** `select` only */
  options?: SelectOption[];
  value?: string | number;
  /** convenience: called with the new string value on `input` (and `change` for selects) */
  onValue?: (value: string) => void;
  onInput?: (e: Event) => void;
  onChange?: (e: Event) => void;
  onBlur?: (e: FocusEvent) => void;
  type?: string;
  name?: string;
  placeholder?: string;
  disabled?: boolean;
  readOnly?: boolean;
  required?: boolean;
  autoComplete?: string;
  inputMode?: 'none' | 'text' | 'tel' | 'url' | 'email' | 'numeric' | 'decimal' | 'search';
  min?: number | string;
  max?: number | string;
  step?: number | string;
  rows?: number;
  maxLength?: number;
  spellcheck?: boolean;
  checked?: boolean;
  id?: string;
  class?: string;
  /** forwarded to the CONTROL element (not the wrapper): `field-<intentFieldName>`, `order-price`, ... */
  'data-testid'?: string;
  /** trailing content next to the control (unit, "max" button) */
  suffix?: ComponentChildren;
}

/**
 * Label + input / textarea / select + hint + error. `data-testid` goes on the control itself so `page.getByTestId(...).fill(...)` works.
 * The value is controlled: pass `value` and `onValue` (string) or the raw `onInput`.
 */
export function Field(props: FieldProps) {
  const { label, labelAside, hint, error, as = 'input', options, onValue, onInput, onChange, suffix, class: cls, id, 'data-testid': testId, ...rest } = props;
  return (
    <FieldShell label={label} labelAside={labelAside} hint={hint} error={error} id={id} class={cls}>
      {({ id: cid, describedBy, invalid }) => {
        const common = {
          id: cid,
          'aria-describedby': describedBy,
          'aria-invalid': invalid ? ('true' as const) : undefined,
          'data-testid': testId,
        };
        let control: ComponentChildren;
        if (as === 'select') {
          control = (
            <select
              {...common}
              class="select"
              name={rest.name}
              value={rest.value}
              disabled={rest.disabled}
              required={rest.required}
              onChange={(e) => {
                onChange?.(e);
                onValue?.((e.currentTarget as HTMLSelectElement).value);
              }}
            >
              {(options ?? []).map((o) => (
                <option key={o.value} value={o.value} disabled={o.disabled}>
                  {o.label}
                </option>
              ))}
            </select>
          );
        } else if (as === 'textarea') {
          control = (
            <textarea
              {...common}
              class="textarea"
              name={rest.name}
              value={rest.value as string | undefined}
              placeholder={rest.placeholder}
              disabled={rest.disabled}
              readOnly={rest.readOnly}
              required={rest.required}
              rows={rest.rows}
              maxLength={rest.maxLength}
              spellcheck={rest.spellcheck}
              onInput={(e) => {
                onInput?.(e);
                onValue?.((e.currentTarget as HTMLTextAreaElement).value);
              }}
              onBlur={rest.onBlur}
            />
          );
        } else {
          control = (
            <input
              {...common}
              class="input"
              type={rest.type ?? 'text'}
              name={rest.name}
              value={rest.value}
              checked={rest.checked}
              placeholder={rest.placeholder}
              disabled={rest.disabled}
              readOnly={rest.readOnly}
              required={rest.required}
              autoComplete={rest.autoComplete ?? 'off'}
              inputMode={rest.inputMode}
              min={rest.min}
              max={rest.max}
              step={rest.step}
              maxLength={rest.maxLength}
              spellcheck={rest.spellcheck ?? false}
              onInput={(e) => {
                onInput?.(e);
                onValue?.((e.currentTarget as HTMLInputElement).value);
              }}
              onChange={onChange}
              onBlur={rest.onBlur}
            />
          );
        }
        return suffix ? <div class="row" style="flex-wrap:nowrap"><div class="grow">{control}</div>{suffix}</div> : control;
      }}
    </FieldShell>
  );
}

export interface SelectProps extends Omit<FieldProps, 'as' | 'type'> {
  options: SelectOption[];
}

/** `<Field as="select">` with a required option list. */
export function Select(props: SelectProps) {
  return <Field {...props} as="select" />;
}

/** Checkbox with its label to the right; `data-testid` on the input. */
export function Checkbox(props: { checked: boolean; onChange: (checked: boolean) => void; label: ComponentChildren; hint?: ComponentChildren; disabled?: boolean; 'data-testid'?: string; id?: string }) {
  return (
    <FieldShell hint={props.hint} id={props.id}>
      {({ id, describedBy }) => (
        <div class="checkbox-row">
          <input
            id={id}
            type="checkbox"
            checked={props.checked}
            disabled={props.disabled}
            aria-describedby={describedBy}
            data-testid={props['data-testid']}
            onChange={(e) => props.onChange((e.currentTarget as HTMLInputElement).checked)}
          />
          <label for={id} style="font-weight:400">{props.label}</label>
        </div>
      )}
    </FieldShell>
  );
}
