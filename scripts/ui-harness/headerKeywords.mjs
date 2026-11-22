// `header-keywords` scenario: Settings → General → FITS keywords (`/settings`). The five commands the section calls.
// The default document is the twelve built-in chains (`HeaderFallbackConfig`); the preview carries non-zero counts
// (`HeaderFallbackCounts`) with a calibration regroup and kept values, or none with `HARNESS_PREVIEW=empty`.
// `set_header_fallbacks` echoes the document it received, `reset_header_fallbacks` and `apply_header_fallbacks`
// answer as the real ones do; nothing is stored, so a reload brings the default chains back and the preview line
// stays after Apply. The harness has no events, so `header-fallbacks-progress` never arrives.

const CHAINS = {
  ccdTemp: ['CCD-TEMP', 'DET-TEMP'],
  setTemp: ['SET-TEMP'],
  gain: ['GAIN'],
  offset: ['OFFSET'],
  exptime: ['EXPTIME', 'EXPOSURE'],
  focallen: ['FOCALLEN'],
  xpixsz: ['XPIXSZ', 'PIXSIZE1'],
  ypixsz: ['YPIXSZ', 'PIXSIZE2'],
  object: ['OBJECT'],
  filter: ['FILTER'],
  instrume: ['INSTRUME'],
  telescop: ['TELESCOP'],
};

const defaultDocument = () => ({
  fields: Object.fromEntries(Object.entries(CHAINS).map(([f, order]) => [f, { order: [...order], retired: [] }])),
});

export function headerKeywordsHandlers(mode = 'counts') {
  const counts = mode === 'empty'
    ? { byField: [], framesChanged: 0, valuesKept: 0, setsRegrouped: 0, framesWithoutHeader: 0 }
    : {
        byField: [{ field: 'ccdTemp', frames: 290 }, { field: 'exptime', frames: 22 }, { field: 'xpixsz', frames: 14 }],
        framesChanged: 312,
        valuesKept: 9,
        setsRegrouped: 3,
        framesWithoutHeader: 2,
      };
  return {
    get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDocument() }),
    get_header_fallbacks: () => defaultDocument(),
    set_header_fallbacks: (args) => {
      const changed = Object.entries(args.config?.fields ?? {})
        .filter(([f, r]) => JSON.stringify(r.order) !== JSON.stringify(CHAINS[f]))
        .map(([f, r]) => `${f}: ${r.order.join(', ')}`);
      console.log(`[harness] set_header_fallbacks ${changed.length ? changed.join(' | ') : '(defaults)'}`);
      return args.config;
    },
    reset_header_fallbacks: () => defaultDocument(),
    preview_header_fallbacks: () => counts,
    apply_header_fallbacks: () => counts,
  };
}
