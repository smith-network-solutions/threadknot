// With Vite running, execute in the browser console:
// await (await import('/scripts/test-terminal-paste-browser.mjs')).testTerminalPaste()
// Uses the installed xterm and the actual component keyboard handler. Synthetic
// clipboard events model the browser default only when keydown wasn't cancelled.
import { Terminal } from '@xterm/xterm';
import source from '../src/components/terminal/TerminalInstance.tsx?raw';

export async function testTerminalPaste() {
  const marker = 'term.attachCustomKeyEventHandler((e) => {';
  const start = source.indexOf(marker) + marker.length;
  const end = source.indexOf('\n    });', start);
  if (start < marker.length || end < 0) throw new Error('Keyboard handler not found');
  const handleKey = new Function('term', 'copyText', 'pasteFromClipboard', 'isNativeShell', 'e', source.slice(start, end));
  const results = [];
  for (const native of [false, true]) {
    for (const shortcut of ['ctrl-shift-v', 'cmd-v', 'ctrl-v']) {
      const host = document.createElement('div');
      document.body.append(host);
      const term = new Terminal({cols: 40, rows: 5});
      const sent = [];
      let reads = 0;
      try {
        term.open(host);
        term.onData(data => sent.push(data));
        await new Promise(resolve => term.write('\x1b[?2004h', resolve));
        term.attachCustomKeyEventHandler(event => handleKey(term, () => {}, () => {
          reads++;
          Promise.resolve().then(() => term.paste('paste-test'));
        }, () => native, event));
        const input = host.querySelector('textarea');
        const key = new KeyboardEvent('keydown', {
          key: 'v', code: 'KeyV', keyCode: 86, bubbles: true, cancelable: true,
          ctrlKey: shortcut !== 'cmd-v', metaKey: shortcut === 'cmd-v', shiftKey: shortcut === 'ctrl-shift-v',
        });
        input.dispatchEvent(key);
        if (!key.defaultPrevented) {
          const clipboardData = new DataTransfer();
          clipboardData.setData('text/plain', 'paste-test');
          input.dispatchEvent(new ClipboardEvent('paste', {clipboardData, bubbles: true, cancelable: true}));
        }
        input.dispatchEvent(new KeyboardEvent('keyup', {key: 'v', bubbles: true}));
        await new Promise(resolve => setTimeout(resolve, 20));
        const expected = shortcut === 'ctrl-v' ? '\x16' : '\x1b[200~paste-test\x1b[201~';
        if (sent.length !== 1 || sent[0] !== expected) throw new Error(`${native ? 'desktop' : 'browser'} ${shortcut}: ${JSON.stringify(sent)}`);
        if (!native && reads !== 0) throw new Error('Browser keyboard paste must not require clipboard read permission');
        results.push(`${native ? 'desktop' : 'browser'} ${shortcut}: ${shortcut === 'ctrl-v' ? 'literal-next preserved' : 'one bracketed paste'}`);
      } finally { term.dispose(); host.remove(); }
    }
  }
  return results;
}
