// Paste into the page (Chrome javascript tool or DevTools) on BOTH the mockup and
// the harness; diff the two results by text. Geometry in CSS px, rounded.
(() => {
  const vis = (e) => e.offsetParent !== null || getComputedStyle(e).position === 'fixed';
  const r = (e) => {
    const b = e.getBoundingClientRect();
    const cs = getComputedStyle(e);
    return { x: Math.round(b.left), y: Math.round(b.top), w: Math.round(b.width), h: Math.round(b.height), fs: cs.fontSize, fw: cs.fontWeight, color: cs.color, bg: cs.backgroundColor };
  };
  const txt = (e) => e.textContent.replace(/\s+/g, ' ').trim().slice(0, 48);
  const pick = (sel, n = 60) => [...document.querySelectorAll(sel)].filter(vis).slice(0, n).map((e) => ({ t: txt(e), ...r(e) }));
  const rows = [...document.querySelectorAll('tbody tr')].filter(vis);
  return JSON.stringify({
    th: pick('th'),
    rows: rows.slice(0, 8).map((e) => ({ t: txt(e), ...r(e) })),
    cells: rows[3] ? [...rows[3].children].map((e) => ({ t: txt(e), ...r(e) })) : [],
    headings: pick('h1,h2,h3'),
    tabs: pick('[role=tab]'),
    buttons: pick('button', 40),
  });
})();
