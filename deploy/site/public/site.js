// Owns the website's dark default, optional light preference, and copy control.
// Runs before styles so an explicit light choice applies before the first paint.
// Without JavaScript or browser storage, the site remains usable and dark.
try {
  document.documentElement.dataset.theme = localStorage.getItem('roost.site.theme') === 'light' ? 'light' : 'dark';
} catch {
  document.documentElement.dataset.theme = 'dark';
}

document.addEventListener('DOMContentLoaded', () => {
  const root = document.documentElement;
  const themeButton = document.querySelector('[data-theme-toggle]');
  const updateThemeControl = () => {
    const dark = root.dataset.theme === 'dark';
    if (themeButton) themeButton.textContent = dark ? 'Light mode' : 'Dark mode';
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', dark ? '#232824' : '#f8f7ef');
  };
  if (themeButton) {
    themeButton.hidden = false;
    themeButton.addEventListener('click', () => {
      root.dataset.theme = root.dataset.theme === 'dark' ? 'light' : 'dark';
      try {
        localStorage.setItem('roost.site.theme', root.dataset.theme);
      } catch {
        // Storage can be disabled; the choice still applies to this page.
      }
      updateThemeControl();
    });
  }
  updateThemeControl();

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
});
