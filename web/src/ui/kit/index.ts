// UI KIT: the small, dependency-free component set every view uses (Preact 10, styles in src/styles.css). Import from here:
//   import { Button, Field, Banner, Modal, Amount, ... } from '../kit';
//
// Components (all forward `data-testid`; text comes from the caller, translated with `t()` from src/i18n):
//   Button        { variant: 'primary'|'secondary'|'danger'|'ghost', loading, small, block, disabled, ...button attributes }
//   Field         label + input/textarea/select + hint + error.  { label, hint, error, as: 'input'|'textarea'|'select', options, value, onValue(string),
//                 onInput(event), type, inputMode, placeholder, disabled, readOnly, suffix, 'data-testid' (on the CONTROL) }
//   Select        Field as="select" with { options: SelectOption[] }
//   Checkbox      { checked, onChange(boolean), label, hint }
//   FieldShell    render-prop wrapper for custom controls: children({ id, describedBy, invalid })
//   Segmented     radio-group toggle { options: [{ value, label, tone: 'buy'|'sell', 'data-testid' }], value, onChange, 'aria-label' }
//   Table         scrollable <table> wrapper { caption, dense, children }; TableMessage { colSpan } for empty / loading rows
//   Badge         { tone: 'ok'|'warn'|'bad'|'info'|'neutral', title }
//   Banner        { tone: 'info'|'ok'|'warn'|'error', title, actions, onDismiss };  ErrorBanner { error, onRetry }
//   Modal/Dialog  { title, onClose, footer, large, dismissable, hideClose } (render conditionally: mounted = open)
//   Spinner       { large };  Loading { text }
//   Amount        { kind: 'kas'|'token'|'price', value: bigint|null, decimals, scale, maxFraction, unit, group, signed }
//   CopyText      { value, short, head, tail } shortened id/address with a copy button;  copyToClipboard(text)
//   ToastHost     mount once (App does); show messages with showToast(message, tone, timeoutMs?)
//   Section       titled card { title, actions, collapsible, defaultOpen }
//   KeyValueList  <dl> { items: [{ label, value, 'data-testid', show }], compact }
//   Tabs          tab strip { tabs, active, onChange, 'aria-label' }
//
// Helpers: download.ts (downloadText, readFileText, exportFileName, jsonText), format.ts (shortId, shortAddress, kasText, tokenText, pricePerTokenText, amountText), hooks.ts (useAsync, useInterval, useNow),
// toast-store.ts (showToast, ToastStore).
export * from './download';
export * from './format';
export * from './hooks';
export { Amount, type AmountProps } from './Amount';
export { Badge, type BadgeProps, type Tone } from './Badge';
export { IssueLine, RawDetails } from './RawDetails';
export { Banner, ErrorBanner, type BannerProps, type BannerTone } from './Banner';
export { Button, type ButtonProps, type ButtonVariant } from './Button';
export { CopyText, copyToClipboard, type CopyTextProps } from './CopyText';
export { Checkbox, Field, FieldShell, Select, type FieldProps, type FieldShellProps, type SelectOption, type SelectProps } from './Field';
export { KeyValueList, type KeyValueItem } from './KeyValueList';
export { Dialog, Modal, type ModalProps } from './Modal';
export { Section, type SectionProps } from './Section';
export { Segmented, type SegmentedOption, type SegmentedProps } from './Segmented';
export { Loading, Spinner, type SpinnerProps } from './Spinner';
export { Table, TableMessage, type TableProps } from './Table';
export { Tabs, type TabItem } from './Tabs';
export { ToastHost, useToasts } from './Toast';
export { showToast, toasts, ToastStore, type ToastItem, type ToastTone } from './toast-store';
