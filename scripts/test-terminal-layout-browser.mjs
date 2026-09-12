// Run from the Vite browser console:
// await (await import('/scripts/test-terminal-layout-browser.mjs')).testTerminalLayout()
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import '../src/styles/terminal.css';

export async function testTerminalLayout() {
  const results = [];
  for (const width of [390, 1100]) {
    const pane = document.createElement('div');
    pane.style.cssText = `position:fixed;top:0;left:0;width:${width}px;height:500px;z-index:2147483647`;
    pane.innerHTML = '<div class="term-instance"><div class="term-frame"><div class="term-host"></div></div><div class="term-keys"><button>Esc</button><button>Paste</button></div></div>';
    document.body.append(pane);
    const host = pane.querySelector('.term-host');
    const keys = pane.querySelector('.term-keys');
    keys.style.display = width === 390 ? 'flex' : 'none';
    const term = new Terminal({fontSize: 14, cols: 80, rows: 24});
    const fit = new FitAddon();
    term.loadAddon(fit);
    try {
      term.open(host);
      // Shrink and grow as the soft keyboard opens/closes or a panel resizes.
      for (const height of [500, 237, 180, 420]) {
        pane.style.height = `${height}px`;
        fit.fit();
        await new Promise(resolve => term.write('\x1b[2J\x1b[999;1Hprompt $ ', resolve));
        const screen = host.querySelector('.xterm-screen').getBoundingClientRect();
        const bounds = host.getBoundingClientRect();
        const rowVisible = screen.bottom <= bounds.bottom + 0.5;
        const keysClear = width !== 390 || screen.bottom <= keys.getBoundingClientRect().top;
        const widthFits = screen.right <= bounds.right + 0.5;
        if (!rowVisible || !keysClear || !widthFits) throw new Error(`Clipped grid at ${width}x${height}: ${JSON.stringify({screenBottom:screen.bottom,hostBottom:bounds.bottom,keysTop:keys.getBoundingClientRect().top})}`);
        results.push(`${width}x${height}: prompt fits (${term.rows} rows)`);
      }
    } finally { term.dispose(); pane.remove(); }
  }
  return results;
}
