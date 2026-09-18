// Settings redesign (spec 2026-09-18 §3/§5): the wire↔typed converters every
// KV field goes through via `useSettingField`. A `Codec<T>` never throws —
// `parse` returns the value or an `Error` carrying the message the field
// shows inline, so a bad draft is diagnosable without a try/catch at every
// call site.

export interface Codec<T> {
  parse(raw: string): T | Error;
  format(value: T): string;
  /** Extra validation beyond what `parse` can express (e.g. cross-field). */
  validate?(value: T): string | null;
}

/** `'true' | 'false'` on the wire — discrete controls only ever write one of
 *  the two through `setValue`, but `parse` stays honest about anything else
 *  a hand-edited row or a future format change might contain. */
export const boolCodec: Codec<boolean> = {
  parse(raw) {
    if (raw === 'true') return true;
    if (raw === 'false') return false;
    return new Error('Must be true or false');
  },
  format(value) {
    return value ? 'true' : 'false';
  },
};

/** Whole numbers in `[min, max]`, inclusive. */
export function intCodec(min: number, max: number): Codec<number> {
  return {
    parse(raw) {
      const trimmed = raw.trim();
      if (trimmed === '') return new Error(`Must be a whole number between ${min} and ${max}`);
      const n = Number(trimmed);
      if (!Number.isFinite(n) || !Number.isInteger(n)) {
        return new Error(`Must be a whole number between ${min} and ${max}`);
      }
      if (n < min || n > max) {
        return new Error(`Must be a whole number between ${min} and ${max}`);
      }
      return n;
    },
    format(value) {
      return String(value);
    },
  };
}

/** Real numbers in `[min, max]`, inclusive. `step` is advisory only (the
 *  `<input step>` attribute a caller may render) — it never rejects a valid
 *  value that doesn't fall on the step grid. */
export function floatCodec(min: number, max: number, _step?: number): Codec<number> {
  return {
    parse(raw) {
      const trimmed = raw.trim();
      if (trimmed === '') return new Error(`Must be a number between ${min} and ${max}`);
      const n = Number(trimmed);
      if (!Number.isFinite(n)) return new Error(`Must be a number between ${min} and ${max}`);
      if (n < min || n > max) return new Error(`Must be a number between ${min} and ${max}`);
      return n;
    },
    format(value) {
      return String(value);
    },
  };
}

/** A plain string, optionally capped at `maxLen` characters. Empty is a
 *  valid string (callers requiring non-empty add that as `validate`). */
export function stringCodec(maxLen?: number): Codec<string> {
  return {
    parse(raw) {
      if (maxLen != null && raw.length > maxLen) {
        return new Error(`Must be ${maxLen} characters or fewer`);
      }
      return raw;
    },
    format(value) {
      return value;
    },
  };
}

/** One of a fixed set of string values (a `<select>`'s options). */
export function enumCodec<T extends string>(values: readonly T[]): Codec<T> {
  return {
    parse(raw) {
      if ((values as readonly string[]).includes(raw)) return raw as T;
      return new Error(`Must be one of ${values.join(', ')}`);
    },
    format(value) {
      return value;
    },
  };
}
