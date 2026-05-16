# Pietro — container build helpers (podman, multi-arch).
#
# The Dockerfile is fully self-contained (multi-stage: node → rust-musl →
# scratch). This Makefile drives podman to produce a multi-arch manifest
# list covering linux/amd64 and linux/arm64.
#
# Cross-arch builds use qemu-user emulation under the hood — check that
# `qemu-aarch64` is registered in /proc/sys/fs/binfmt_misc/ before the
# first arm64 build (on Fedora/Silverblue it ships preregistered; on
# Debian/Ubuntu: `apt install qemu-user-static binfmt-support`).

CONTAINER  ?= podman

IMAGE      ?= pietro
TAG        ?= latest
IMAGE_REF  := $(IMAGE):$(TAG)

# Default to the two architectures the user actually runs on. Override
# with `make PLATFORMS=linux/amd64 docker` for a fast single-arch build,
# or `make PLATFORMS=linux/arm64 ...` to test the foreign-arch path alone.
PLATFORMS  ?= linux/amd64,linux/arm64

# Extra flags forwarded to `podman build` (e.g. --no-cache, --pull=always,
# --build-arg, --jobs=N for parallel multi-arch builds).
BUILD_ARGS ?=

# Registry to push to when running `make push`. Defaults to the Pietro
# source-of-truth at git.patchnotes.com/patchnotes (Gitea container
# registry under the same namespace as the source repo,
# https://git.patchnotes.com/patchnotes/pietro). Override at the CLI for
# scratch pushes elsewhere.
REGISTRY   ?= git.patchnotes.com/patchnotes

.PHONY: docker docker-build docker-build-amd64 docker-build-arm64 \
        docker-run docker-shell push inspect clean help

help:
	@echo "Pietro container targets (engine: $(CONTAINER)):"
	@echo "  make docker              # multi-arch build → $(IMAGE_REF) manifest"
	@echo "  make docker-build-amd64  # build just linux/amd64 (fast, native)"
	@echo "  make docker-build-arm64  # build just linux/arm64 (qemu emulated)"
	@echo "  make docker-run          # run the native-arch image on :18080"
	@echo "  make docker-shell        # debug shell in the *backend* stage"
	@echo "  make push                # push manifest list to \$$(REGISTRY) (default: $(REGISTRY))"
	@echo "  make inspect             # show the manifest list contents"
	@echo "  make clean               # remove image + manifest locally"
	@echo ""
	@echo "Overrides:"
	@echo "  IMAGE=$(IMAGE) TAG=$(TAG)"
	@echo "  PLATFORMS='$(PLATFORMS)'"
	@echo "  BUILD_ARGS='$(BUILD_ARGS)' REGISTRY='$(REGISTRY)'"

# Default: full multi-arch build wrapped in a manifest list.
docker: docker-build

# `--manifest` tells podman to attach each per-arch image to a manifest
# list of the given name, so a single `podman manifest push` ships both
# arches. `--jobs=2` builds the two platforms in parallel (well, as
# parallel as qemu lets us go — the arm64 leg is still the long pole).
docker-build:
	$(CONTAINER) build \
	  --platform=$(PLATFORMS) \
	  --manifest $(IMAGE_REF) \
	  --jobs=2 \
	  $(BUILD_ARGS) \
	  .

# Single-arch shortcuts for tighter dev loops.
docker-build-amd64:
	$(CONTAINER) build \
	  --platform=linux/amd64 \
	  -t $(IMAGE_REF) \
	  $(BUILD_ARGS) \
	  .

docker-build-arm64:
	$(CONTAINER) build \
	  --platform=linux/arm64 \
	  -t $(IMAGE_REF) \
	  $(BUILD_ARGS) \
	  .

# Quick smoke-run. podman picks the manifest entry matching the host arch
# automatically when you reference the manifest list by name. Expects
# pietro.yaml in the cwd and PIETRO_* env vars set in the shell.
docker-run:
	$(CONTAINER) run --rm -it \
	  -p 18080:18080 \
	  -v "$(PWD)/pietro.yaml:/etc/pietro/pietro.yaml:ro" \
	  -e PIETRO_COOKIE_KEY \
	  -e PIETRO_API_KEY_PEPPER \
	  -e PIETRO_OIDC_CLIENT_ID \
	  -e PIETRO_OIDC_CLIENT_SECRET \
	  $(IMAGE_REF)

# Drop into the backend builder stage on the native arch — useful when a
# `cargo build` fails inside the container and you want to poke around.
docker-shell:
	$(CONTAINER) build \
	  --target backend \
	  -t $(IMAGE)-builder:$(TAG) \
	  $(BUILD_ARGS) \
	  .
	$(CONTAINER) run --rm -it $(IMAGE)-builder:$(TAG) sh

# Push the manifest list (and every arch image it references) to REGISTRY.
# Default REGISTRY is git.patchnotes.com/patchnotes, so:
#   make push
# ships to git.patchnotes.com/patchnotes/pietro:latest (matching the
# source repo at https://git.patchnotes.com/patchnotes/pietro). Override
# REGISTRY=host/namespace to push elsewhere.
push:
	@test -n "$(REGISTRY)" || (echo "REGISTRY is empty; set REGISTRY=host/namespace"; exit 1)
	$(CONTAINER) manifest push --all $(IMAGE_REF) docker://$(REGISTRY)/$(IMAGE_REF)

# Pretty-print the manifest list so you can verify both arches are in.
inspect:
	$(CONTAINER) manifest inspect $(IMAGE_REF)

clean:
	-$(CONTAINER) manifest rm $(IMAGE_REF)
	-$(CONTAINER) image rm $(IMAGE_REF)
	-$(CONTAINER) image rm $(IMAGE)-builder:$(TAG)
