# Changelog

### v0.1.2 (2026-11-03)

#### Misc

- Give the greeter its own make target, install both by default (97981f5)

### v0.1.1 (2026-11-03)

#### Misc

- Let the greeter skip closed laptop lids, move and rotate the login (adb6bfe)
- Let foonlock's lock box follow the mouse, fit portrait screens (39cbc4c)
- Let the greeter switch VTs with Alt+Fn (d374a81)
- Document foonlock's suspend handling and hypridle setup (ac0b122)
- Blank foonlock before a suspend and power on again after it (c636702)
- Add the dormant mode to the greeter (d3315f6)
- Make the CRT renderer about twice as fast (8f53f86)
- Let foonlock go dormant when idle instead of dropping to 10 fps (33a203a)
- Check the password with PAM in foonlock (86a03c4)
- Cap render threads at four, drop foonlock to 10 fps when idle (31842ff)
- Add foonlock, a Wayland screen locker (test build) (5599ecf)
- Split into a shared library and the greeter binary (31e06d4)
- Wait for the display before animating, add a power-on animation (57af8ef)
- Smooth horizontal sampling so glyph strokes keep an even width (49c1790)
- License under GPL-3.0-or-later (e9e0b55)
- Add Makefile for building and installing (f5aba16)
- Rename to Foonly Microsystems, add --demo and screenshots (fafe2b2)
- Initial commit: CRT-style greetd greeter (6f02c29)

