// The twelve catalog fields whose header keywords the user orders (Settings → General → FITS keywords).
// Display order, labels and the built-in chains live here so the registry (search keywords) and the section
// share one list. The chains mirror the Rust defaults (`header_fallbacks::default_chain`); the section reads the
// real defaults from `get_settings_defaults` and uses these only while that call has not answered.

import type { HeaderField } from '../types/models';

export const HEADER_FIELDS: HeaderField[] = [
  'ccdTemp',
  'setTemp',
  'gain',
  'offset',
  'exptime',
  'focallen',
  'xpixsz',
  'ypixsz',
  'object',
  'filter',
  'instrume',
  'telescop',
];

export const HEADER_FIELD_LABELS: Record<HeaderField, string> = {
  ccdTemp: 'Sensor temperature',
  setTemp: 'Set temperature',
  gain: 'Gain',
  offset: 'Offset',
  exptime: 'Exposure',
  focallen: 'Focal length',
  xpixsz: 'Pixel size X',
  ypixsz: 'Pixel size Y',
  object: 'Object',
  filter: 'Filter',
  instrume: 'Camera',
  telescop: 'Telescope',
};

export const DEFAULT_HEADER_CHAINS: Record<HeaderField, string[]> = {
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

/** The catalog column behind each field (snake_case), used as a search term. */
export const HEADER_FIELD_COLUMNS: Record<HeaderField, string> = {
  ccdTemp: 'ccd_temp',
  setTemp: 'set_temp',
  gain: 'gain',
  offset: 'offset',
  exptime: 'exptime',
  focallen: 'focallen',
  xpixsz: 'xpixsz',
  ypixsz: 'ypixsz',
  object: 'object',
  filter: 'filter',
  instrume: 'instrume',
  telescop: 'telescop',
};

/** Plain-language synonyms a user may type into the settings search. */
export const HEADER_FIELD_SYNONYMS: Partial<Record<HeaderField, string[]>> = {
  ccdTemp: ['temperature'],
  setTemp: ['temperature', 'cooling'],
  exptime: ['exposure'],
  focallen: ['focal length'],
  xpixsz: ['pixel size'],
  ypixsz: ['pixel size'],
  object: ['target'],
  instrume: ['camera'],
  telescop: ['telescope'],
};
