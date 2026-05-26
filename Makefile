PREFIX ?= $(HOME)/.local

build:
	cargo build --release

install: build
	install -d $(PREFIX)/bin
	install -m 755 target/release/enchanter $(PREFIX)/bin/enchanter

clean:
	cargo clean

.PHONY: build install clean