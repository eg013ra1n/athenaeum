import { describe, expect, it } from 'vitest';
import css from './index.css?raw';

/** The body of the first rule whose selector list is exactly `selector`. */
function ruleBody(selector: string): string {
  const at = css.indexOf(`${selector} {`);
  expect(at, `rule "${selector}" exists`).toBeGreaterThanOrEqual(0);
  return css.slice(at, css.indexOf('}', at));
}

describe('index.css — spec §14 rules', () => {
  it('reduced motion turns transitions off but keeps animations (busy spinners)', () => {
    const at = css.indexOf('@media (prefers-reduced-motion: reduce)');
    expect(at).toBeGreaterThanOrEqual(0);
    const block = css.slice(at, css.indexOf('}', at));
    expect(block).toContain('transition: none !important');
    expect(block).not.toContain('animation');
  });

  it('select:focus leaves focus to the base focus-visible outline (no outline: none, no ring)', () => {
    const body = ruleBody('select:focus');
    expect(body).not.toMatch(/outline/);
    expect(body).not.toMatch(/box-shadow/);
    expect(css).toMatch(/select:focus-visible[^{]*\{\s*@apply outline outline-2 outline-offset-1 outline-accent;/);
  });

  it('the native checkbox focus rule is untouched', () => {
    const body = ruleBody('input[type="checkbox"]:focus');
    expect(body).toContain('outline: none');
    expect(body).toContain('box-shadow');
  });
});
