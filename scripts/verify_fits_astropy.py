#!/usr/bin/env python3
"""Dev-only: validate an athenaeum-written FITS against astropy (reference impl).
Not part of CI — requires astropy installed. Usage:
    python3 scripts/verify_fits_astropy.py <file.fits>"""
import sys
from astropy.io import fits

with fits.open(sys.argv[1]) as hdul:
    hdul.verify('exception')
    print("OK:", repr(hdul[0].header))
