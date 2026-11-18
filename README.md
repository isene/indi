# indi

Telescope gear in the terminal. indi talks to an
[INDI](https://indilib.org) server, the program that drives focusers,
mounts, cameras and filter wheels, and shows every device it has.

- The devices are on the left. A green dot means the device is connected.
- On the right is the device you picked. A focuser or a mount gets a
  control block at the top: the focuser's position, temperature and
  saved positions, or where the mount points, how high, whether it
  tracks, and its speed.
- Below that come all the device's settings, one group at a time, as
  its driver defines them. The dot before each one is grey when idle,
  green when fine, yellow when busy and red on trouble.
- The mount goes to a planet, the Moon, any Messier or Caldwell object
  or a named star by name, or to a star you pick on a map of tonight's
  sky. It asks first when the target is below the horizon or near the
  Sun, and it never points at the Sun.
- Night colours turn everything red on black, to keep your eyes used
  to the dark.
- Ctrl+A opens a Claude session about the device on screen, its
  settings and the server's messages.

indi is built on [crust](https://github.com/isene/crust), the terminal
library of the [Fe₂O₃](https://github.com/isene/fe2o3) suite. It
replaces the Ruby `tuindi` in
[ruby-indi](https://github.com/isene/ruby-indi).

## Start

Run `indi`. With no server running, press `d`, tick the drivers for your
gear and press Enter: indi starts the server and connects. It remembers
the drivers and starts them again next time. A server indi started
ends when indi does.

Or start the server yourself. For a ZWO EAF focuser and an EQMod mount
(an EQ6-R Pro, say):

```bash
indiserver indi_ieaf_focuser indi_eqmod_telescope
indi
```

To try it without gear, use the simulators:

```bash
indiserver indi_simulator_telescope indi_simulator_focus
```

`indi host` or `indi host:port` reaches a server on another machine,
for example a Raspberry Pi at the telescope. The port is 7624 when left
out. On Debian and Ubuntu the server and the drivers come in the
package `indi-bin`.

## Keys

| Key | What it does |
|---|---|
| Tab | Moves between the devices, the control block and the settings |
| ↑ ↓ | Picks a device or a setting |
| ← → | Shows the next or the last group of settings |
| Enter | Connects a device, flips a switch, or asks for a new value |
| + - | Nudges a number setting by its step |
| / | Finds a setting by name |
| c | Connects or disconnects the device |
| W | Has the driver save the device's settings |
| d | Picks drivers and starts a server |
| m | Shows the messages from the server, errors in red |
| n | Switches the night colours on or off |
| r | Connects to the server again |
| Ctrl+A | Asks Claude about the device on screen |
| ? | Shows the help |
| q | Quits; a mount that is moving is stopped first |

In the focuser's control block:

| Key | What it does |
|---|---|
| ← → | Moves 10 steps in or out |
| Shift+← → | Moves 100 steps |
| [ ] | Moves 1 step |
| g | Goes to a position you type |
| s | Saves the position in a slot from 1 to 9, with a name |
| 1 to 9 | Goes back to a saved position |
| Space | Stops the focuser |

In the mount's control block:

| Key | What it does |
|---|---|
| ↑ ↓ ← → | Moves north, south, east or west while you hold the key |
| Space | Stops all motion |
| g | Goes to a name (`M42`, `Vega`, `Jupiter`) or an RA and Dec (`5:35:17 -5:23:28`) |
| s | Picks a star on a map of the sky and goes there |
| y | Syncs: tells the mount it points at the name you type |
| + - | Changes the speed |
| t | Turns tracking on or off |
| p | Parks or unparks the mount, after a yes |

The mount stops when you let go of an arrow in terminals that report
key releases, such as kitty. In other terminals it moves until you
press Space.

## Settings

indi keeps its settings in `~/.indirc` and rewrites the file when you
change one in the app:

```
night = no
lat = 51.48
lon = 0.0
drivers = indi_ieaf_focuser indi_eqmod_telescope
focus1 = 31250 Visual
focus2 = 30120 Camera
```

`lat` and `lon` (east is positive) are used only when the mount does
not know where it stands. With a site, indi shows how high the mount
points and warns before a go-to below the horizon.

## Install

Download a binary from the
[releases](https://github.com/isene/indi/releases) (Linux on Intel or
ARM, macOS), or build it beside the libraries it uses:

```bash
for r in crust glow orbit starmap indi; do git clone https://github.com/isene/$r; done
cd indi && cargo build --release
```

## Cost while it waits

A thread reads the server and wakes the program only when something
changes. With no key and no news from the gear, indi does no work.

## License

[Unlicense](https://unlicense.org): public domain.
