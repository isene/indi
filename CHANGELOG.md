# Changelog

## 0.2.1 (2026-11-18)

- Stars in front of a bright nebula measure as sharp as on a dark sky.
  The nebula's glow counted as starlight and made the HFR many times
  too large.
- A sharp star on a colour camera is counted; it was taken for a hot
  pixel and dropped.
- A key typed in a prompt (go-to, focuser position) comes once. On a
  terminal that reports key releases it came twice (crust 0.1.80).
- The sky picker no longer keeps a core busy while it is open
  (starmap 0.1.13, crust 0.1.80).
- Screenshots in the README.

## 0.2.0 (2026-11-18)

- A camera gets a control block, and its pictures show below it: real
  pixels where the terminal shows images, half blocks elsewhere.
- Each picture is stretched so the faint sky shows, and measured: size,
  lowest, middle and highest value, the stars found and their HFR (how
  sharp they are).
- The focuser's block shows the HFR of the last picture, the sharpest
  so far and the focuser position it was taken at.
- Space takes a picture or stops one, `l` takes them over and over, `z`
  shows the middle pixel for pixel, `s` saves the picture as FITS and
  `a` saves every one. `e`, `+` and `-` set the exposure, `t` cools the
  sensor, `g` sets the gain and `b` the binning.
- Raw colour sensors show in colour, and night colours show the picture
  in red.
- The server sends pictures only after the first exposure from indi, so
  a camera left alone costs nothing.

## 0.1.0 (2026-11-18)

- First version: the devices on an INDI server, a control block for a
  focuser and a mount, and every property of a device, one group at a
  time.
- The mount goes to a planet, the Moon, a Messier or Caldwell object or
  a named star by name, or to a star picked on a map of the sky. It
  asks before a target below the horizon or near the Sun, and never
  points at the Sun. `y` syncs on a name.
- A held arrow moves the mount; letting go stops it where the terminal
  reports key releases.
- The focuser saves positions in nine named slots and shows its
  temperature as a line of bars.
- `d` picks drivers and starts a server that ends with indi.
- Night colours, finding a setting, nudging a number by its step,
  saving a driver's settings, server messages with times and colours,
  and Ctrl+A for a Claude session about the device on screen.
