/**
 * Starts a browser download of a text file with a Blob and `a[download]`. It is DOM-only and kept
 * out of the pure model.
 */
export function downloadText(filename: string, text: string, mime = 'application/json'): void {
  const url = URL.createObjectURL(new Blob([text], { type: `${mime};charset=utf-8` }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.rel = 'noopener';
  a.style.display = 'none';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  // Revoke after the browser has started the download.
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
