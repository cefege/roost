// Owns the optional install-command clipboard enhancement.
// The page and selectable command remain usable without JavaScript.
// Success is announced only after the browser confirms a clipboard write.
const copyStatus = document.getElementById('copy-status');
for (const button of document.querySelectorAll('[data-copy]')) {
  button.hidden = false;
  button.addEventListener('click', async () => {
    const command = document.getElementById(button.dataset.copy);
    try {
      await navigator.clipboard.writeText(command.textContent);
      copyStatus.textContent = 'Copied. Paste it into a terminal on your first machine.';
    } catch {
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(command);
      selection.removeAllRanges();
      selection.addRange(range);
      copyStatus.textContent = 'Clipboard unavailable. The command is selected; copy it with your browser.';
    }
  });
}
