# Changelog

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
