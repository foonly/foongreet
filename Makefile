# Build as your user, install as root:
#
#   make
#   sudo make install
#
# The screen locker builds and installs separately (it needs libpam and
# libxkbcommon):
#
#   make lock
#   sudo make install-lock
#
# install never builds, so target/ is not left owned by root.

PREFIX       ?= /usr/local
BINDIR       ?= $(PREFIX)/bin
SYSCONFDIR   ?= /etc
CACHEDIR     ?= /var/cache/foongreet
GREETER_USER ?= greeter
DESTDIR      ?=

BIN    := target/release/foongreet
CONFIG := $(DESTDIR)$(SYSCONFDIR)/foongreet/config.toml
LOCK_BIN := target/release/foonlock
PAM_FILE := $(DESTDIR)$(SYSCONFDIR)/pam.d/foonlock

.PHONY: all build lock test preview screenshots install install-lock uninstall clean

all: build

build:
	cargo build --release

lock:
	cargo build --release --features lock --bin foonlock

test:
	cargo test --release --features lock

# Desktop window with a fake greetd (password: "password").
preview:
	cargo run --release --features preview -- --preview 1600x900

# Regenerates the README screenshots with made-up machine details.
screenshots: build
	$(BIN) --snapshot docs/amber.png --preview 1600x900 --config /dev/null --demo
	printf '[display]\nphosphor = "green"\n' > target/green.toml
	$(BIN) --snapshot docs/green.png --preview 1600x900 --config target/green.toml --demo

install:
	@test -x $(BIN) || { echo "$(BIN) not found; run 'make' as your user first"; exit 1; }
	install -Dm755 $(BIN) $(DESTDIR)$(BINDIR)/foongreet
	@# Never overwrite an existing config.
	@if [ -e $(CONFIG) ]; then \
		echo "keeping existing $(CONFIG)"; \
	else \
		install -Dm644 config.example.toml $(CONFIG) && echo "installed $(CONFIG)"; \
	fi
	@# Remembers the last user/session; must be writable by the greeter.
	@# Staged installs (packages) can't chown; they should use tmpfiles.d.
	@if [ -n "$(DESTDIR)" ]; then \
		echo "DESTDIR set: not creating $(CACHEDIR)"; \
	elif id $(GREETER_USER) >/dev/null 2>&1; then \
		install -d -o $(GREETER_USER) -g $(GREETER_USER) -m 700 $(DESTDIR)$(CACHEDIR); \
	else \
		echo "warning: user '$(GREETER_USER)' not found; create $(CACHEDIR) for your greeter user"; \
	fi
	@echo
	@echo "In /etc/greetd/config.toml, under [default_session]:"
	@echo "  command = \"$(BINDIR)/foongreet\""

install-lock:
	@test -x $(LOCK_BIN) || { echo "$(LOCK_BIN) not found; run 'make lock' as your user first"; exit 1; }
	install -Dm755 $(LOCK_BIN) $(DESTDIR)$(BINDIR)/foonlock
	@# Without it PAM falls back to "other" and nothing could unlock.
	install -Dm644 pam/foonlock $(PAM_FILE)
	@echo
	@echo "In ~/.config/hypr/hypridle.conf, under general:"
	@echo "  lock_cmd = pidof foonlock || $(BINDIR)/foonlock"

# Leaves the config and cache in place.
uninstall:
	rm -f $(DESTDIR)$(BINDIR)/foongreet $(DESTDIR)$(BINDIR)/foonlock $(PAM_FILE)
	@echo "kept $(CONFIG) and $(DESTDIR)$(CACHEDIR); remove them by hand if wanted"

clean:
	cargo clean
