import { describe, expect, it } from 'vitest';
import { fieldMeta, SETTINGS_SECTIONS, SETTINGS_TABS, sectionById } from './registry';

describe('SETTINGS_SECTIONS', () => {
  it('has unique section ids', () => {
    const ids = SETTINGS_SECTIONS.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('has a tab for every section that exists in SETTINGS_TABS', () => {
    const tabIds = new Set(SETTINGS_TABS.map((t) => t.id));
    for (const section of SETTINGS_SECTIONS) {
      expect(tabIds.has(section.tab), `section "${section.id}" has unknown tab "${section.tab}"`).toBe(true);
    }
  });

  it('has unique field ids within each section', () => {
    for (const section of SETTINGS_SECTIONS) {
      const ids = section.fields.map((f) => f.id);
      expect(new Set(ids).size, `section "${section.id}" has duplicate field ids`).toBe(ids.length);
    }
  });

  it('never has an empty section title', () => {
    for (const section of SETTINGS_SECTIONS) {
      expect(section.title.trim().length, `section "${section.id}" has an empty title`).toBeGreaterThan(0);
    }
  });

  it('never has an empty field label', () => {
    for (const section of SETTINGS_SECTIONS) {
      for (const field of section.fields) {
        expect(
          field.label.trim().length,
          `field "${section.id}/${field.id}" has an empty label`,
        ).toBeGreaterThan(0);
      }
    }
  });
});

describe('sectionById', () => {
  it('returns the matching section', () => {
    expect(sectionById('general.updates').title).toBe('Updates');
  });
  it('throws on a miss', () => {
    expect(() => sectionById('nonexistent.section')).toThrow();
  });
});

describe('fieldMeta', () => {
  it('returns the matching field', () => {
    expect(fieldMeta('general.updates', 'autoCheck').label).toBe(
      'Automatically check for updates on startup',
    );
  });
  it('throws on a miss', () => {
    expect(() => fieldMeta('general.updates', 'nonexistent')).toThrow();
    expect(() => fieldMeta('nonexistent.section', 'x')).toThrow();
  });
});
