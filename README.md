# foongreet

A greetd greeter that draws straight to DRM/KMS (or `/dev/fb0`) with a
software CRT effect in amber or green phosphor. No compositor, no GPU
stack.

![Amber phosphor](docs/amber.png)
![Green phosphor](docs/green.png)

## Build and install

    make                 # build as your user
    sudo make install    # binary, default config, cache directory

`make install` puts the binary in `/usr/local/bin`, installs
`config.example.toml` as `/etc/foongreet/config.toml` unless one already
exists, and creates `/var/cache/foongreet` for the `greeter` user.
`PREFIX`, `DESTDIR` and `GREETER_USER` can be overridden as usual.
`sudo make uninstall` removes the binary but keeps the config and cache.

Other targets: `make test`, `make preview` (desktop window with a fake
greetd), `make screenshots` (regenerates the images above).

## Trying it out

1. **Desktop preview** (fake greetd, password is `password`):

       make preview

   `foongreet --snapshot out.png` renders offscreen instead; add `--demo`
   to swap your hostname, kernel, memory and clock for made-up values
   (`make screenshots` uses both).

2. **Spare VT with the real display path**: switch to a free VT
   (Ctrl+Alt+F3), log in, and run:

       ./target/release/foongreet --mock

   Ctrl+C quits in mock mode (or with `--dev`).

   To exercise the real IPC path, run it under greetd's `fakegreet`
   (`cargo install --git https://git.sr.ht/~kennylevinsen/greetd fakegreet`):

       fakegreet "./target/release/foongreet --dev" > fakegreet.log 2>&1

   Log in as `user` / `password`, then answer `9` to `7 + 2`.

   To try settings without touching `/etc`, pass `--config my.toml`.

3. **For real**: after `sudo make install`, set the greeter in
   `/etc/greetd/config.toml`:

       [default_session]
       command = "/usr/local/bin/foongreet"
       user = "greeter"

   Settings go in `/etc/foongreet/config.toml` (see below). Keep your previous `command` line commented out, and make sure you
   can still get a root shell on another VT.

## Configuration

Settings are read from `/etc/foongreet/config.toml`; every key is
optional. `config.example.toml` lists them all with their defaults:
phosphor color, background image, font, login screen, `/dev/fb0` mode,
default user, cache directory, power commands and extra sessions.

With several screens, the login goes on one (`output`, default the first
connected) and the others show an empty tube. `foongreet --list-outputs`
shows the screen names and which one gets the login; it works while your
compositor is running.

A broken config does not stop the greeter: it starts with the defaults,
shows `CONFIG ERROR, USING DEFAULTS`, and writes the details to stderr.
A missing image or font is handled the same way. To see the details,
run `foongreet --snapshot /tmp/x.png` against the config.

The command line only has testing flags; see `foongreet --help`.

## Screen locker

`foonlock` locks a Wayland session with the same look, through
ext-session-lock-v1 (Hyprland, sway, niri, river, KWin and others).
It checks the password with PAM and reads the `[display]` settings from
the same config file. Build and install it separately; it needs libpam
and libxkbcommon:

    make lock
    sudo make install-lock    # binary and /etc/pam.d/foonlock

Then start it from hypridle (`~/.config/hypr/hypridle.conf`):

    general {
        lock_cmd = pidof foonlock || foonlock
        before_sleep_cmd = loginctl lock-session
    }

`foonlock` refuses to start without `/etc/pam.d/foonlock`, as PAM's
fallback would deny every password and the session could not be
unlocked. If it ever dies while locked, the compositor keeps the session
locked; in Hyprland, set `misc:allow_session_lock_restore` so a new
locker can take over: start `foonlock` again from another VT with
`WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` pointing at the session.

To try it out without PAM, `foonlock --mock` takes the password
`password`, and `--unlock-after SECS` unlocks by itself.

## License

Copyright (C) 2026 Niklas Schönberg

foongreet is free software: you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation, either version 3 of the License, or (at your
option) any later version. See [LICENSE](LICENSE).
