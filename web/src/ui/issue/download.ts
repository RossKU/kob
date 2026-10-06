// Browser download of a text file (Blob + a[download]). DOM-only helper kept out of the pure model.
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
  // revoke after the browser has started the download
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
