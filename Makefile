# Trellis Protocol — Build Automation
#

.PHONY: help build test lint fmt deploy clean setup
.PHONY: build-contract build-cli build-frontend
.PHONY: test-contract test-cli test-snapshots-update test-frontend
.PHONY: lint-contract lint-frontend typecheck-frontend
.PHONY: changelog

help: ## list all targets
	@echo "Trellis Protocol — Build Targets"
	@echo ""
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | sort | while IFS= read -r line; do \
		target=$${line%%:*}; \
		desc=$${line##*## }; \
		printf "  %-20s %s\n" "$$target" "$$desc"; \
	done

# ── Build ──────────────────────────────────────────────────────────────────

build: build-contract build-cli build-frontend ## build everything (contract WASM + frontend)

build-contract: ## build only the contract WASM
	cargo build --frozen --manifest-path contracts/trellis_core/Cargo.toml --target wasm32-unknown-unknown --release

build-cli: ## build only the CLI binary
	cargo build --frozen --manifest-path cli/trellis_cli/Cargo.toml --release

build-frontend: ## build only the frontend bundle
	# npm ci (not npm install) installs exact versions from package-lock.json
	# for reproducible builds.
	cd frontend && npm ci && npm run build

# ── Test ───────────────────────────────────────────────────────────────────

test: test-contract test-cli test-frontend ## run all tests (contract + CLI + frontend)

test-contract: ## run only contract tests
	cargo test --frozen --manifest-path contracts/trellis_core/Cargo.toml

# CI does not shell out to make: the `verify` job in
# .github/workflows/contract-ci.yml runs `cargo test --workspace`, which
# already covers cli/trellis_cli. This target keeps `make test` in parity.
test-cli: ## run only CLI tests
	cargo test --frozen --manifest-path cli/trellis_cli/Cargo.toml

test-snapshots-update: ## regenerate Soroban test snapshots (commit the result)
	SOROBAN_TEST_SNAPSHOT_FILE=overwrite cargo test --manifest-path contracts/trellis_core/Cargo.toml
	@echo "Snapshots regenerated. Review the diff with: git diff contracts/trellis_core/test_snapshots/"

test-frontend: ## run only frontend tests
	cd frontend && npm ci && npm test

# ── Lint ───────────────────────────────────────────────────────────────────

lint: lint-contract lint-frontend ## run all linters (clippy + oxlint)

lint-contract: ## run only clippy on contract and CLI
	cargo clippy --frozen --manifest-path contracts/trellis_core/Cargo.toml -- -D warnings
	cargo clippy --frozen --manifest-path cli/trellis_cli/Cargo.toml -- -D warnings

lint-frontend: ## run only oxlint on frontend
	cd frontend && npm ci && npm run lint

fmt: ## run cargo fmt on the workspace
	cargo fmt --all

typecheck-frontend: ## run tsc typecheck on the frontend
	cd frontend && npm run typecheck

# ── Changelog ──────────────────────────────────────────────────────────────

# Requires git-cliff: https://github.com/orhun/git-cliff
changelog: ## regenerate CHANGELOG.md from git history (git-cliff)
	# Regenerate the committed CHANGELOG.md from conventional commits.
	@command -v git-cliff >/dev/null 2>&1 || { \
		echo "git-cliff not found. Install it with: cargo install git-cliff"; \
		exit 1; \
	}
	git-cliff --config cliff.toml --output CHANGELOG.md
	@echo "CHANGELOG.md regenerated from git history."

# ── Deploy ─────────────────────────────────────────────────────────────────

DEPLOYER_IDENTITY ?= trellis-deployer

deploy: ## deploy contract to testnet (see DEPLOYMENT.md)
	@echo "=== Deploying Trellis to Stellar Testnet ==="
	@echo "Ensure you have:"
	@echo "  1. A funded testnet identity: stellar keys fund <name> --network testnet"
	@echo "  2. The contract WASM built: make build-contract"
	@echo ""
	@echo "Running: stellar contract deploy ..."
	stellar contract deploy \
		--wasm target/wasm32-unknown-unknown/release/trellis_core.wasm \
		--source $(DEPLOYER_IDENTITY) \
		--network testnet

# ── Clean ──────────────────────────────────────────────────────────────────

clean: ## remove all build artifacts
	cargo clean
	cd frontend && rm -rf dist node_modules
	rm -rf target

# ── Setup ──────────────────────────────────────────────────────────────────

setup: ## install prerequisites (rustup target + npm deps)
	rustup target add wasm32-unknown-unknown
	cd frontend && npm ci