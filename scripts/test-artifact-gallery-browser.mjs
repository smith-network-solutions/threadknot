// Run in the Vite browser console:
// await (await import('/scripts/test-artifact-gallery-browser.mjs')).testArtifactGallery()
import React from 'react';
import { createRoot } from 'react-dom/client';
import { ArtifactPreview } from '../src/components/artifacts/ArtifactPreview';
import { ArtifactNavigation } from '../src/components/artifacts/ArtifactNavigation';
import '../src/styles.css';
import '../src/styles/files.css';

export async function testArtifactGallery() {
  const wait = ms => new Promise(r => setTimeout(r, ms));
  const check = (pass, message) => { if (!pass) throw new Error(message); };
  const image = 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="375" height="4000"><rect width="375" height="4000" fill="white"/><rect width="375" height="100" fill="red"/><rect y="3900" width="375" height="100" fill="blue"/></svg>');
  const results = [];
  for (const width of [390, 1100]) {
    const host = document.createElement('div');
    host.style.cssText = `position:fixed;inset:0;width:${width}px;height:500px;z-index:2147483647;background:white`;
    document.body.append(host);
    let current = 0;
    function App() {
      const [index, select] = React.useState(0);
      current = index;
      return React.createElement(ArtifactNavigation, {index, count: 3, onSelect: select},
        React.createElement('div', {className: 'artifact-lightbox-body'},
          index === 1 ? React.createElement('pre', {style: {height: 1500}}, 'A text artifact') : React.createElement(ArtifactPreview, {
            key: index, mode: 'full', url: image,
            artifact: {id: String(index), name: 'Tall screenshot', relPath: 'mobile.svg', mimeType: 'image/svg+xml', sizeBytes: 0, op: 'created'},
          })));
    }
    const root = createRoot(host);
    root.render(React.createElement(App));
    let pointerId = 1;
    const swipe = async (dx, dy = 0) => {
      const target = host.querySelector('.artifact-zoom-stage') ?? host.querySelector('pre');
      for (const [type, x, y] of [['pointerdown', 160, 200], ['pointerup', 160 + dx, 200 + dy]]) {
        target.dispatchEvent(new PointerEvent(type, {bubbles: true, cancelable: true, pointerType: 'touch', pointerId, isPrimary: true, clientX:x, clientY:y}));
      }
      pointerId++;
      await wait(30);
    };
    try {
      await wait(100);
      const img = host.querySelector('img');
      await img.decode();
      const stage = host.querySelector('.artifact-zoom-stage').getBoundingClientRect();
      const bounds = img.getBoundingClientRect();
      check(bounds.top >= stage.top - .5 && bounds.bottom <= stage.bottom + .5, 'Tall image escapes fitted stage');
      check(getComputedStyle(img).objectFit === 'contain', 'Fit must show the entire image');
      await swipe(-100);
      check(current === 1, 'Swipe to text artifact');
      await swipe(0, 150);
      check(current === 1, 'Vertical reading gesture must not navigate');
      await swipe(-100);
      check(current === 2, 'Swipe to next image');
      host.querySelector('[aria-label="Zoom in"]').click();
      await wait(30);
      await swipe(100);
      check(current === 2, 'Zoomed image swipe must remain a pan');
      host.querySelector('[aria-label="Fit to window"]').click();
      await wait(30);
      await swipe(100);
      check(current === 1, 'Fit restores swipe navigation');
      host.querySelector('[aria-label="Previous artifact"]').click();
      await wait(30);
      check(current === 0 && host.querySelector('[aria-label="Previous artifact"]').disabled, 'Previous button and boundary');
      results.push(`${width}px: tall image fits; swipe, vertical scroll, zoom and navigation buttons pass`);
    } finally { root.unmount(); host.remove(); }
  }
  return results;
}
