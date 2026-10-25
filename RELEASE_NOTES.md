*Athenaeum v0.7.0: shoot one target together with friends, stack faster, and see at a glance which frames to throw away.*

This update arrives inside the app: click **Download and install** when it is offered. If you take part in
a collaboration project, update soon — older versions show **Update required** on every project.

## What's New

### Collaboration projects, rebuilt

- **Share frames as soon as they are ready.** Calibrate your night, look through it in Blink, and publish
  the frames you want to share. Everyone else in the project receives them within seconds.
- **See the whole project on one page:** how much you have contributed, how close each filter is to the
  project's goal, who is online, and what is being exchanged right now.
- **Choose how hands-off you want to be.** Publish by hand (the default), let the app calibrate for you, or
  make it fully automatic.
- **Different filter names are no longer a problem.** If your filter is called "Ha" and the project's is
  "H-alpha", map it once and the app remembers. Frames without a filter name can be shared too.
- **Already calibrated somewhere else?** Mark the frame set **Calibrated externally** and share it as it is.
- **For coordinators:** approve or reject frames in batches, set a frame aside and bring it back, and fix a
  member's filter names yourself.
- **Export the whole project** for stacking, sorted into one folder per filter.

### Stacking

- **Noticeably faster** — up to a quarter less time for the same result.
- **Local normalization sets its own scale** to fit your image — one setting less to guess.
- **Frame sets calibrated elsewhere** stack without asking for darks or flats.
- **A heads-up when a light has a flat but no dark or bias**, before you stack.

### Choosing your best frames

- **A plain verdict for every frame:** *Trailed*, *Defocused* or *No stars*. Tick **Unusable** in Frame
  Analysis and reject them all at once.
- **Star measurements you can trust everywhere.** Frame Analysis, Blink and stacking now measure stars the
  same way, so their numbers agree.

### Plate solving

- **Much faster solves without coordinates.** Download the quad index packs in **Settings → Plate Solving**
  once, and frames with no position in their header solve in a second or two.

### Calibration

- **Fewer wrong matches.** Darks and flats of a different frame size are never linked to your lights.
- **Fix links faster:** select several groups on the Calibration Coverage tab and re-assign or unlink them
  together.
- **Find the right master quickly** with the new Size column and Gain, Bin and Size filters.
- **Masters can be saved as XISF** as well as FITS.

### Settings

- **Simpler and searchable.** Every change saves by itself — there is no Save button — and a search box
  finds any setting.

## Good to know after updating

- **Click Re-Analyze on your frame sets once** to measure them the new way; until then a set shows its old
  numbers. The first stack of each set also takes a little longer once. Nothing is calibrated twice.
- **Numbers shift slightly.** Star sizes on colour cameras read smaller and star counts higher than before,
  so take a quick look at your rejection limits.
- **Collaboration projects start in manual mode.** Nothing is published until you choose to.

## Bug Fixes

- **Frames you moved to the Black Hole are now left out** of stacks, exports and frame sets sent to another
  device. Before, some of them could still end up in the result — if you stacked or exported a set after
  rejecting frames, run it again.
- The stacking results show a realistic SNR gain; it used to read far too high.
- The Export tab counts imaging nights correctly, and its folder preview no longer reshuffles.
- Frame set cards show the right total exposure.
- Colour cameras: hot pixels no longer make some frames count less than they should in the stack.
- Some 16-bit FITS files and colour frames with an unusual Bayer layout now open with the right brightness
  and colours.
- A stack no longer fails to save its aligned frames when the reference frame has a very long file name.
