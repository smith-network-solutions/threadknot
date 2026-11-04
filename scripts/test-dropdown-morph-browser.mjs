// Run against Vite in the browser console:
// await (await import('/scripts/test-dropdown-morph-browser.mjs')).testDropdownMorph()
import React from 'react';
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { Dropdown } from '../src/components/alcove/Dropdown';
import { ContextMenu } from '../src/components/ContextMenu';

export async function testDropdownMorph() {
  const previousTheme = document.documentElement.dataset.theme;
  const matchMedia = window.matchMedia;
  const host = document.createElement('div');
  host.style.cssText = 'position:fixed;left:50%;top:50%;z-index:800';
  document.body.append(host);
  document.documentElement.dataset.theme = 'alcove';
  const root = createRoot(host);
  const results = [];
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const near = (a, b) => Math.abs(a - b) < 1;
  const sameBox = (a, b) => ['left', 'top', 'width', 'height'].every(key => near(a[key], b[key]));
  const panel = () => document.querySelector('.dropdown-morph-test');
  const trigger = () => host.querySelector('button');
  const click = element => flushSync(() => element.click());
  const pause = (element, time) => element.getAnimations({ subtree: true }).forEach(animation => {
    animation.pause();
    animation.currentTime = time === 'end' ? animation.effect.getTiming().duration : time;
  });
  const finish = async () => {
    panel()?.getAnimations({ subtree: true }).forEach(animation => animation.finish());
    await tick();
  };
  const render = (placement, label = 'Choose model', skinClass = '') => flushSync(() => root.render(
    React.createElement(React.StrictMode, null,
      React.createElement(Dropdown, {
        key: `${placement}:${skinClass}`, placement, menuClassName: `dropdown-morph-test ${skinClass}`,
        trigger: React.createElement('button', {
          style: { border: 0, borderRadius: 24, padding: '12px 24px', background: '#fff' },
        }, label),
      }, React.createElement('div', { className: 'attachment-menu__items' },
        ['First option', 'Second option', 'Third option'].map(text => React.createElement('button', {
          key: text, className: 'attachment-menu__option',
        }, text)))))));

  try {
    for (const placement of ['top-left', 'top-center', 'top-right', 'bottom-left', 'bottom-center',
      'bottom-right', 'left-top', 'left-bottom', 'right-top', 'right-bottom']) {
      render(placement);
      const source = trigger().getBoundingClientRect();
      click(trigger());
      pause(panel(), 0);
      assert(sameBox(panel().getBoundingClientRect(), source), `${placement}: must start at the button`);
      assert(getComputedStyle(host.querySelector('.dropdown-trigger')).opacity === '0', 'Duplicate button visible');
      assert(trigger().getAttribute('aria-expanded') === 'true', 'Expanded state missing');
      const content = panel().querySelector('.dropdown-content');
      const contentBox = content.getBoundingClientRect();
      pause(panel(), 210);
      assert(near(content.getBoundingClientRect().width, contentBox.width), 'Text reflowed during morph');
      await finish();
      const expanded = panel().getBoundingClientRect();
      assert(expanded.left >= 9 && expanded.right <= innerWidth - 9, `${placement}: horizontal overflow`);
      assert(expanded.top >= 9 && expanded.bottom <= innerHeight - 9, `${placement}: vertical overflow`);
      assert(!panel().hasAttribute('data-morphing'), 'Animation did not release layout');
      assert(content.style.width === '', 'Content width stayed frozen');
      flushSync(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })));
      pause(panel(), 'end');
      assert(sameBox(panel().getBoundingClientRect(), source), `${placement}: must close into the button`);
      await finish();
      assert(!panel(), 'Closed menu remained mounted');
      assert(getComputedStyle(host.querySelector('.dropdown-trigger')).opacity === '1', 'Button not restored');
      results.push(`${placement}: exact button geometry, stable text, clamping, Escape return`);
    }

    render('top-center');
    click(trigger());
    pause(panel(), 120);
    const openingBox = panel().getBoundingClientRect();
    click(trigger());
    pause(panel(), 0);
    assert(sameBox(panel().getBoundingClientRect(), openingBox), 'Close interrupted opening with a jump');
    pause(panel(), 80);
    const closingBox = panel().getBoundingClientRect();
    click(trigger());
    pause(panel(), 0);
    assert(sameBox(panel().getBoundingClientRect(), closingBox), 'Reopening interrupted close with a jump');
    await finish();
    click(panel().querySelector('.attachment-menu__option'));
    await finish();
    assert(!panel(), 'Selection failed to dismiss');
    results.push('Rapid reversal in both directions and item selection');

    click(trigger());
    await finish();
    flushSync(() => document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true })));
    await finish();
    assert(!panel(), 'Outside click failed to dismiss');
    results.push('Outside click dismissal');

    // Inspect the finish callback itself, before React has committed removal.
    // The previous implementation cancelled its fill here, exposing the full menu.
    click(trigger());
    pause(panel(), 80);
    const face = panel().querySelector('[aria-hidden="true"]');
    assert(parseFloat(getComputedStyle(face).filter.match(/[\d.]+/)?.[0]) > 0, 'Button face does not blur away');
    assert(parseFloat(getComputedStyle(panel().querySelector('.dropdown-content')).filter.match(/[\d.]+/)?.[0]) > 0,
      'Menu contents do not resolve from blur');
    await finish();
    click(trigger());
    pause(panel(), 240);
    const returningFace = panel().querySelector('[aria-hidden="true"]');
    assert(parseFloat(getComputedStyle(returningFace).opacity) > 0, 'Button face is not returning');
    assert(parseFloat(getComputedStyle(returningFace).filter.match(/[\d.]+/)?.[0]) > 0, 'Returning button does not resolve from blur');
    const closingPanel = panel();
    const collapsed = trigger().getBoundingClientRect();
    const motion = closingPanel.getAnimations().find(animation =>
      animation.effect.getKeyframes().some(frame => frame.width));
    let flashed = false;
    motion.addEventListener('finish', () => {
      if (!closingPanel.isConnected) return;
      flashed = !sameBox(closingPanel.getBoundingClientRect(), collapsed) ||
        getComputedStyle(closingPanel.querySelector('.dropdown-content')).opacity !== '0';
    });
    await finish();
    assert(!flashed, 'Menu flashed full-size between animation finish and React removal');
    assert(!panel(), 'Menu did not unmount after the collapsed-frame handoff');
    results.push('Icons blur out and resolve back in; collapsed frame survives the close callback');

    // Also let the browser complete a real timeline, rather than seeking it.
    click(trigger());
    await new Promise(resolve => setTimeout(resolve, 650));
    click(trigger());
    await new Promise(resolve => setTimeout(resolve, 500));
    assert(!panel(), 'Natural animation completion did not dismiss');
    assert(getComputedStyle(host.querySelector('.dropdown-trigger')).opacity === '1', 'Natural close left button hidden');
    results.push('Natural open/close completion restores the button');

    // Real composer triggers grow on press and settle while the menu is opening.
    // Check the exact handoff, including fractional layout and the copied SVG.
    host.className = 'composer-anchor';
    host.style.left = 'calc(50% + 0.375px)';
    host.style.top = 'calc(50% + 0.625px)';
    flushSync(() => root.render(React.createElement(Dropdown, {
      key: 'press-handoff', placement: 'top-left', menuClassName: 'dropdown-morph-test',
      menuStyle: { width: 298.375, padding: 10.375 },
      trigger: React.createElement('button', { className: 'composer-icon' },
        React.createElement('svg', { width: 25, height: 25, viewBox: '0 0 24 24' },
          React.createElement('path', { d: 'M12 5v14M5 12h14', stroke: 'currentColor', strokeWidth: 2 }))),
    }, React.createElement('button', { className: 'attachment-menu__option' }, 'Insert image'))));
    const exactBox = (a, b) => ['left', 'top', 'width', 'height'].every(key => Math.abs(a[key] - b[key]) < 0.05);
    const press = () => {
      const animation = trigger().animate([{ scale: 1.08 }, { scale: 1 }], { duration: 370, fill: 'both' });
      animation.pause();
      animation.currentTime = 0;
      return animation;
    };
    let release = press();
    const pressedIcon = trigger().querySelector('svg').getBoundingClientRect();
    click(trigger());
    pause(panel(), 0);
    assert(exactBox(panel().querySelector('[aria-hidden] svg').getBoundingClientRect(), pressedIcon),
      'Pressed trigger icon jumped when handed to the morph');
    pause(panel(), 'end');
    const finalOpenBox = panel().getBoundingClientRect();
    const finalRowBox = panel().querySelector('.attachment-menu__option').getBoundingClientRect();
    release.cancel();
    await finish();
    assert(exactBox(panel().getBoundingClientRect(), finalOpenBox), 'Menu moved when opening animation released layout');
    assert(exactBox(panel().querySelector('.attachment-menu__option').getBoundingClientRect(), finalRowBox),
      'Menu contents moved when opening animation released layout');
    release = press();
    click(trigger());
    pause(panel(), 'end');
    const finalCloseBox = panel().getBoundingClientRect();
    const returningIcon = panel().querySelector('[aria-hidden] svg').getBoundingClientRect();
    release.cancel();
    await finish();
    assert(exactBox(trigger().getBoundingClientRect(), finalCloseBox), 'Surface snapped to the resting trigger on close');
    assert(exactBox(trigger().querySelector('svg').getBoundingClientRect(), returningIcon),
      'Returning icon snapped when handed back to the trigger');
    results.push('Pressed composer trigger and fractional menu geometry match across both handoffs (within 0.05px)');

    for (const skinClass of ['model-menu', 'attachment-menu', 'inbox-menu', 'sidebar-view-pop', 'thread-menu ws-menu', 'ctx-menu new-chat-menu']) {
      render('bottom-right', 'Menu', skinClass);
      click(trigger());
      await finish();
      click(trigger());
      pause(panel(), 'end');
      assert(!panel().getAnimations().some(animation => animation instanceof CSSAnimation), `${skinClass}: legacy animation competes with the morph`);
      assert(exactBox(panel().getBoundingClientRect(), trigger().getBoundingClientRect()), `${skinClass}: close misses trigger geometry`);
      assert(getComputedStyle(panel()).getPropertyValue('corner-shape') === getComputedStyle(trigger()).getPropertyValue('corner-shape'),
        `${skinClass}: menu's squircle did not return to the trigger's round corners`);
      await finish();
    }
    results.push('Every menu skin closes to the original geometry and corner shape, without competing CSS animation');

    // New chat owns its trigger in App, and its menu in Sidebar. Exercise that
    // external-anchor path, including keyboard selection and delayed action.
    flushSync(() => root.render(null));
    const slot = document.createElement('span');
    slot.style.display = 'inline-flex';
    const external = document.createElement('button');
    external.style.cssText = 'width:50px;height:50px;border-radius:50%;border:0';
    external.textContent = '+';
    slot.append(external);
    document.body.append(slot);
    let selected = false;
    const mountContext = () => flushSync(() => root.render(React.createElement(ContextMenu, {
      x: 0, y: 0, anchor: external, className: 'new-chat-menu dropdown-morph-test',
      items: [{ label: 'Quick thread', onSelect: () => { selected = true; } }],
      onClose: () => root.render(null),
    })));
    try {
      mountContext();
      pause(panel(), 0);
      assert(exactBox(panel().getBoundingClientRect(), external.getBoundingClientRect()), 'External menu did not start at its trigger');
      assert(external.style.opacity === '0', 'External trigger was not hidden');
      await finish();
      assert(document.activeElement === panel().querySelector('[role=menuitem]'), 'Context menu did not take keyboard focus');
      click(panel().querySelector('[role=menuitem]'));
      assert(!selected, 'Selection interrupted the closing morph');
      pause(panel(), 'end');
      assert(exactBox(panel().getBoundingClientRect(), external.getBoundingClientRect()), 'External menu closed to the wrong box');
      await finish();
      assert(selected && !panel() && external.style.opacity === '', 'External menu did not finish selection and restore its trigger');
      mountContext();
      await finish();
      flushSync(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })));
      await finish();
      assert(!panel() && external.style.opacity === '', 'External menu Escape did not restore the trigger');
    } finally {
      flushSync(() => root.render(null));
      slot.remove();
    }
    results.push('New chat external trigger: focus, selection, Escape, rounded return and cleanup');
    render('top-center');

    // The preference path should not wait for an animation callback to unmount.
    window.matchMedia = query => query === '(prefers-reduced-motion: reduce)'
      ? { matches: true } : matchMedia.call(window, query);
    click(trigger());
    assert(panel() && !panel().hasAttribute('data-morphing'), 'Reduced motion still animates');
    assert(panel().getAnimations().length === 0, 'Reduced motion created animation');
    click(trigger());
    await tick();
    assert(!panel(), 'Reduced motion did not dismiss immediately');
    results.push('Reduced motion opens and dismisses immediately');
    return results;
  } finally {
    flushSync(() => root.unmount());
    host.remove();
    window.matchMedia = matchMedia;
    if (previousTheme === undefined) delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = previousTheme;
  }
}
