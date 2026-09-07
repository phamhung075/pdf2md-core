# Builds the pdf2md-core standalone server (Go + cgo -> Rust).
#
#   make build     # build Rust core then Go server -> bin/pdf2md-server
#   make static    # build static binary embedding libpdf2md_core.a
#   make run       # build and run on :8080 (override with PORT=...)
#   make clean

RUST_LIB_DIR := ../crates/pdf2md-core/target/release
ABS_LIB_DIR  := $(abspath $(RUST_LIB_DIR))
RUST_STATIC  := $(ABS_LIB_DIR)/libpdf2md_core.a

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  RUST_LIB := $(RUST_LIB_DIR)/libpdf2md_core.dylib
else
  RUST_LIB := $(RUST_LIB_DIR)/libpdf2md_core.so
endif

.PHONY: build static run rust clean

build: $(RUST_LIB)
	@mkdir -p bin
	CGO_ENABLED=1 \
	CGO_LDFLAGS="-L$(ABS_LIB_DIR) -lpdf2md_core -lm -ldl -Wl,-rpath,$(ABS_LIB_DIR)" \
	go build -o bin/pdf2md-server .

static: $(RUST_STATIC)
	@mkdir -p bin
	CGO_ENABLED=1 \
	CGO_LDFLAGS="$(RUST_STATIC) -lm -ldl -lpthread" \
	go build -ldflags="-s -w" -o bin/pdf2md-server .

$(RUST_LIB) $(RUST_STATIC):
	cd ../crates/pdf2md-core && cargo build --release --no-default-features

rust: $(RUST_LIB) $(RUST_STATIC)

run: build
	./bin/pdf2md-server

clean:
	rm -rf bin
