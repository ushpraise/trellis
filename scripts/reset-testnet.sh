#!/usr/bin/env bash
# Redeploys the Trellis contract to Stellar testnet and creates a fresh test
# agreement. Use this after a testnet reset invalidates the contract ID or
# agreement ID referenced in DEPLOYMENT.md.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

RPC_URL="https://soroban-testnet.stellar.org"
NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
DEPLOYER_KEY="trellis-deployer"

echo "==> Ensuring wasm32-unknown-unknown target is installed"
rustup target add wasm32-unknown-unknown

echo "==> Ensuring deployer identity exists and is funded"
if ! stellar keys address "$DEPLOYER_KEY" >/dev/null 2>&1; then
  stellar keys generate "$DEPLOYER_KEY" --network testnet
fi
stellar keys fund "$DEPLOYER_KEY" --network testnet --rpc-url "$RPC_URL"

echo "==> Building contract WASM"
cargo rustc \
  --manifest-path=contracts/trellis_core/Cargo.toml \
  --crate-type=cdylib \
  --target=wasm32-unknown-unknown \
  --release

WASM_PATH="target/wasm32-unknown-unknown/release/trellis_core.wasm"

echo "==> Deploying contract to testnet"
CONTRACT_ID=$(stellar contract deploy \
  --wasm "$WASM_PATH" \
  --source "$DEPLOYER_KEY" \
  --network testnet)

echo "==> Building CLI"
cargo build --release --manifest-path=cli/trellis_cli/Cargo.toml

CLI_BIN="target/release/trellis"

echo "==> Creating payer/payee/resolver identities for the test agreement"
for name in trellis-payer trellis-payee trellis-resolver; do
  if ! stellar keys address "$name" >/dev/null 2>&1; then
    stellar keys generate "$name" --network testnet
  fi
  stellar keys fund "$name" --network testnet --rpc-url "$RPC_URL"
done

PAYER=$(stellar keys address trellis-payer)
PAYEE=$(stellar keys address trellis-payee)
RESOLVER=$(stellar keys address trellis-resolver)
USDC_ISSUER="GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5"
# `init` needs the USDC Stellar Asset Contract (C...) address, not the
# issuer account (G...) — derive it from the asset.
USDC_SAC=$(stellar contract id asset --network testnet --asset "USDC:$USDC_ISSUER")
AGREEMENT_ID=$(printf '01%.0s' {1..64} | head -c 64)

export STELLAR_RPC_URL="$RPC_URL"
export STELLAR_NETWORK_PASSPHRASE="$NETWORK_PASSPHRASE"
export TRELLIS_CONTRACT_ID="$CONTRACT_ID"
export TRELLIS_SOURCE_KEY="$DEPLOYER_KEY"

echo "==> Creating fresh test agreement"
"$CLI_BIN" init \
  --agreement-id "$AGREEMENT_ID" \
  --payer "$PAYER" \
  --payee "$PAYEE" \
  --token "$USDC_SAC" \
  --resolver "$RESOLVER" \
  --milestones "1000"

cat <<EOF

==> Done. Update DEPLOYMENT.md and frontend/.env with the values below:

Contract ID:   $CONTRACT_ID
Agreement ID:  $AGREEMENT_ID
Payer:         $PAYER
Payee:         $PAYEE
Resolver:      $RESOLVER

Verify with:
  TRELLIS_CONTRACT_ID=$CONTRACT_ID $CLI_BIN status --agreement-id $AGREEMENT_ID
EOF
