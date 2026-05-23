PREFIX ?= $(HOME)/.local

build:
	cargo build --release

release: build

install: build
	install -d $(PREFIX)/bin
	install -m 755 target/release/enchanter $(PREFIX)/bin/enchanter

clean:
	cargo clean

.PHONY: build release install clean