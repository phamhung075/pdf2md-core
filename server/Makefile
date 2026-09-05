# Builds the pdf2md-core mini dev server (Go + cgo -> Rust cdylib).
#
#   make build     # build Rust core (release, no Python) then Go server -> bin/pdf2md-server
#   make run       # build and run on :8989 (override with PORT=...)
#   make clean

RUST_LIB_DIR := ../crates/pdf2md-core/target/release
ABS_LIB_DIR  := $(abspath $(RUST_LIB_DIR))

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  RUST_LIB := $(RUST_LIB_DIR)/libpdf2md_core.dylib
else
  RUST_LIB := $(RUST_LIB_DIR)/libpdf2md_core.so
endif

.PHONY: build run rust clean

build: $(RUST_LIB)
	@mkdir -p bin
	CGO_ENABLED=1 \
	CGO_LDFLAGS="-L$(ABS_LIB_DIR) -lpdf2md_core -lm -ldl -Wl,-rpath,$(ABS_LIB_DIR)" \
	go build -o bin/pdf2md-server .

$(RUST_LIB):
	cd ../crates/pdf2md-core && cargo build --release --no-default-features

rust: $(RUST_LIB)

run: build
	./bin/pdf2md-server

clean:
	rm -rf bin
