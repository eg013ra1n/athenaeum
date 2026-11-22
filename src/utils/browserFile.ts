// Browser-side file helpers for the web build (no native dialogs): save a text as a download, read a chosen file.

/** Offer `text` as a file download named `fileName`. */
export function downloadTextFile(fileName: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: 'application/json;charset=utf-8' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = fileName;
  a.style.display = 'none';
  document.body.appendChild(a);
  a.click();
  a.remove();
  // The download has started by the time the click returns; release the blob after the browser has taken it.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** The text of a file the user chose in a file input. */
export function readFileText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(typeof reader.result === 'string' ? reader.result : '');
    reader.onerror = () => reject(reader.error ?? new Error('The file could not be read.'));
    reader.readAsText(file);
  });
}
