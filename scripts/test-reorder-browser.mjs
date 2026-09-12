// With Vite running, execute in its browser console:
// await (await import('/scripts/test-reorder-browser.mjs')).testReorder()
// Exercises the real hook with synthetic touch pointers; real-device gesture
// arbitration still needs a phone check.
import React from 'react';
import { createRoot } from 'react-dom/client';
import { useReorderDrag } from '../src/lib/reorder';

export async function testReorder() {
  const host = document.createElement('div');
  host.style.cssText = 'position:fixed;inset:0;z-index:2147483647;background:white';
  document.body.append(host);
  let commits = 0, clicks = 0, menus = 0;
  const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
  const check = (condition, label) => { if (!condition) throw new Error(label); };
  function Fixture() {
    const ref = React.useRef(null);
    const [ids, setIds] = React.useState(['a', 'b', 'c']);
    const drag = useReorderDrag({ containerRef: ref, onCommit: ids => { commits++; setIds(ids); } });
    return React.createElement('div', { ref, style: { height: 300, overflow: 'auto' } },
      ...ids.map(id => React.createElement('div', {
        key: id, 'data-reorder-id': id, 'data-dragging': drag.draggingId === id,
        ...drag.handleProps(id), onClick: () => { clicks++; }, style: { height: 70 },
      }, id, React.createElement('button', {onClick: e => { e.stopPropagation(); menus++; }}, 'Menu'))));
  }
  const root = createRoot(host);
  root.render(React.createElement(Fixture));
  let pid = 0;
  const row = id => host.querySelector(`[data-reorder-id="${id}"]`);
  const order = () => [...host.querySelectorAll('[data-reorder-id]')].map(n => n.dataset.reorderId).join('');
  const pointer = (node, type, y) => node.dispatchEvent(new PointerEvent(type, {
    bubbles: true, cancelable: true, pointerType: 'touch', pointerId: pid, button: 0, clientX: 20, clientY: y,
  }));
  try {
    await wait(50);
    pid++;
    pointer(row('a'), 'pointerdown', 35);
    await wait(350);
    check(row('a').dataset.dragging === 'true', 'hold activates drag');
    const movement = new Event('touchmove', { bubbles: true, cancelable: true });
    window.dispatchEvent(movement);
    check(movement.defaultPrevented, 'active drag blocks native scrolling');
    pointer(window, 'pointermove', 190);
    pointer(window, 'pointerup', 190);
    await wait(20);
    row('a').click();
    check(order() === 'bca' && commits === 1 && clicks === 0, 'drop downward and suppress ghost click');
    pid++;
    pointer(row('a'), 'pointerdown', 175);
    await wait(350);
    pointer(window, 'pointermove', 10);
    pointer(window, 'pointerup', 10);
    await wait(20);
    check(order() === 'abc' && commits === 2, 'drop upward');
    pid++;
    const menu = row('a').querySelector('button');
    pointer(menu, 'pointerdown', 35);
    await wait(350);
    check(!document.body.classList.contains('reordering'), 'nested menu never starts drag');
    pointer(window, 'pointerup', 35);
    menu.click();
    check(menus === 1 && clicks === 0, 'menu works immediately after drag');
    pid++;
    pointer(row('b'), 'pointerdown', 100);
    pointer(window, 'pointermove', 130);
    await wait(350);
    pointer(window, 'pointerup', 130);
    await wait(20);
    check(commits === 2 && !document.body.classList.contains('reordering'), 'swipe before hold remains scroll');
    pid++;
    pointer(row('b'), 'pointerdown', 100);
    pointer(window, 'pointerup', 100);
    await wait(20);
    row('b').click();
    check(clicks === 1, 'new deliberate tap opens normally');
    pid++;
    pointer(row('c'), 'pointerdown', 175);
    await wait(350);
    pointer(window, 'pointercancel', 175);
    check(!document.body.classList.contains('reordering') && commits === 2, 'cancel cleans up without reorder');
    return 'PASS: touch hold, reorder both directions, ghost click, swipe, tap, cancellation';
  } finally {
    root.unmount();
    host.remove();
  }
}
