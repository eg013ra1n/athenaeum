import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import { MemberDot } from '../../ui';
import { MemberColorsProvider, useMemberColor } from './MemberColorsContext';
import { MEMBER_PALETTE } from './memberColors';

const members = [
  { accountId: 'acc-me', displayName: 'Me' },
  { accountId: 'acc-a', displayName: 'Alice' },
];

function Probe({ keys }: { keys: (string | null)[] }) {
  const colorOf = useMemberColor();
  return (
    <>
      {keys.map((k, i) => (
        <span key={i} data-testid={`k${i}`} data-color={colorOf(k) ?? 'none'}>
          <MemberDot color={colorOf(k)} />
        </span>
      ))}
    </>
  );
}

describe('MemberColorsProvider', () => {
  it('an unknown key (null, unnamed device, departed member) gets no colour, never the accent', () => {
    const { getByTestId } = render(
      <MemberColorsProvider members={members} selfAccountId="acc-me">
        <Probe keys={[null, 'dev-unnamed', 'acc-gone', 'acc-me', 'Alice', 'acc-a']} />
      </MemberColorsProvider>,
    );
    const color = (i: number) => getByTestId(`k${i}`).dataset.color;
    expect(color(0)).toBe('none');
    expect(color(1)).toBe('none');
    expect(color(2)).toBe('none');
    expect(color(3)).toBe(MEMBER_PALETTE[0]); // this account: the accent
    expect(color(4)).toBe(MEMBER_PALETTE[1]); // by display name
    expect(color(5)).toBe(MEMBER_PALETTE[1]); // by account id
  });

  it('this account keeps the accent even before the member list names it', () => {
    const { getByTestId } = render(
      <MemberColorsProvider members={[]} selfAccountId="acc-me">
        <Probe keys={['acc-me']} />
      </MemberColorsProvider>,
    );
    expect(getByTestId('k0').dataset.color).toBe(MEMBER_PALETTE[0]);
  });

  it('without a provider every key is unknown', () => {
    const { getByTestId } = render(<Probe keys={['acc-a']} />);
    expect(getByTestId('k0').dataset.color).toBe('none');
  });
});

describe('MemberDot', () => {
  it('an unknown member is the neutral border dot with no inline colour', () => {
    const { container } = render(<MemberDot />);
    const dot = container.firstChild as HTMLElement;
    expect(dot.className).toContain('bg-border');
    expect(dot.getAttribute('style')).toBeNull();
  });
  it('a known member is painted in its colour', () => {
    const { container } = render(<MemberDot color="#a3be8c" />);
    const dot = container.firstChild as HTMLElement;
    expect(dot.style.backgroundColor).toBe('rgb(163, 190, 140)');
    expect(dot.className).not.toContain('bg-border');
  });
});
