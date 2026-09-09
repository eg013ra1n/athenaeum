# Sky chart inspection and DSS background

Click a footprint to inspect its exposure count, integration, filters, dates and cameras, then open its object. Grouping, exposure count and integration time have separate colour legends.

The date filter selects fields; totals cover the complete field. Confirmed processing versions count once, and integrated products are excluded from single-exposure totals. Rectangle selections retain every file ID for operations while calculating integration from exposure representatives.

The optional DSS colored HiPS background uses Aladin Lite in an isolated same-origin frame, synchronized to the equatorial stereographic projection. It needs WebGL 2 and remote survey access. Bundled licence notices are in `public/licenses/`.

Projection tests compare the HiPS field-of-view conversion with bundled D3. Native WebGL rendering and interaction remain part of visual acceptance testing.
