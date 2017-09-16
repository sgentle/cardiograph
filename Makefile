.PHONY: build fmt clippy

build:
	cargo build --release

fmt:
	cargo +nightly fmt

clippy:
	cargo +nightly rustc --features clippy -- -Z no-trans -Z extra-plugins=clippy
