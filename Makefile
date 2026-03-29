TARGET ?= aarch64-unknown-linux-gnu
PACKAGE ?= ks
BIN_NAME ?= ks
PROFILE ?= release
IMAGE ?= kk-ks:aarch64
PLATFORM ?= linux/arm64
CONTAINER_TOOL ?= docker
CONTAINERFILE ?= Containerfile.ks.aarch64

# If your host cannot cross-compile with cargo, use: make ... CARGO=cross
CARGO ?= cargo
RUSTUP ?= rustup

BIN_PATH ?= target/$(TARGET)/$(PROFILE)/$(BIN_NAME)
STAGE_DIR ?= container-bin
STAGED_BIN ?= $(STAGE_DIR)/$(BIN_NAME)

.PHONY: help rust-target build-aarch64 stage-binary image-aarch64 clean-stage

help:
	@echo "Targets:"
	@echo "  make build-aarch64                 Build $(PACKAGE) for $(TARGET)"
	@echo "  make image-aarch64 IMAGE=<name>    Build arm64 image from prebuilt binary"
	@echo "  make clean-stage                   Remove staged binary"
	@echo ""
	@echo "Variables:"
	@echo "  IMAGE=$(IMAGE)"
	@echo "  TARGET=$(TARGET)"
	@echo "  CONTAINER_TOOL=$(CONTAINER_TOOL)"
	@echo "  CARGO=$(CARGO)"

rust-target:
	$(RUSTUP) target add $(TARGET)

build-aarch64: rust-target
	$(CARGO) build -p $(PACKAGE) --$(PROFILE) --target $(TARGET)

stage-binary: build-aarch64
	mkdir -p $(STAGE_DIR)
	cp $(BIN_PATH) $(STAGED_BIN)
	chmod +x $(STAGED_BIN)

image-aarch64: stage-binary
	$(CONTAINER_TOOL) buildx build \
		--platform $(PLATFORM) \
		-f $(CONTAINERFILE) \
		--build-arg BIN_PATH=$(STAGED_BIN) \
		--build-arg BIN_NAME=$(BIN_NAME) \
		-t $(IMAGE) \
		--load \
		.

clean-stage:
	rm -f $(STAGED_BIN)
