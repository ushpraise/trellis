use crate::config::Config;
use crate::commands::ContractResult;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::io::Write;
use std::num::NonZeroU32;
use std::sync::OnceLock;

/// Default hard timeout (in seconds) for a single `stellar` subprocess call.
///
/// The process is killed and a transient error is returned when this elapses,
/// allowing the existing retry/backoff logic to attempt again. Set
/// `STELLAR_INVOKE_TIMEOUT_SECS=0` to disable the timeout entirely (not
/// recommended in production).
const DEFAULT_INVOKE_TIMEOUT_SECS: u64 = 30;

static RPC_RATE_LIMITER: OnceLock<DefaultDirectRateLimiter> = OnceLock::new();

fn get_rate_limiter() -> &'static DefaultDirectRateLimiter {
    RPC_RATE_LIMITER.get_or_init(|| {
        let limit_per_sec: u32 = std::env::var("STELLAR_RPC_RATE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        if let Some(limit) = NonZeroU32::new(limit_per_sec) {
            RateLimiter::direct(Quota::per_second(limit))
        } else {
            RateLimiter::direct(Quota::per_second(NonZeroU32::new(10).unwrap()))
        }
    })
}

fn apply_rate_limit() {
    let limiter = get_rate_limiter();
    if limiter.check().is_err() {
        eprintln!("⚠️  RPC rate limit active — request queued until quota resets");
        // `until_ready` is async; the CLI is synchronous, so poll instead.
        while limiter.check().is_err() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Output from a Soroban contract invoke.
#[derive(Debug)]
pub struct InvokeOutput {
    /// Combined stdout from the process.
    pub stdout: String,
    /// Combined stderr from the process.
    pub stderr: String,
    /// Whether the process exited successfully.
    pub success: bool,
    /// The exact command string that was executed — printed on failure so
    /// the caller can reproduce/debug locally.
    pub command_debug: String,
}

// ---------------------------------------------------------------------------
// Native transaction simulation (#476)
// ---------------------------------------------------------------------------

/// A typed failure from [`RpcClient::simulate_transaction`].
///
/// Simulation can fail in three genuinely different ways, and callers need to
/// tell them apart:
///
/// * [`SimulationError::Network`] — the request never produced a simulation
///   (DNS/connect/timeout/HTTP/JSON failure). These are the only errors safe
///   to retry.
/// * [`SimulationError::Rpc`] — the endpoint answered with a JSON-RPC error
///   object, e.g. the envelope was malformed. Retrying cannot help.
/// * [`SimulationError::Contract`] — the endpoint *did* simulate the call but
///   the host function reverted (a contract-level error), reported by the RPC
///   as `result.error` together with any diagnostic `events`. Never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationError {
    /// Transport-/protocol-level failure talking to the RPC endpoint.
    Network(String),
    /// A JSON-RPC error object returned by the endpoint.
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// JSON-RPC error message.
        message: String,
    },
    /// The simulated invocation reverted — a contract-level, not network,
    /// error. `events` carries any diagnostic events the RPC returned.
    Contract {
        /// Human-readable description of the reverted call.
        message: String,
        /// Base64-encoded diagnostic events, if the endpoint returned any.
        events: Vec<String>,
    },
}

impl std::fmt::Display for SimulationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SimulationError::Network(msg) => {
                write!(f, "simulateTransaction network error: {msg}")
            }
            SimulationError::Rpc { code, message } => {
                write!(f, "simulateTransaction RPC error {code}: {message}")
            }
            SimulationError::Contract { message, .. } => {
                write!(f, "simulateTransaction contract error: {message}")
            }
        }
    }
}

impl std::error::Error for SimulationError {}

/// A parsed `LedgerKey` from a simulated transaction's resource footprint.
///
/// Only the identity fields are kept — enough to show *what* the invocation
/// touches. Addresses and hashes are hex-encoded XDR payloads rather than
/// StrKey; callers that need StrKey re-encode them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerKey {
    /// A classic account entry (`LedgerEntryType::ACCOUNT`).
    Account { account_id: String },
    /// A trustline (`LedgerEntryType::TRUSTLINE`).
    Trustline { account_id: String, asset: String },
    /// An offer (`LedgerEntryType::OFFER`).
    Offer { seller_id: String, offer_id: i64 },
    /// A data entry (`LedgerEntryType::DATA`).
    Data { account_id: String, name: String },
    /// A claimable balance (`LedgerEntryType::CLAIMABLE_BALANCE`).
    ClaimableBalance { balance_id: String },
    /// A liquidity pool (`LedgerEntryType::LIQUIDITY_POOL`).
    LiquidityPool { pool_id: String },
    /// A contract data entry (`LedgerEntryType::CONTRACT_DATA`).
    ContractData { contract: String, durability: String },
    /// A contract WASM entry (`LedgerEntryType::CONTRACT_CODE`).
    ContractCode { hash: String },
    /// A network config setting (`LedgerEntryType::CONFIG_SETTING`).
    ConfigSetting { id: u32 },
    /// A time-to-live entry (`LedgerEntryType::TTL`).
    Ttl { key_hash: String },
}

impl std::fmt::Display for LedgerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerKey::Account { account_id } => write!(f, "account({account_id})"),
            LedgerKey::Trustline { account_id, asset } => {
                write!(f, "trustline({account_id}, {asset})")
            }
            LedgerKey::Offer { seller_id, offer_id } => {
                write!(f, "offer({seller_id}, {offer_id})")
            }
            LedgerKey::Data { account_id, name } => {
                write!(f, "data({account_id}, {name})")
            }
            LedgerKey::ClaimableBalance { balance_id } => {
                write!(f, "claimable_balance({balance_id})")
            }
            LedgerKey::LiquidityPool { pool_id } => {
                write!(f, "liquidity_pool({pool_id})")
            }
            LedgerKey::ContractData {
                contract,
                durability,
            } => write!(f, "contract_data({contract}, {durability})"),
            LedgerKey::ContractCode { hash } => write!(f, "contract_code({hash})"),
            LedgerKey::ConfigSetting { id } => write!(f, "config_setting({id})"),
            LedgerKey::Ttl { key_hash } => write!(f, "ttl({key_hash})"),
        }
    }
}

/// The ledger entries a simulated invocation reads and writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceFootprint {
    /// Entries read but not modified.
    pub read_only: Vec<LedgerKey>,
    /// Entries read and written.
    pub read_write: Vec<LedgerKey>,
}

impl std::fmt::Display for ResourceFootprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "read-only ({}):", self.read_only.len())?;
        for key in &self.read_only {
            writeln!(f, "  - {key}")?;
        }
        write!(f, "read-write ({}):", self.read_write.len())?;
        for key in &self.read_write {
            write!(f, "\n  - {key}")?;
        }
        Ok(())
    }
}

/// A parsed `simulateTransaction` response.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Recommended minimum resource fee (in stroops) from `minResourceFee`.
    pub min_resource_fee: i64,
    /// Resource fee embedded in the recommended `transactionData`.
    pub resource_fee: i64,
    /// The ledger footprint the invocation would touch.
    pub footprint: ResourceFootprint,
    /// CPU instructions consumed.
    pub instructions: u32,
    /// Ledger bytes read.
    pub read_bytes: u32,
    /// Ledger bytes written.
    pub write_bytes: u32,
    /// Decoded `results[0].xdr`, when the endpoint returned one. Mutating
    /// invocations typically return a scalar or `void`; read-only invocations
    /// return their query result here.
    pub result: Option<serde_json::Value>,
    /// Raw base64 `results[0].xdr`, preserved for callers that want the
    /// undecoded `ScVal`.
    pub raw_result_xdr: Option<String>,
    /// If `results[0].xdr` was present but could not be decoded, the reason.
    pub result_decode_error: Option<String>,
    /// Base64-encoded diagnostic events emitted during simulation.
    pub events: Vec<String>,
    /// Latest ledger known to the RPC at simulation time.
    pub latest_ledger: Option<u64>,
}

/// A minimal big-endian XDR cursor over an in-memory byte slice.
///
/// This is intentionally separate from the `std::io::Cursor` used by the
/// `ScVal` decoder above: parsing a footprint requires *skipping* arbitrary
/// XDR values (including nested `ScVal`s) as well as reading fixed scalars,
/// and a byte-offset cursor makes that straightforward.
struct XdrReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> XdrReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        XdrReader { bytes, pos: 0 }
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let end = self.pos + 4;
        if end > self.bytes.len() {
            return Err(format!(
                "unexpected end of XDR: wanted 4 bytes at offset {}, have {}",
                self.pos,
                self.bytes.len() - self.pos
            ));
        }
        let v = u32::from_be_bytes(self.bytes[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }

    fn read_i64(&mut self) -> Result<i64, String> {
        let end = self.pos + 8;
        if end > self.bytes.len() {
            return Err(format!(
                "unexpected end of XDR: wanted 8 bytes at offset {}, have {}",
                self.pos,
                self.bytes.len() - self.pos
            ));
        }
        let v = i64::from_be_bytes(self.bytes[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }

    fn read_fixed(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos + n;
        if end > self.bytes.len() {
            return Err(format!(
                "unexpected end of XDR: wanted {n} bytes at offset {}, have {}",
                self.pos,
                self.bytes.len() - self.pos
            ));
        }
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Read a length-prefixed opaque value, skipping its alignment padding.
    fn read_opaque(&mut self) -> Result<Vec<u8>, String> {
        let len = self.read_u32()? as usize;
        let raw = self.read_fixed(len)?.to_vec();
        let pad = (4 - (len % 4)) % 4;
        self.read_fixed(pad)?;
        Ok(raw)
    }
}

/// Parse a base64 `SorobanTransactionData` XDR blob into the pieces
/// `simulateTransaction` callers need: the ledger footprint, the embedded
/// resource fee, and the instruction/IO byte budgets.
///
/// The wire layout (see `Stellar-transaction.x` / CAP-0046-07) is:
///
/// ```text
/// struct SorobanTransactionData {
///     ExtensionPoint ext;          // uint32 discriminant, 0 today
///     SorobanResources resources;  // { footprint, instructions, readBytes, writeBytes }
///     int64 resourceFee;
/// }
/// struct LedgerFootprint { LedgerKey readOnly<>; LedgerKey readWrite<>; }
/// ```
fn parse_transaction_data(
    xdr_base64: &str,
) -> Result<(ResourceFootprint, i64, u32, u32, u32), String> {
    let raw = base64_decode(xdr_base64)
        .map_err(|e| format!("invalid base64 in transactionData: {e}"))?;
    let mut r = XdrReader::new(&raw);

    let ext = r.read_u32()?;
    if ext != 0 {
        return Err(format!("unsupported SorobanTransactionData extension {ext}"));
    }

    let read_only_len = r.read_u32()? as usize;
    let mut read_only = Vec::with_capacity(read_only_len);
    for _ in 0..read_only_len {
        read_only.push(read_ledger_key(&mut r)?);
    }

    let read_write_len = r.read_u32()? as usize;
    let mut read_write = Vec::with_capacity(read_write_len);
    for _ in 0..read_write_len {
        read_write.push(read_ledger_key(&mut r)?);
    }

    let instructions = r.read_u32()?;
    let read_bytes = r.read_u32()?;
    let write_bytes = r.read_u32()?;
    let resource_fee = r.read_i64()?;

    Ok((
        ResourceFootprint {
            read_only,
            read_write,
        },
        resource_fee,
        instructions,
        read_bytes,
        write_bytes,
    ))
}

/// `LedgerEntryType` discriminants (see `Stellar-ledger-entries.x`).
const LEDGER_ACCOUNT: u32 = 0;
const LEDGER_TRUSTLINE: u32 = 1;
const LEDGER_OFFER: u32 = 2;
const LEDGER_DATA: u32 = 3;
const LEDGER_CLAIMABLE_BALANCE: u32 = 4;
const LEDGER_LIQUIDITY_POOL: u32 = 5;
const LEDGER_CONTRACT_DATA: u32 = 6;
const LEDGER_CONTRACT_CODE: u32 = 7;
const LEDGER_CONFIG_SETTING: u32 = 8;
const LEDGER_TTL: u32 = 9;

/// Read one `LedgerKey` from the footprint, consuming exactly its bytes.
fn read_ledger_key(r: &mut XdrReader<'_>) -> Result<LedgerKey, String> {
    let kind = r.read_u32()?;
    match kind {
        LEDGER_ACCOUNT => Ok(LedgerKey::Account {
            account_id: read_account_id(r)?,
        }),
        LEDGER_TRUSTLINE => Ok(LedgerKey::Trustline {
            account_id: read_account_id(r)?,
            asset: read_trustline_asset(r)?,
        }),
        LEDGER_OFFER => Ok(LedgerKey::Offer {
            seller_id: read_account_id(r)?,
            offer_id: r.read_i64()?,
        }),
        LEDGER_DATA => Ok(LedgerKey::Data {
            account_id: read_account_id(r)?,
            name: String::from_utf8_lossy(&r.read_opaque()?).into_owned(),
        }),
        LEDGER_CLAIMABLE_BALANCE => Ok(LedgerKey::ClaimableBalance {
            balance_id: read_claimable_balance_id(r)?,
        }),
        LEDGER_LIQUIDITY_POOL => Ok(LedgerKey::LiquidityPool {
            pool_id: hex_encode(r.read_fixed(32)?),
        }),
        LEDGER_CONTRACT_DATA => {
            let contract = read_sc_address(r)?;
            skip_scval(r)?;
            let durability = match r.read_u32()? {
                0 => "temporary",
                1 => "persistent",
                other => return Err(format!("unknown ContractDataDurability {other}")),
            };
            Ok(LedgerKey::ContractData {
                contract,
                durability: durability.to_string(),
            })
        }
        LEDGER_CONTRACT_CODE => Ok(LedgerKey::ContractCode {
            hash: hex_encode(r.read_fixed(32)?),
        }),
        LEDGER_CONFIG_SETTING => Ok(LedgerKey::ConfigSetting { id: r.read_u32()? }),
        LEDGER_TTL => Ok(LedgerKey::Ttl {
            key_hash: hex_encode(r.read_fixed(32)?),
        }),
        other => Err(format!("unsupported LedgerKey type {other}")),
    }
}

/// Read a StrKey-shaped `AccountID` (a `PublicKey` union with a single
/// ed25519 case) and return the 32-byte key as hex.
fn read_account_id(r: &mut XdrReader<'_>) -> Result<String, String> {
    let pk_type = r.read_u32()?;
    if pk_type != 0 {
        return Err(format!("unsupported PublicKey type {pk_type}"));
    }
    Ok(hex_encode(r.read_fixed(32)?))
}

/// Read a `SCAddress` union and return a hex encoding of its identity.
fn read_sc_address(r: &mut XdrReader<'_>) -> Result<String, String> {
    match r.read_u32()? {
        // SC_ADDRESS_TYPE_ACCOUNT: PublicKey union (ed25519 only).
        0 => read_account_id(r),
        // SC_ADDRESS_TYPE_CONTRACT / _LIQUIDITY_POOL: 32-byte hash.
        1 | 4 => Ok(hex_encode(r.read_fixed(32)?)),
        // SC_ADDRESS_TYPE_MUXED_ACCOUNT: MuxedEd25519Account { id, ed25519 }.
        2 => {
            let id = r.read_fixed(8)?;
            let key = r.read_fixed(32)?;
            let mut out = Vec::with_capacity(40);
            out.extend_from_slice(id);
            out.extend_from_slice(key);
            Ok(hex_encode(&out))
        }
        // SC_ADDRESS_TYPE_CLAIMABLE_BALANCE: ClaimableBalanceID union.
        3 => read_claimable_balance_id(r),
        other => Err(format!("unsupported ScAddress type {other}")),
    }
}

/// Read a `ClaimableBalanceID` union (only the v0 hash case exists today).
fn read_claimable_balance_id(r: &mut XdrReader<'_>) -> Result<String, String> {
    let kind = r.read_u32()?;
    if kind != 0 {
        return Err(format!("unsupported ClaimableBalanceID type {kind}"));
    }
    Ok(hex_encode(r.read_fixed(32)?))
}

/// Read a `TrustLineAsset` union and render it as `code:issuer` / `native`.
fn read_trustline_asset(r: &mut XdrReader<'_>) -> Result<String, String> {
    let kind = r.read_u32()?;
    match kind {
        // ASSET_TYPE_NATIVE
        0 => Ok("native".to_string()),
        // ASSET_TYPE_CREDIT_ALPHANUM4 / _ALPHANUM12
        1 | 2 => {
            let code_len = if kind == 1 { 4 } else { 12 };
            let code = r.read_fixed(code_len)?;
            let code = code
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect::<String>();
            let issuer = read_account_id(r)?;
            Ok(format!("{code}:{issuer}"))
        }
        // ASSET_TYPE_POOL_SHARE
        3 => Ok(format!("pool_share:{}", hex_encode(r.read_fixed(32)?))),
        other => Err(format!("unsupported TrustLineAsset type {other}")),
    }
}

/// Skip one XDR-encoded `ScVal`, so the cursor lands on the field after it.
///
/// This must decode exactly (not guess) the value's length — e.g. a
/// `contract_data` footprint key carries an arbitrary `ScVal` as its key, so
/// the cursor can only reach the following durability byte by walking that
/// value. Discriminants match `Stellar-contract.x`.
fn skip_scval(r: &mut XdrReader<'_>) -> Result<(), String> {
    let kind = r.read_u32()?;
    match kind {
        // SCV_BOOL
        0 => {
            r.read_fixed(4)?;
        }
        // SCV_VOID
        1 => {}
        // SCV_ERROR: SCErrorType + (uint32 contractCode | SCErrorCode)
        2 => {
            r.read_fixed(8)?;
        }
        // SCV_U32 / SCV_I32
        3 | 4 => {
            r.read_fixed(4)?;
        }
        // SCV_U64 / SCV_I64 / SCV_TIMEPOINT / SCV_DURATION
        5..=8 => {
            r.read_fixed(8)?;
        }
        // SCV_U128 / SCV_I128
        9 | 10 => {
            r.read_fixed(16)?;
        }
        // SCV_U256 / SCV_I256
        11 | 12 => {
            r.read_fixed(32)?;
        }
        // SCV_BYTES / SCV_STRING / SCV_SYMBOL
        13..=15 => {
            r.read_opaque()?;
        }
        // SCV_VEC: optional `SCVec*`
        16 => {
            if r.read_u32()? != 0 {
                let len = r.read_u32()? as usize;
                for _ in 0..len {
                    skip_scval(r)?;
                }
            }
        }
        // SCV_MAP: optional `SCMap*`
        17 => {
            if r.read_u32()? != 0 {
                let len = r.read_u32()? as usize;
                for _ in 0..len {
                    skip_scval(r)?;
                    skip_scval(r)?;
                }
            }
        }
        // SCV_ADDRESS
        18 => skip_sc_address(r)?,
        // SCV_CONTRACT_INSTANCE: ContractExecutable + optional SCMap storage
        19 => {
            skip_contract_executable(r)?;
            if r.read_u32()? != 0 {
                let len = r.read_u32()? as usize;
                for _ in 0..len {
                    skip_scval(r)?;
                    skip_scval(r)?;
                }
            }
        }
        // SCV_LEDGER_KEY_CONTRACT_INSTANCE
        20 => {}
        // SCV_LEDGER_KEY_NONCE: SCNonceKey { int64 nonce }
        21 => {
            r.read_fixed(8)?;
        }
        // SCV_EXECUTABLE_TAG: ContractExecutable
        22 => skip_contract_executable(r)?,
        other => return Err(format!("unsupported ScVal discriminant {other}")),
    }
    Ok(())
}

/// Skip a `SCAddress` union.
fn skip_sc_address(r: &mut XdrReader<'_>) -> Result<(), String> {
    match r.read_u32()? {
        // ACCOUNT: PublicKey union (ed25519)
        0 => {
            let pk = r.read_u32()?;
            if pk != 0 {
                return Err(format!("unsupported PublicKey type {pk}"));
            }
            r.read_fixed(32)?;
        }
        // CONTRACT / LIQUIDITY_POOL: Hash
        1 | 4 => {
            r.read_fixed(32)?;
        }
        // MUXED_ACCOUNT: MuxedEd25519Account { id, ed25519 }
        2 => {
            r.read_fixed(40)?;
        }
        // CLAIMABLE_BALANCE: ClaimableBalanceID union
        3 => {
            let cb = r.read_u32()?;
            if cb != 0 {
                return Err(format!("unsupported ClaimableBalanceID type {cb}"));
            }
            r.read_fixed(32)?;
        }
        other => return Err(format!("unsupported ScAddress type {other}")),
    }
    Ok(())
}

/// Skip a `ContractExecutable` union.
fn skip_contract_executable(r: &mut XdrReader<'_>) -> Result<(), String> {
    match r.read_u32()? {
        // CONTRACT_EXECUTABLE_WASM: Hash
        0 => {
            r.read_fixed(32)?;
        }
        // CONTRACT_EXECUTABLE_STELLAR_ASSET: void
        1 => {}
        // CONTRACT_EXECUTABLE_EXTERNAL_REF: SCAddress + SCString tag
        2 => {
            skip_sc_address(r)?;
            r.read_opaque()?;
        }
        other => return Err(format!("unsupported ContractExecutable type {other}")),
    }
    Ok(())
}

/// Collect the base64 diagnostic events from a `simulateTransaction` result.
fn extract_events_vec(result: &serde_json::Value) -> Vec<String> {
    result
        .get("events")
        .and_then(|v| v.as_array())
        .map(|events| {
            events
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a raw `simulateTransaction` JSON-RPC response body into a
/// [`SimulationResult`], or a typed [`SimulationError`].
fn parse_simulation_response(
    json: &serde_json::Value,
) -> Result<SimulationResult, SimulationError> {
    // A JSON-RPC-level error object means the endpoint rejected the request.
    if let Some(err) = json.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown JSON-RPC error")
            .to_string();
        return Err(SimulationError::Rpc { code, message });
    }

    let result = json.get("result").ok_or_else(|| {
        SimulationError::Network("simulateTransaction response is missing `result`".to_string())
    })?;

    let events = extract_events_vec(result);

    // `result.error` is how the RPC reports a reverted host function call.
    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        return Err(SimulationError::Contract {
            message: err.to_string(),
            events,
        });
    }

    let min_resource_fee = result
        .get("minResourceFee")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| {
            SimulationError::Network(
                "simulateTransaction response is missing a valid `minResourceFee`".to_string(),
            )
        })?;

    let (footprint, resource_fee, instructions, read_bytes, write_bytes) = match result
        .get("transactionData")
        .and_then(|v| v.as_str())
    {
        Some(data) => parse_transaction_data(data).map_err(SimulationError::Network)?,
        None => (ResourceFootprint::default(), 0, 0, 0, 0),
    };

    let raw_result_xdr = result
        .get("results")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|r| r.get("xdr"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Best-effort decode of the return value. A mutating invocation commonly
    // returns a scalar or void — still valid, just not the map shape the
    // Trellis read-only queries use — so a decode failure is recorded rather
    // than failing the whole simulation.
    let (decoded_result, result_decode_error) = match raw_result_xdr.as_deref() {
        Some(xdr) => match decode_scval_json(xdr) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        },
        None => (None, None),
    };

    Ok(SimulationResult {
        min_resource_fee,
        resource_fee,
        footprint,
        instructions,
        read_bytes,
        write_bytes,
        result: decoded_result,
        raw_result_xdr,
        result_decode_error,
        events,
        latest_ledger: result.get("latestLedger").and_then(|v| v.as_u64()),
    })
}

/// Resolve which `stellar` executable to invoke.
///
/// The integration suite (`tests/cli_integration.rs`) sets
/// `TRELLIS_TEST_MODE=true` together with `STELLAR_MOCK_BIN=<script>` so the
/// tests exercise the full argv-building / output-rendering path against a
/// mock script instead of a live network or a real CLI install. In every
/// other case this is just `"stellar"` from `PATH`.
pub(crate) fn stellar_bin() -> String {
    if std::env::var_os("TRELLIS_TEST_MODE").is_some() {
        if let Some(mock) = std::env::var_os("STELLAR_MOCK_BIN") {
            return mock.to_string_lossy().into_owned();
        }
    }
    "stellar".to_string()
}

/// Decode a base64-encoded Soroban `ScVal` XDR result into the CLI's
/// internal `ContractResult` representation.
///
/// This is the native replacement for shelling out to `stellar contract
/// invoke` and re-printing its stdout: callers that already have a raw XDR
/// `ScVal` (e.g. from `simulateTransaction`) can decode it directly into the
/// same shape `render_json` / `render_human` consume.
///
/// The decoder understands the two result shapes produced by the Trellis
/// contract's read-only queries:
///
/// * `get_agreement` → a `ScVal::Map` with fields `id`, `payer`, `payee`,
///   `amount`, `status`, `milestone_count`.
/// * `get_milestone` → a `ScVal::Map` with fields `agreement_id`, `index`,
///   `amount`, `status`, `released`.
///
/// Returns `Err` with a human-readable message when the XDR is malformed or
/// the top-level value is not a map (so callers can surface a clear error
/// instead of silently printing garbage).
pub fn decode_scval_result(xdr_base64: &str) -> Result<ContractResult, String> {
    let raw = base64_decode(xdr_base64)
        .map_err(|e| format!("invalid base64 in ScVal result: {e}"))?;
    let val = parse_scval(&raw)
        .map_err(|e| format!("failed to parse ScVal XDR: {e}"))?;
    scval_to_contract_result(&val)
}

/// Decode a base64-encoded Soroban `ScVal` XDR result into a
/// [`serde_json::Value`].
///
/// Unlike [`decode_scval_result`], which requires the top-level value to be a
/// map (the shape of the Trellis read-only queries), this accepts *any*
/// `ScVal` and is therefore the right decoder for a `simulateTransaction`
/// `results[0].xdr` payload — a mutating invocation's simulated return value
/// is often a scalar or `void` rather than a map.
pub fn decode_scval_json(xdr_base64: &str) -> Result<serde_json::Value, String> {
    let raw = base64_decode(xdr_base64)
        .map_err(|e| format!("invalid base64 in ScVal result: {e}"))?;
    let val = parse_scval(&raw)
        .map_err(|e| format!("failed to parse ScVal XDR: {e}"))?;
    Ok(scval_to_json(&val))
}

/// Minimal base64 decoder (standard alphabet, `=` padding) so the CLI does
/// not need an extra dependency just for result decoding.
fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return Err("length is not a multiple of 4".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|&&b| b == b'=').count();
        if pad > 2 {
            return Err("too much padding".to_string());
        }
        let mut n: u32 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            let v = if b == b'=' {
                0
            } else {
                val(b).ok_or_else(|| format!("invalid base64 byte 0x{b:02x} at index {i}"))?
            };
            n = (n << 6) | v as u32;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// A parsed subset of the Soroban `ScVal` union — just enough to represent
/// the values returned by `get_agreement` / `get_milestone`.
#[derive(Debug, Clone, PartialEq)]
enum ScVal {
    Void,
    Bool(bool),
    /// `SCV_ERROR` — an `SCError` union rendered as a diagnostic string.
    Error(String),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    U128(u128),
    I128(i128),
    Symbol(String),
    String(String),
    Bytes(Vec<u8>),
    Address(String),
    Map(Vec<(ScVal, ScVal)>),
    Vec(Vec<ScVal>),
}

/// Parse a raw `ScVal` XDR blob into the local `ScVal` enum.
///
/// This is a deliberately small reader: it walks the XDR discriminant and
/// payload for the variants the Trellis contract actually returns. Unknown
/// discriminants produce an error rather than a silent mis-decode.
fn parse_scval(bytes: &[u8]) -> Result<ScVal, String> {
    let mut cur = std::io::Cursor::new(bytes);
    read_scval(&mut cur)
}

fn read_u32(cur: &mut std::io::Cursor<&[u8]>) -> Result<u32, String> {
    use std::io::Read;
    let mut buf = [0u8; 4];
    cur.read_exact(&mut buf)
        .map_err(|e| format!("unexpected end of XDR: {e}"))?;
    Ok(u32::from_be_bytes(buf))
}

fn read_u64(cur: &mut std::io::Cursor<&[u8]>) -> Result<u64, String> {
    use std::io::Read;
    let mut buf = [0u8; 8];
    cur.read_exact(&mut buf)
        .map_err(|e| format!("unexpected end of XDR: {e}"))?;
    Ok(u64::from_be_bytes(buf))
}

/// Read a length-prefixed XDR opaque value, consuming the 0–3 zero bytes of
/// padding that align it to the next 4-byte boundary.
///
/// The padding matters for nested values: without it a short symbol inside a
/// map/vec would leave the cursor misaligned and every following field would
/// be decoded from the wrong offset.
fn read_opaque(cur: &mut std::io::Cursor<&[u8]>) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let len = read_u32(cur)? as usize;
    let mut buf = vec![0u8; len];
    cur.read_exact(&mut buf)
        .map_err(|e| format!("unexpected end of XDR: {e}"))?;
    let pad = (4 - (len % 4)) % 4;
    if pad > 0 {
        let mut skip = [0u8; 4];
        cur.read_exact(&mut skip[..pad])
            .map_err(|e| format!("unexpected end of XDR padding: {e}"))?;
    }
    Ok(buf)
}

fn read_scval(cur: &mut std::io::Cursor<&[u8]>) -> Result<ScVal, String> {
    let disc = read_u32(cur)?;
    match disc {
        0 => Ok(ScVal::Bool(read_u32(cur)? != 0)),
        1 => Ok(ScVal::Void),
        2 => {
            let kind = read_u32(cur)?;
            let code = read_u32(cur)?;
            Ok(ScVal::Error(format!("SCError(type={kind}, code={code})")))
        }
        3 => Ok(ScVal::U32(read_u32(cur)?)),
        4 => Ok(ScVal::I32(read_u32(cur)? as i32)),
        5 => Ok(ScVal::U64(read_u64(cur)?)),
        6 => Ok(ScVal::I64(read_u64(cur)? as i64)),
        7 | 8 => Ok(ScVal::U64(read_u64(cur)?)),
        9 => {
            let hi = read_u64(cur)? as u128;
            let lo = read_u64(cur)? as u128;
            Ok(ScVal::U128((hi << 64) | lo))
        }
        10 => {
            let hi = read_u64(cur)? as i64 as i128;
            let lo = read_u64(cur)? as i128;
            Ok(ScVal::I128((hi << 64) | lo))
        }
        13 => Ok(ScVal::Bytes(read_opaque(cur)?)),
        14 => Ok(ScVal::String(
            String::from_utf8_lossy(&read_opaque(cur)?).into_owned(),
        )),
        15 => Ok(ScVal::Symbol(
            String::from_utf8_lossy(&read_opaque(cur)?).into_owned(),
        )),
        // SCV_VEC: `SCVec*` — an optional pointer, so a presence flag
        // precedes the length. A null vec decodes to an empty vec.
        16 => {
            if read_u32(cur)? == 0 {
                return Ok(ScVal::Vec(Vec::new()));
            }
            let len = read_u32(cur)? as usize;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(read_scval(cur)?);
            }
            Ok(ScVal::Vec(items))
        }
        // SCV_MAP: `SCMap*` — same optional-pointer encoding.
        17 => {
            if read_u32(cur)? == 0 {
                return Ok(ScVal::Map(Vec::new()));
            }
            let len = read_u32(cur)? as usize;
            let mut entries = Vec::with_capacity(len);
            for _ in 0..len {
                let k = read_scval(cur)?;
                let v = read_scval(cur)?;
                entries.push((k, v));
            }
            Ok(ScVal::Map(entries))
        }
        // SCV_ADDRESS: the payload is a ScAddress union. We only need a
        // printable form; the contract's read-only queries return
        // account/contract addresses, so we render the raw XDR bytes as hex
        // and let callers that need StrKey re-encode.
        18 => {
            let addr_type = read_u32(cur)?;
            match addr_type {
                0 => {
                    // SC_ADDRESS_TYPE_ACCOUNT: PublicKey union, Ed25519 = 0.
                    let pk_type = read_u32(cur)?;
                    if pk_type != 0 {
                        return Err(format!("unsupported PublicKey type {pk_type}"));
                    }
                    let mut buf = [0u8; 32];
                    use std::io::Read;
                    cur.read_exact(&mut buf)
                        .map_err(|e| format!("unexpected end of XDR: {e}"))?;
                    Ok(ScVal::Address(hex_encode(&buf)))
                }
                // SC_ADDRESS_TYPE_CONTRACT / _LIQUIDITY_POOL: 32-byte hash.
                1 | 4 => {
                    let mut buf = [0u8; 32];
                    use std::io::Read;
                    cur.read_exact(&mut buf)
                        .map_err(|e| format!("unexpected end of XDR: {e}"))?;
                    Ok(ScVal::Address(hex_encode(&buf)))
                }
                other => Err(format!("unsupported ScAddress type {other}")),
            }
        }
        other => Err(format!("unsupported ScVal discriminant {other}")),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Convert a decoded `ScVal::Map` into the CLI's `ContractResult` shape.
fn scval_to_contract_result(val: &ScVal) -> Result<ContractResult, String> {
    let map = match val {
        ScVal::Map(m) => m,
        other => {
            return Err(format!(
                "expected ScVal::Map at top level, got {other:?}"
            ))
        }
    };
    let mut result = ContractResult::default();
    for (k, v) in map {
        let key = match k {
            ScVal::Symbol(s) | ScVal::String(s) => s.clone(),
            other => return Err(format!("non-string map key: {other:?}")),
        };
        result.fields.insert(key, scval_to_json(v));
    }
    Ok(result)
}

/// Render a decoded `ScVal` as a `serde_json::Value` so it slots directly
/// into the existing `render_json` output.
fn scval_to_json(val: &ScVal) -> serde_json::Value {
    use serde_json::Value;
    match val {
        ScVal::Void => Value::Null,
        ScVal::Bool(b) => Value::Bool(*b),
        ScVal::Error(e) => Value::String(e.clone()),
        ScVal::U32(n) => Value::from(*n),
        ScVal::I32(n) => Value::from(*n),
        ScVal::U64(n) => Value::from(*n),
        ScVal::I64(n) => Value::from(*n),
        ScVal::U128(n) => Value::from(n.to_string()),
        ScVal::I128(n) => Value::from(n.to_string()),
        ScVal::Symbol(s) | ScVal::String(s) => Value::from(s.clone()),
        ScVal::Bytes(b) => Value::from(hex_encode(b)),
        ScVal::Address(a) => Value::from(a.clone()),
        ScVal::Vec(items) => Value::Array(items.iter().map(scval_to_json).collect()),
        ScVal::Map(entries) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in entries {
                let key = match k {
                    ScVal::Symbol(s) | ScVal::String(s) => s.clone(),
                    other => format!("{other:?}"),
                };
                obj.insert(key, scval_to_json(v));
            }
            Value::Object(obj)
        }
    }
}

/// Native Soroban RPC client that talks directly to the Soroban JSON-RPC endpoint.
/// No external CLI dependency required.
pub struct RpcClient;

impl RpcClient {
    /// Invoke a Trellis contract function.
    ///
    /// Currently delegates to `stellar contract invoke` (see the type-level
    /// docs for the architecture and the planned native RPC rewrite).
    ///
    /// Transient RPC failures (timeouts, rate limits, temporary unavailability)
    /// are automatically retried with exponential backoff and jitter. The number
    /// of retries is controlled by `STELLAR_RPC_RETRIES` (default 3).
    ///
    /// # Arguments
    /// * `config`  – runtime configuration (RPC URL, keys, contract ID)
    /// * `fn_name` – the Soroban function name (e.g. `"init"`, `"lock_funds"`)
    /// * `args`    – a flat list of `--flag value` pairs **after** the `--`
    ///   separator, e.g. `["--agreement_id", "0x…", "--payer", "G…"]`
    /// * `quiet`   – suppress the retry progress messages normally printed to stderr
    pub fn invoke(config: &Config, fn_name: &str, args: &[String], quiet: bool) -> InvokeOutput {
        // TODO(native-rpc): replace this shell-out with direct Soroban
        // JSON-RPC calls (typed arg parsing, key loading, envelope signing,
        // submit + poll). See the `RpcClient` type docs for the full plan.
        // Until then we delegate to the `stellar` CLI, which already handles
        // argument encoding, transaction assembly, signing and submission.
        Self::invoke_with_retry(config, fn_name, args, quiet)
    }

    /// Build the exact `stellar contract invoke …` argument list and its
    /// copy-paste-friendly command string, without executing anything.
    ///
    /// Shared by the real invocation path (so failures can print the command
    /// that ran) and by `--dry-run` previews (which never execute at all).
    fn build_cmd_args(config: &Config, fn_name: &str, args: &[String]) -> (Vec<String>, String) {
        let mut cmd_args: Vec<String> = vec![
            "contract".to_string(),
            "invoke".to_string(),
            "--id".to_string(),
            config.contract_id.clone(),
        ];

        // #240: a raw `S…` secret seed must never land in argv — anyone on the
        // host can read it via `ps`. Pass it to the child through the
        // `STELLAR_SECRET_KEY` environment variable instead (see
        // `invoke_once`); only non-secret identity names go on the command
        // line. Named `stellar keys` identities are still passed via
        // `--source` exactly as before.
        if !crate::config::is_secret_seed(&config.source_key) {
            cmd_args.push("--source".to_string());
            cmd_args.push(config.source_key.clone());
        }

        cmd_args.extend_from_slice(&[
            "--rpc-url".to_string(),
            config.rpc_url.clone(),
            "--network-passphrase".to_string(),
            config.network_passphrase.clone(),
            "--".to_string(),
            fn_name.to_string(),
        ]);
        cmd_args.extend_from_slice(args);

        // Quote any argument containing whitespace so the printed command can
        // be copy-pasted straight into a shell.
        let quoted = cmd_args
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("'{a}'")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        // Never print the seed itself — show that it is supplied via the
        // environment so `--dry-run` / failure output stays copy-pasteable
        // without leaking the key.
        let command_debug = if crate::config::is_secret_seed(&config.source_key) {
            format!("STELLAR_SECRET_KEY=<redacted> stellar {quoted}")
        } else {
            format!("stellar {quoted}")
        };

        (cmd_args, command_debug)
    }

    /// Build the `stellar contract invoke …` command that *would* run for
    /// `fn_name`/`args`, without executing it. Used by `--dry-run`.
    pub fn preview(config: &Config, fn_name: &str, args: &[String]) -> String {
        Self::build_cmd_args(config, fn_name, args).1
    }

    /// Send a signed transaction envelope via `sendTransaction` JSON-RPC,
    /// then poll `getTransaction` on an interval until `SUCCESS`, `FAILED`, or timeout.
    ///
    /// Reuses the CLI's existing retry/backoff conventions for the polling loop.
    pub fn send_and_poll(config: &Config, envelope_xdr: &str, quiet: bool) -> InvokeOutput {
        let max_retries: u32 = std::env::var("STELLAR_RPC_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        const BACKOFF_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];
        let mut attempt = 0u32;

        let client = reqwest::blocking::Client::new();
        let rpc_url = &config.rpc_url;

        // 1. Send transaction
        let send_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": {
                "transaction": envelope_xdr
            }
        });

        let send_res = loop {
            apply_rate_limit();
            match client.post(rpc_url).json(&send_body).send() {
                Ok(resp) => match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        if let Some(err) = json.get("error") {
                            let err_msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("unknown RPC error");
                            if attempt >= max_retries {
                                return InvokeOutput {
                                    stdout: String::new(),
                                    stderr: format!("sendTransaction failed: {}", err_msg),
                                    success: false,
                                    command_debug: format!("sendTransaction({})", rpc_url),
                                };
                            }
                        } else if let Some(result) = json.get("result") {
                            let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            if status == "PENDING" || status == "SUCCESS" {
                                if let Some(hash) = result.get("hash").and_then(|v| v.as_str()) {
                                    break hash.to_string();
                                }
                            }
                            if status == "ERROR" || status == "FAILED" {
                                let error_result = result.get("errorResult").map(|v| v.to_string()).unwrap_or_else(|| "transaction failed".to_string());
                                return InvokeOutput {
                                    stdout: String::new(),
                                    stderr: format!("Transaction failed: {}", error_result),
                                    success: false,
                                    command_debug: format!("sendTransaction({})", rpc_url),
                                };
                            }
                            if let Some(hash) = result.get("hash").and_then(|v| v.as_str()) {
                                break hash.to_string();
                            }
                        }
                    }
                    Err(e) => {
                        if attempt >= max_retries {
                            return InvokeOutput {
                                stdout: String::new(),
                                stderr: format!("Failed to parse sendTransaction response: {}", e),
                                success: false,
                                command_debug: format!("sendTransaction({})", rpc_url),
                            };
                        }
                    }
                },
                Err(e) => {
                    if attempt >= max_retries {
                        return InvokeOutput {
                            stdout: String::new(),
                            stderr: format!("sendTransaction network error: {}", e),
                            success: false,
                            command_debug: format!("sendTransaction({})", rpc_url),
                        };
                    }
                }
            }

            let idx = (attempt as usize).min(BACKOFF_MS.len() - 1);
            let base_ms = BACKOFF_MS[idx];
            let jitter = (std::time::Instant::now().elapsed().subsec_nanos() % 200) as u64;
            let sleep_duration = std::time::Duration::from_millis(base_ms + jitter);

            if !quiet {
                eprintln!("⚠️  sendTransaction transient error (attempt {}/{}), retrying in {}ms...", attempt + 1, max_retries, base_ms + jitter);
            }

            std::thread::sleep(sleep_duration);
            attempt += 1;
        };

        // 2. Poll getTransaction until terminal status or timeout
        let max_polls: u32 = std::env::var("STELLAR_RPC_POLL_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        let mut poll_attempt = 0u32;
        loop {
            apply_rate_limit();
            let poll_body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getTransaction",
                "params": {
                    "hash": send_res
                }
            });

            match client.post(rpc_url).json(&poll_body).send() {
                Ok(resp) => match resp.json::<serde_json::Value>() {
                    Ok(json) => {
                        if let Some(result) = json.get("result") {
                            let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            match status {
                                "SUCCESS" => {
                                    return InvokeOutput {
                                        stdout: serde_json::to_string_pretty(&result).unwrap_or_default(),
                                        stderr: String::new(),
                                        success: true,
                                        command_debug: format!("getTransaction({})", send_res),
                                    };
                                }
                                "FAILED" | "ERROR" => {
                                    let err_res = result.get("errorResult").map(|v| v.to_string()).unwrap_or_default();
                                    return InvokeOutput {
                                        stdout: String::new(),
                                        stderr: format!("Transaction failed on-chain: status={}, errorResult={}", status, err_res),
                                        success: false,
                                        command_debug: format!("getTransaction({})", send_res),
                                    };
                                }
                                _ => {
                                    // PENDING or other non-terminal status
                                }
                            }
                        }
                    }
                    Err(_) => {}
                },
                Err(_) => {}
            }

            if poll_attempt >= max_polls {
                return InvokeOutput {
                    stdout: String::new(),
                    stderr: format!("Transaction polling timed out after {} attempts (hash: {})", max_polls, send_res),
                    success: false,
                    command_debug: format!("getTransaction({})", send_res),
                };
            }

            std::thread::sleep(std::time::Duration::from_secs(1));
            poll_attempt += 1;
        }
    }

    /// Simulate a contract invocation via the native Soroban JSON-RPC
    /// `simulateTransaction` method, without signing or submitting anything.
    ///
    /// This is the native counterpart to shelling out to `stellar contract
    /// invoke`: callers pass a base64 `TransactionEnvelope` XDR, exactly like
    /// [`RpcClient::send_and_poll`], and receive the recommended resource
    /// footprint, minimum resource fee and — for a read-only invocation — the
    /// decoded return value.
    ///
    /// Unlike `send_and_poll`, this never mutates chain state; it is also the
    /// mechanism for fetching a read-only result with no signing key at all,
    /// because the network executes the invocation without ever asking for a
    /// signature.
    ///
    /// Transient network failures are retried with the same exponential
    /// backoff as the rest of the CLI (`STELLAR_RPC_RETRIES`, default 3).
    /// Contract-level failures (`result.error`) become
    /// [`SimulationError::Contract`] and are never retried.
    pub fn simulate_transaction(
        config: &Config,
        envelope_xdr: &str,
        quiet: bool,
    ) -> Result<SimulationResult, SimulationError> {
        let max_retries: u32 = std::env::var("STELLAR_RPC_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        const BACKOFF_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

        let mut attempt = 0u32;
        loop {
            match Self::simulate_once(config, envelope_xdr) {
                Ok(result) => return Ok(result),
                Err(err) => {
                    // Only transport-level failures are transient; an RPC or
                    // contract rejection will fail identically on every retry.
                    let transient = match &err {
                        SimulationError::Network(msg) => is_transient_error(msg),
                        SimulationError::Rpc { .. } | SimulationError::Contract { .. } => false,
                    };
                    if !transient || attempt >= max_retries {
                        return Err(err);
                    }

                    attempt += 1;
                    let idx = ((attempt - 1) as usize).min(BACKOFF_MS.len() - 1);
                    let delay_ms = BACKOFF_MS[idx] + jitter_millis();

                    if !quiet {
                        eprintln!(
                            "simulateTransaction attempt {attempt}/{max_retries} failed \
                             (transient error), retrying in {delay_ms}ms…"
                        );
                    }

                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
        }
    }

    /// A single native `simulateTransaction` request — no retry logic here.
    fn simulate_once(
        config: &Config,
        envelope_xdr: &str,
    ) -> Result<SimulationResult, SimulationError> {
        let client = reqwest::blocking::Client::new();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "simulateTransaction",
            "params": {
                "transaction": envelope_xdr,
            }
        });

        apply_rate_limit();

        let response = client
            .post(config.rpc_url.as_str())
            .json(&body)
            .send()
            .map_err(|e| SimulationError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(SimulationError::Network(format!(
                "HTTP {} from {}",
                status.as_u16(),
                config.rpc_url
            )));
        }

        let json: serde_json::Value = response.json().map_err(|e| {
            SimulationError::Network(format!("malformed simulateTransaction response: {e}"))
        })?;

        parse_simulation_response(&json)
    }
    ///
    /// Backoff schedule (before jitter): 1 s, 2 s, 4 s, 8 s (capped).
    /// Jitter adds up to 200 ms derived from the current system clock so
    /// concurrent processes do not thunder-herd the RPC endpoint together.
    ///
    /// Set `STELLAR_RPC_RETRIES=0` to disable retries entirely.
    fn invoke_with_retry(
        config: &Config,
        fn_name: &str,
        args: &[String],
        quiet: bool,
    ) -> InvokeOutput {
        let max_retries: u32 = std::env::var("STELLAR_RPC_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        // Exponential backoff: 1 s → 2 s → 4 s → 8 s (capped at index 3).
        const BACKOFF_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

        let mut attempt = 0u32;
        loop {
            let out = Self::invoke_once(config, fn_name, args);

            if out.success {
                return out;
            }

            // Spawn failure means the stellar CLI is not installed — no point retrying.
            if out.stderr.starts_with("Failed to spawn") {
                return out;
            }

            if attempt >= max_retries {
                return out;
            }

            // Only retry errors that look like transient network / RPC issues.
            if !is_transient_error(&out.stderr) {
                return out;
            }

            attempt += 1;
            let idx = ((attempt - 1) as usize).min(BACKOFF_MS.len() - 1);
            let base_ms = BACKOFF_MS[idx];
            let jitter_ms = jitter_millis();
            let delay_ms = base_ms + jitter_ms;

            if !quiet {
                eprintln!(
                    "RPC attempt {attempt}/{max_retries} failed (transient error), retrying in {delay_ms}ms…"
                );
                eprintln!(
                    "  {}",
                    out.stderr.lines().next().unwrap_or("(no error message)")
                );
            }

            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }

    /// Single attempt at invoking the stellar CLI — no retry logic here.
    ///
    /// A hard timeout is applied: if the child process does not finish within
    /// `STELLAR_INVOKE_TIMEOUT_SECS` (default 30 s), it is forcibly killed and
    /// an `InvokeOutput` with a transient-error message is returned so the
    /// caller's retry loop can act on it. Set `STELLAR_INVOKE_TIMEOUT_SECS=0`
    /// to disable the timeout.
    fn invoke_once(config: &Config, fn_name: &str, args: &[String]) -> InvokeOutput {
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::Duration;

        let (cmd_args, command_debug) = Self::build_cmd_args(config, fn_name, args);

        apply_rate_limit();
        // Resolve the per-call timeout from the environment.
        let timeout_secs: u64 = std::env::var("STELLAR_INVOKE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_INVOKE_TIMEOUT_SECS);

        let mut command = Command::new(stellar_bin());
        command.args(&cmd_args);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        // #240: hand a raw secret seed to the child via its environment rather
        // than argv so it cannot be read from `ps` / `/proc/<pid>/cmdline`.
        if crate::config::is_secret_seed(&config.source_key) {
            command.env("STELLAR_SECRET_KEY", &config.source_key);
        }

        // Spawn the child process.
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                return InvokeOutput {
                    stdout: String::new(),
                    stderr: format!(
                        "Failed to spawn `stellar` CLI: {e}\n\
                         Is the Stellar CLI installed?  https://developers.stellar.org/docs/tools/cli/install-cli"
                    ),
                    success: false,
                    command_debug,
                };
            }
        };

        // Zero timeout means "no limit" — fall back to a simple blocking wait.
        if timeout_secs == 0 {
            let output = child.wait_with_output();
            return match output {
                Ok(out) => InvokeOutput {
                    stdout: decode_process_output("stdout", out.stdout),
                    stderr: decode_process_output("stderr", out.stderr),
                    success: out.status.success(),
                    command_debug,
                },
                Err(e) => InvokeOutput {
                    stdout: String::new(),
                    stderr: format!("Failed to wait for `stellar` CLI: {e}"),
                    success: false,
                    command_debug,
                },
            };
        }

        // Drive the child on a background thread; the main thread races it
        // against a deadline via an `mpsc` channel so it can kill the process
        // if it exceeds the timeout without blocking forever itself.
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = child.wait_with_output();
            // Ignore send errors: a timeout-triggered kill may have caused
            // the receiver to drop before we get here.
            let _ = tx.send(result);
        });

        match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
            Ok(Ok(out)) => {
                let _ = handle.join();
                InvokeOutput {
                    stdout: decode_process_output("stdout", out.stdout),
                    stderr: decode_process_output("stderr", out.stderr),
                    success: out.status.success(),
                    command_debug,
                }
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                InvokeOutput {
                    stdout: String::new(),
                    stderr: format!("Failed to wait for `stellar` CLI: {e}"),
                    success: false,
                    command_debug,
                }
            }
            // Timeout — the child is still running.  Kill it, then let the
            // background thread finish so we do not leak OS resources.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // `wait_with_output` inside the thread has taken ownership of
                // `child`, so we cannot call `child.kill()` directly any more.
                // The thread will see the process exit (or an error) after the
                // OS kills it through the handle it holds internally. We detach
                // here; the OS will reap the zombie when the thread unblocks.
                drop(handle);
                InvokeOutput {
                    stdout: String::new(),
                    stderr: format!(
                        "subprocess timed out: `stellar` did not respond within \
                         {timeout_secs}s. The process has been abandoned. \
                         You can raise STELLAR_INVOKE_TIMEOUT_SECS to allow more time."
                    ),
                    success: false,
                    command_debug,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                InvokeOutput {
                    stdout: String::new(),
                    stderr: "internal error: worker thread exited unexpectedly".to_string(),
                    success: false,
                    command_debug,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Native Soroban JSON-RPC (read-only methods)
// ---------------------------------------------------------------------------
//
// First slice of the native-RPC work: methods that need no signing and no
// XDR, just a JSON request/response, are POSTed straight to
// `config.rpc_url`. Contract invokes still go through `stellar` above.

/// Per-request timeout for native JSON-RPC calls. Generous enough for a slow
/// public endpoint, short enough that a dead one fails fast.
const NATIVE_RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Typed result of the Soroban `getHealth` JSON-RPC method.
///
/// Parsed from the RPC's camelCase keys; serialised (e.g. by `trellis
/// health --json`) with the snake_case field names below.
///
/// Only `status` is guaranteed by every RPC release; the ledger fields were
/// added later, so they are optional rather than failing the parse against
/// an older endpoint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct HealthStatus {
    /// `"healthy"` when the node is in sync; anything else is unhealthy.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_ledger: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_ledger: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger_retention_window: Option<u32>,
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        self.status == "healthy"
    }
}

/// Typed result of the Soroban `getLatestLedger` JSON-RPC method.
///
/// The building block for TTL-aware transaction building: a transaction's
/// validity window and a footprint's `liveUntilLedger` are both expressed
/// relative to `sequence`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct LatestLedger {
    /// Hex-encoded ledger hash (the RPC names this field `id`).
    #[serde(rename(deserialize = "id"))]
    pub hash: String,
    /// Stellar protocol version the ledger closed under.
    pub protocol_version: u32,
    /// Ledger sequence number.
    pub sequence: u32,
}

/// A JSON-RPC 2.0 response envelope: exactly one of `result` / `error`.
#[derive(serde::Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(serde::Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl RpcClient {
    /// Call the Soroban `getHealth` method natively (no `stellar` binary).
    ///
    /// Returns `Err` on a transport failure, a non-2xx HTTP status, a
    /// JSON-RPC `error` object, or a body that does not parse. An endpoint
    /// that answers but reports itself unhealthy is `Ok` — check
    /// [`HealthStatus::is_healthy`].
    pub fn get_health(config: &Config) -> Result<HealthStatus, String> {
        native_call(&config.rpc_url, "getHealth")
    }

    /// Call the Soroban `getLatestLedger` method natively (no `stellar`
    /// binary). Same error contract as [`Self::get_health`].
    pub fn get_latest_ledger(config: &Config) -> Result<LatestLedger, String> {
        native_call(&config.rpc_url, "getLatestLedger")
    }

    /// Describe the native request `method` would send, without sending it.
    /// Used by `--dry-run` and printed on failure, like [`Self::preview`].
    pub fn native_preview(config: &Config, method: &str) -> String {
        format!("POST {} {}", config.rpc_url, json_rpc_request(method))
    }
}

/// Build the JSON-RPC 2.0 request body for a parameterless `method`.
fn json_rpc_request(method: &str) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method })
}

/// POST a parameterless JSON-RPC `method` to `rpc_url` and decode `result`.
fn native_call<T: serde::de::DeserializeOwned>(rpc_url: &str, method: &str) -> Result<T, String> {
    apply_rate_limit();

    let client = reqwest::blocking::Client::builder()
        .timeout(NATIVE_RPC_TIMEOUT)
        .build()
        .map_err(|e| format!("{method}: failed to build HTTP client: {e}"))?;

    let response = client
        .post(rpc_url)
        .json(&json_rpc_request(method))
        .send()
        .map_err(|e| format!("{method}: request to {rpc_url} failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("{method}: failed to read response body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "{method}: HTTP {status} from {rpc_url}: {}",
            body.trim()
        ));
    }

    parse_json_rpc_response(method, &body)
}

/// Decode a JSON-RPC response body into `T`, surfacing an `error` object.
fn parse_json_rpc_response<T: serde::de::DeserializeOwned>(
    method: &str,
    body: &str,
) -> Result<T, String> {
    let envelope: JsonRpcResponse<T> = serde_json::from_str(body).map_err(|e| {
        format!(
            "{method}: malformed JSON-RPC response ({e}): {}",
            body.trim()
        )
    })?;

    if let Some(err) = envelope.error {
        return Err(format!("{method}: RPC error {}: {}", err.code, err.message));
    }
    envelope
        .result
        .ok_or_else(|| format!("{method}: JSON-RPC response has neither result nor error"))
}

/// Decode a subprocess output stream, without silently discarding bytes.
///
/// `String::from_utf8_lossy` replaces every invalid byte sequence with
/// U+FFFD, which can erase the very error detail a caller needs to debug a
/// non-UTF-8 failure. This tries strict UTF-8 first; on failure it falls
/// back to Latin-1 (ISO-8859-1), a direct byte→codepoint mapping that never
/// fails and preserves every original byte, and logs a warning to stderr
/// (including a hex preview of the raw bytes) so the user still has the
/// original context even though the string could not be decoded cleanly.
fn decode_process_output(label: &str, bytes: Vec<u8>) -> String {
    let (decoded, fell_back) = decode_bytes(&bytes);
    if let Some(first_bad) = fell_back {
        eprintln!(
            "warning: stellar CLI {label} was not valid UTF-8 ({} bytes, first \
             invalid byte at offset {first_bad}); decoded as Latin-1 — output \
             may not render correctly. Raw bytes (hex): {}",
            bytes.len(),
            hex_preview(&bytes),
        );
    }
    decoded
}

/// Decode `bytes` as UTF-8, falling back to a lossless Latin-1 mapping.
///
/// Returns the decoded string and, when the Latin-1 fallback was used, the
/// byte offset of the first invalid UTF-8 sequence (so callers can point at
/// exactly where the stream stopped being valid UTF-8).
fn decode_bytes(bytes: &[u8]) -> (String, Option<usize>) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), None),
        Err(e) => (
            // Latin-1: every byte maps 1:1 to U+0000..=U+00FF, so no byte is
            // ever lost and the original stream can be recovered.
            bytes.iter().map(|&b| b as char).collect(),
            Some(e.valid_up_to()),
        ),
    }
}

/// Render up to the first 64 bytes of `bytes` as space-separated hex, so a
/// non-UTF-8 stream still leaves a reproducible trace in the warning.
fn hex_preview(bytes: &[u8]) -> String {
    const MAX: usize = 64;
    let mut out = bytes
        .iter()
        .take(MAX)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if bytes.len() > MAX {
        out.push_str(&format!(" … (+{} more)", bytes.len() - MAX));
    }
    out
}

/// Extract the value of a top-level string field from a JSON object without
/// pulling in a JSON dependency.
///
/// This is intentionally minimal: it looks for `"<field>"` followed by a
/// colon and a double-quoted string, and returns the unescaped contents. It is
/// only used for the small, well-formed `getNetwork` response.
fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let start = json.find(&needle)? + needle.len();
    let after = &json[start..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let mut chars = rest.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Return true when stderr content indicates a transient, retriable RPC error.
///
/// Matches common patterns from Stellar RPC responses, HTTP errors, and
/// OS-level network failures. Contract-level errors (e.g. "contract not found",
/// "invalid argument") do not match and will not be retried.
///
/// Patterns are deliberately specific. A bare `"network"` substring, for
/// example, also matches the *permanent* error "network passphrase mismatch",
/// which would send the CLI into an endless retry loop (issue #249). Each entry
/// below is an exact phrase that only appears in genuinely transient failures;
/// add a negative test to `non_transient_*` whenever a new pattern is added.
///
/// HTTP status codes (429, 502, 503, 504) are matched with
/// `contains_http_status_code` rather than a space-anchored substring so that
/// shapes like `"429: too many requests"` or `"Error(429)"` are also detected
/// (issue #412).
fn is_transient_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    const TRANSIENT_PATTERNS: &[&str] = &[
        "subprocess timed out",
        "timeout",
        "timed out",
        "connection refused",
        "connection reset",
        "connection closed",
        "connection error",
        "network error",
        "network timeout",
        "network is unreachable",
        "network is down",
        "temporary failure in name resolution",
        "rate limit",
        "too many requests",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "deadline exceeded",
        "host unreachable",
        "no route to host",
    ];
    if TRANSIENT_PATTERNS.iter().any(|p| lower.contains(p)) {
        return true;
    }
    // Match transient HTTP status codes regardless of surrounding punctuation
    // (e.g. "429: ...", "Error(429)", "status=429", " 429 ").  We require only
    // that the three-digit code is not immediately adjacent to another digit so
    // we do not accidentally match a longer number such as "14290" or "5040".
    const TRANSIENT_CODES: &[&str] = &["429", "502", "503", "504"];
    TRANSIENT_CODES
        .iter()
        .any(|code| contains_http_status_code(&lower, code))
}

/// Return true when `haystack` contains `code` (a bare ASCII digit sequence)
/// that is not immediately preceded or followed by another ASCII digit.
///
/// This is a simple word-boundary check that avoids pulling in the `regex`
/// crate while still matching all common error-text shapes:
/// `"HTTP 429"`, `"429: too many requests"`, `"Error(429)"`, `"status=429"`.
fn contains_http_status_code(haystack: &str, code: &str) -> bool {
    let bytes = haystack.as_bytes();
    let code_bytes = code.as_bytes();
    let code_len = code_bytes.len();

    if code_len > bytes.len() {
        return false;
    }

    for i in 0..=(bytes.len() - code_len) {
        if &bytes[i..i + code_len] == code_bytes {
            // Ensure the character before (if any) is not a digit.
            let preceded_by_digit = i > 0 && bytes[i - 1].is_ascii_digit();
            // Ensure the character after (if any) is not a digit.
            let followed_by_digit =
                i + code_len < bytes.len() && bytes[i + code_len].is_ascii_digit();
            if !preceded_by_digit && !followed_by_digit {
                return true;
            }
        }
    }
    false
}

/// Compute a 0–199 ms jitter value from the subsecond part of the system clock.
///
/// Using wall-clock nanoseconds avoids a dependency on the `rand` crate while
/// still producing enough variance to prevent concurrent processes from all
/// waking up at the same millisecond.
fn jitter_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() % 200) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_transient_error ---

    #[test]
    fn transient_detects_timeout() {
        assert!(is_transient_error("error: connection timeout after 30s"));
        assert!(is_transient_error("request timed out"));
    }

    #[test]
    fn transient_detects_connection_refused() {
        assert!(is_transient_error(
            "Os error: connection refused (os error 111)"
        ));
    }

    #[test]
    fn transient_detects_rate_limit_http_codes() {
        assert!(is_transient_error(
            "server returned status 429 Too Many Requests"
        ));
        assert!(is_transient_error("HTTP 503 Service Unavailable"));
        assert!(is_transient_error("upstream error: 502 Bad Gateway"));
        assert!(is_transient_error("gateway timeout: 504"));
    }

    /// #412: status codes must be detected even when not space-prefixed.
    #[test]
    fn transient_detects_unanchored_http_status_codes() {
        // "429: <message>" — colon immediately after the code, no leading space
        assert!(is_transient_error("429: too many requests"));
        assert!(is_transient_error("502: bad gateway"));
        assert!(is_transient_error("503: service unavailable"));
        assert!(is_transient_error("504: gateway timeout"));

        // "Error(429)" — code wrapped in parentheses
        assert!(is_transient_error("Error(429)"));
        assert!(is_transient_error("rpc error(503): upstream unavailable"));

        // "status=429" — code after an equals sign
        assert!(is_transient_error("http status=429"));
    }

    /// #412 + #249: a longer number that merely *contains* a transient code as
    /// a substring must not trigger a retry (false-positive guard).
    #[test]
    fn non_transient_longer_numbers_not_retried() {
        // 14290, 5040, 15030, 25040 — all contain a transient code as a
        // contiguous substring but are not the bare three-digit code itself.
        assert!(!is_transient_error("error code 14290"));
        assert!(!is_transient_error("transaction fee 5040 stroops"));
        assert!(!is_transient_error("ledger 15030 not found"));
        assert!(!is_transient_error("sequence 25040 too old"));
    }

    #[test]
    fn transient_detects_rate_limit_text() {
        assert!(is_transient_error("rate limit exceeded, please slow down"));
        assert!(is_transient_error("too many requests"));
    }

    #[test]
    fn non_transient_contract_errors_not_retried() {
        assert!(!is_transient_error("contract not found: CABC123"));
        assert!(!is_transient_error("invalid argument: agreement_id"));
        assert!(!is_transient_error("error: source account does not exist"));
        assert!(!is_transient_error("authentication failed"));
    }

    #[test]
    fn non_transient_empty_stderr_not_retried() {
        assert!(!is_transient_error(""));
    }

    /// #249: permanent errors that merely *contain* a transient-looking word
    /// (most notably "network") must never trigger a retry.
    #[test]
    fn non_transient_network_config_errors_not_retried() {
        assert!(!is_transient_error(
            "error: network passphrase mismatch: expected 'Test SDF Network ; September 2015'"
        ));
        assert!(!is_transient_error("unknown network 'testnet'"));
        assert!(!is_transient_error(
            "no network configured; run `stellar network add`"
        ));
        assert!(!is_transient_error(
            "network name contains invalid characters"
        ));
        // "deadline" / "temporary" / "unreachable" as bare words in an
        // unrelated message are no longer enough on their own.
        assert!(!is_transient_error(
            "filing deadline for the proposal has passed"
        ));
        assert!(!is_transient_error(
            "temporary directory could not be created"
        ));
    }

    #[test]
    fn transient_detects_network_failure_phrases() {
        assert!(is_transient_error(
            "network error: could not reach RPC endpoint"
        ));
        assert!(is_transient_error(
            "Os error: network is unreachable (os error 101)"
        ));
        assert!(is_transient_error(
            "dns lookup failed: Temporary failure in name resolution"
        ));
        assert!(is_transient_error("504 Gateway Timeout"));
        assert!(is_transient_error("grpc status: deadline exceeded"));
    }

    // --- decode_process_output / decode_bytes ---

    #[test]
    fn decode_bytes_passes_through_valid_utf8() {
        let (s, fell_back) = decode_bytes("héllo — 世界".as_bytes());
        assert_eq!(s, "héllo — 世界");
        assert_eq!(fell_back, None);
    }

    #[test]
    fn decode_bytes_falls_back_to_latin1_on_invalid_utf8() {
        // 0xE9 is "é" in Latin-1 but an incomplete UTF-8 lead byte here.
        let raw = b"caf\xE9 not utf8";
        let (s, fell_back) = decode_bytes(raw);
        assert_eq!(s, "café not utf8");
        assert_eq!(fell_back, Some(3), "first invalid byte is at offset 3");
        // Every original byte is still recoverable from the decoded string.
        assert_eq!(s.chars().count(), raw.len());
    }

    #[test]
    fn decode_bytes_handles_mixed_valid_and_invalid_sequences() {
        // Valid multi-byte UTF-8 ("→", 0xE2 0x86 0x92) followed by a lone 0xFF.
        let raw = b"ok \xE2\x86\x92 then \xFF end";
        let (s, fell_back) = decode_bytes(raw);
        assert_eq!(fell_back, Some(3));
        assert!(s.starts_with("ok "));
        assert!(s.ends_with(" end"));
        assert_eq!(s.chars().count(), raw.len());
    }

    #[test]
    fn decode_process_output_returns_clean_string_for_valid_utf8() {
        assert_eq!(
            decode_process_output("stdout", "all good".as_bytes().to_vec()),
            "all good"
        );
    }

    #[test]
    fn decode_process_output_still_returns_bytes_on_fallback() {
        let out = decode_process_output("stderr", b"bad \xC0\xC0 byte".to_vec());
        assert!(out.contains("bad "));
        assert!(out.contains(" byte"));
    }

    #[test]
    fn hex_preview_formats_and_truncates() {
        assert_eq!(hex_preview(&[0x00, 0x1f, 0xff]), "00 1f ff");
        let long: Vec<u8> = (0..80).map(|_| 0xABu8).collect();
        let preview = hex_preview(&long);
        assert!(preview.contains("(+16 more)"), "got: {preview}");
    }

    // --- jitter_millis ---

    #[test]
    fn jitter_within_bounds() {
        for _ in 0..20 {
            let j = jitter_millis();
            assert!(j < 200, "jitter {j} should be < 200ms");
        }
    }

    // --- STELLAR_RPC_RETRIES parsing ---

    #[test]
    fn retry_count_defaults_to_three() {
        // Temporarily unset the var to test the default.
        std::env::remove_var("STELLAR_RPC_RETRIES");
        let retries: u32 = std::env::var("STELLAR_RPC_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        assert_eq!(retries, 3);
    }

    #[test]
    fn retry_count_reads_from_env() {
        std::env::set_var("STELLAR_RPC_RETRIES", "5");
        let retries: u32 = std::env::var("STELLAR_RPC_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        assert_eq!(retries, 5);
        std::env::remove_var("STELLAR_RPC_RETRIES");
    }

    #[test]
    fn retry_count_falls_back_on_invalid_value() {
        std::env::set_var("STELLAR_RPC_RETRIES", "not_a_number");
        let retries: u32 = std::env::var("STELLAR_RPC_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        assert_eq!(retries, 3);
        std::env::remove_var("STELLAR_RPC_RETRIES");
    }

    // --- invoke_once timeout (#410) ---

    /// Helper: build a minimal Config for timeout tests.
    fn timeout_test_config() -> Config {
        Config {
            rpc_url: "https://soroban-testnet.stellar.org".to_string(),
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            contract_id: "CAABC123".to_string(),
            source_key: "alice".to_string(),
        }
    }

    /// Path to the platform-appropriate hang mock script.
    fn hang_mock_bin() -> String {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| ".".to_string());
        if cfg!(windows) {
            format!("{manifest_dir}/tests/mock_stellar_hang.bat")
        } else {
            format!("{manifest_dir}/tests/mock_stellar_hang.sh")
        }
    }

    /// When the stellar process hangs, invoke_once must return before the test
    /// times out — well within the 1 s timeout we set here.
    #[test]
    fn invoke_once_returns_when_subprocess_hangs() {
        // Point at the hang mock; ensure test mode is active.
        std::env::set_var("TRELLIS_TEST_MODE", "true");
        std::env::set_var("STELLAR_MOCK_BIN", hang_mock_bin());
        // 1-second hard timeout so the test suite stays fast.
        std::env::set_var("STELLAR_INVOKE_TIMEOUT_SECS", "1");

        let start = std::time::Instant::now();
        let out = RpcClient::invoke_once(&timeout_test_config(), "init", &[]);
        let elapsed = start.elapsed();

        // Must return well within 5 s even if there's scheduling jitter.
        assert!(
            elapsed.as_secs() < 5,
            "invoke_once blocked for {}s — timeout did not fire",
            elapsed.as_secs()
        );
        assert!(!out.success, "hanging process must not be treated as success");
        assert!(
            out.stderr.contains("subprocess timed out"),
            "stderr should contain 'subprocess timed out'; got: {}",
            out.stderr
        );

        std::env::remove_var("STELLAR_INVOKE_TIMEOUT_SECS");
        std::env::remove_var("STELLAR_MOCK_BIN");
        std::env::remove_var("TRELLIS_TEST_MODE");
    }

    /// The error message from a timed-out subprocess must be recognised as a
    /// transient error so the retry loop will attempt again.
    #[test]
    fn timeout_error_is_treated_as_transient() {
        let timeout_stderr = format!(
            "subprocess timed out: `stellar` did not respond within 30s. \
             The process has been abandoned. \
             You can raise STELLAR_INVOKE_TIMEOUT_SECS to allow more time."
        );
        assert!(
            is_transient_error(&timeout_stderr),
            "timeout error must be classified as transient so the retry logic fires"
        );
    }

    /// STELLAR_INVOKE_TIMEOUT_SECS=0 must disable the timeout entirely and
    /// let a fast mock finish normally.
    #[test]
    fn timeout_disabled_when_set_to_zero() {
        std::env::set_var("TRELLIS_TEST_MODE", "true");
        // Point at the normal (fast) mock, not the hang mock.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| ".".to_string());
        let fast_mock = if cfg!(windows) {
            format!("{manifest_dir}/tests/mock_stellar.bat")
        } else {
            format!("{manifest_dir}/tests/mock_stellar.sh")
        };
        std::env::set_var("STELLAR_MOCK_BIN", &fast_mock);
        std::env::set_var("STELLAR_INVOKE_TIMEOUT_SECS", "0");

        // `init` on the fast mock exits 0 immediately.
        let out = RpcClient::invoke_once(&timeout_test_config(), "init", &[]);
        assert!(
            out.success,
            "fast mock with timeout disabled should succeed; stderr: {}",
            out.stderr
        );

        std::env::remove_var("STELLAR_INVOKE_TIMEOUT_SECS");
        std::env::remove_var("STELLAR_MOCK_BIN");
        std::env::remove_var("TRELLIS_TEST_MODE");
    }

    fn cfg_with_source(source_key: &str) -> Config {
        Config {
            rpc_url: "https://soroban-testnet.stellar.org".to_string(),
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            contract_id: "CAABC123".to_string(),
            source_key: source_key.to_string(),
        }
    }

    fn a_seed() -> String {
        // 56 chars, `S` + base32 — matches `config::is_secret_seed`.
        format!("S{}", "A".repeat(55))
    }

    #[test]
    fn build_cmd_args_omits_secret_seed_from_argv() {
        let seed = a_seed();
        let (argv, debug) = RpcClient::build_cmd_args(&cfg_with_source(&seed), "init", &[]);
        assert!(
            !argv.iter().any(|a| a == &seed),
            "secret seed must not appear in argv: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "--source"),
            "--source flag must be dropped for a raw seed"
        );
        assert!(!debug.contains(&seed), "seed must not be printed: {debug}");
        assert!(debug.starts_with("STELLAR_SECRET_KEY=<redacted> stellar "));
    }

    #[test]
    fn build_cmd_args_keeps_source_for_identity_name() {
        let (argv, debug) = RpcClient::build_cmd_args(&cfg_with_source("alice"), "init", &[]);
        let src = argv
            .iter()
            .position(|a| a == "--source")
            .expect("--source present");
        assert_eq!(argv[src + 1], "alice");
        assert!(debug.starts_with("stellar contract invoke"));
    }

    // --- native JSON-RPC: getHealth (#467) / getLatestLedger (#468) ---

    fn cfg_for(server: &mockito::Server) -> Config {
        Config {
            rpc_url: server.url(),
            ..cfg_with_source("alice")
        }
    }

    /// Match a POST whose JSON body is exactly the JSON-RPC 2.0 request for
    /// `method` — this is the request-shape assertion.
    fn expect_request(server: &mut mockito::Server, method: &str, body: &str) -> mockito::Mock {
        server
            .mock("POST", "/")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create()
    }

    #[test]
    fn get_health_sends_json_rpc_request_and_parses_result() {
        let mut server = mockito::Server::new();
        let mock = expect_request(
            &mut server,
            "getHealth",
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"healthy","latestLedger":51583040,"oldestLedger":51565761,"ledgerRetentionWindow":17280}}"#,
        );

        let health = RpcClient::get_health(&cfg_for(&server)).expect("healthy response parses");
        mock.assert();
        assert!(health.is_healthy());
        assert_eq!(
            health,
            HealthStatus {
                status: "healthy".to_string(),
                latest_ledger: Some(51583040),
                oldest_ledger: Some(51565761),
                ledger_retention_window: Some(17280),
            }
        );
    }

    #[test]
    fn get_health_accepts_status_only_response_from_older_rpc() {
        let mut server = mockito::Server::new();
        let _mock = expect_request(
            &mut server,
            "getHealth",
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"unhealthy"}}"#,
        );

        let health = RpcClient::get_health(&cfg_for(&server)).expect("parses");
        assert!(!health.is_healthy());
        assert_eq!(health.latest_ledger, None);
    }

    #[test]
    fn get_latest_ledger_sends_json_rpc_request_and_parses_result() {
        let mut server = mockito::Server::new();
        let mock = expect_request(
            &mut server,
            "getLatestLedger",
            r#"{"jsonrpc":"2.0","id":1,"result":{"id":"c73c5eac58a441d4eb733c35253ae85f783e018f7be5ef974258fed067aabb36","protocolVersion":22,"sequence":2539605}}"#,
        );

        let ledger = RpcClient::get_latest_ledger(&cfg_for(&server)).expect("parses");
        mock.assert();
        assert_eq!(
            ledger,
            LatestLedger {
                hash: "c73c5eac58a441d4eb733c35253ae85f783e018f7be5ef974258fed067aabb36"
                    .to_string(),
                protocol_version: 22,
                sequence: 2539605,
            }
        );
    }

    #[test]
    fn native_call_surfaces_json_rpc_error_object() {
        let mut server = mockito::Server::new();
        let _mock = expect_request(
            &mut server,
            "getLatestLedger",
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#,
        );

        let err = RpcClient::get_latest_ledger(&cfg_for(&server)).unwrap_err();
        assert!(
            err.contains("-32601") && err.contains("method not found"),
            "got: {err}"
        );
    }

    #[test]
    fn native_call_rejects_http_error_status() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/")
            .with_status(503)
            .with_body("Service Unavailable")
            .create();

        let err = RpcClient::get_health(&cfg_for(&server)).unwrap_err();
        assert!(err.contains("503"), "got: {err}");
        // The message stays classifiable by the existing retry heuristic.
        assert!(is_transient_error(&err), "got: {err}");
    }

    #[test]
    fn native_call_rejects_malformed_result() {
        let mut server = mockito::Server::new();
        // `sequence` must be a number — a typed parse must not silently
        // accept a wrong-shaped ledger.
        let _mock = expect_request(
            &mut server,
            "getLatestLedger",
            r#"{"jsonrpc":"2.0","id":1,"result":{"id":"ab","protocolVersion":22,"sequence":"x"}}"#,
        );

        let err = RpcClient::get_latest_ledger(&cfg_for(&server)).unwrap_err();
        assert!(err.contains("malformed JSON-RPC response"), "got: {err}");
    }

    #[test]
    fn native_call_reports_unreachable_endpoint() {
        // Nothing listens on port 9 (discard) on loopback in CI.
        let cfg = Config {
            rpc_url: "http://127.0.0.1:9".to_string(),
            ..cfg_with_source("alice")
        };
        let err = RpcClient::get_health(&cfg).unwrap_err();
        assert!(err.starts_with("getHealth: request to"), "got: {err}");
    }

    #[test]
    fn native_preview_shows_endpoint_and_body() {
        let preview = RpcClient::native_preview(&cfg_with_source("alice"), "getHealth");
        assert!(preview.starts_with("POST https://soroban-testnet.stellar.org "));
        assert!(
            preview.contains(r#""method":"getHealth""#),
            "got: {preview}"
        );
    }

    #[test]
    fn preview_never_prints_a_secret_seed() {
        let seed = a_seed();
        let preview = RpcClient::preview(&cfg_with_source(&seed), "init", &[]);
        assert!(
            !preview.contains(&seed),
            "dry-run leaked the seed: {preview}"
        );
        assert!(preview.contains("<redacted>"));
    }

    // --- network passphrase verification ---

    #[test]
    fn extract_json_string_field_reads_passphrase() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"passphrase":"Test SDF Network ; September 2015","protocolVersion":20}}"#;
        assert_eq!(
            extract_json_string_field(json, "passphrase").as_deref(),
            Some("Test SDF Network ; September 2015")
        );
    }

    #[test]
    fn extract_json_string_field_handles_escapes() {
        let json = r#"{"passphrase":"a \"quoted\" value"}"#;
        assert_eq!(
            extract_json_string_field(json, "passphrase").as_deref(),
            Some("a \"quoted\" value")
        );
    }

    #[test]
    fn extract_json_string_field_missing_returns_none() {
        assert_eq!(extract_json_string_field(r#"{"result":{}}"#, "passphrase"), None);
    }

    #[test]
    fn verify_network_passphrase_rejects_unsupported_scheme() {
        let mut cfg = cfg_with_source("alice");
        cfg.rpc_url = "https://soroban-testnet.stellar.org".to_string();
        let err = RpcClient::verify_network_passphrase(&cfg).unwrap_err();
        assert!(err.contains("unsupported RPC URL scheme"), "got: {err}");
    }

    // --- #476: native transaction simulation / resource-fee parsing ---

    /// The `transactionData` from the official `simulateTransaction` docs
    /// example: two read-only keys (contract data + contract code) and one
    /// read-write contract-data key.
    const DOCS_TRANSACTION_DATA_B64: &str = "AAAAAAAAAAIAAAAGAAAAAcwD/nT9D7Dc2LxRdab+2vEUF8B+XoN7mQW21oxPT8ALAAAAFAAAAAEAAAAHy8vNUZ8vyZ2ybPHW0XbSrRtP7gEWsJ6zDzcfY9P8z88AAAABAAAABgAAAAHMA/50/Q+w3Ni8UXWm/trxFBfAfl6De5kFttaMT0/ACwAAABAAAAABAAAAAgAAAA8AAAAHQ291bnRlcgAAAAASAAAAAAAAAAAg4dbAxsGAGICfBG3iT2cKGYQ6hK4sJWzZ6or1C5v6GAAAAAEAHfKyAAAFiAAAAIgAAAAAAAAAAw==";

    /// `ScVal::Map { status: "active", milestone_count: 3 }` as base64 XDR.
    const MAP_RESULT_B64: &str = "AAAAEQAAAAEAAAACAAAADwAAAAZzdGF0dXMAAAAAAA8AAAAGYWN0aXZlAAAAAAAPAAAAD21pbGVzdG9uZV9jb3VudAAAAAADAAAAAw==";

    #[test]
    fn parse_transaction_data_reads_footprint_and_fees() {
        let (footprint, resource_fee, instructions, read_bytes, write_bytes) =
            parse_transaction_data(DOCS_TRANSACTION_DATA_B64).expect("docs example must parse");

        assert_eq!(footprint.read_only.len(), 2);
        assert_eq!(footprint.read_write.len(), 1);
        assert_eq!(resource_fee, 3);
        assert_eq!(instructions, 1_962_674);
        assert_eq!(read_bytes, 1_416);
        assert_eq!(write_bytes, 136);
        // The read-only set is the contract's instance entry plus its WASM.
        assert!(matches!(
            &footprint.read_only[0],
            LedgerKey::ContractData { .. }
        ));
        assert!(matches!(&footprint.read_only[1], LedgerKey::ContractCode { .. }));
        assert!(matches!(
            &footprint.read_write[0],
            LedgerKey::ContractData { .. }
        ));
    }

    #[test]
    fn parse_transaction_data_rejects_bad_base64() {
        let err = parse_transaction_data("not base64!!").unwrap_err();
        assert!(err.contains("base64"), "got: {err}");
    }

    #[test]
    fn decode_scval_json_decodes_a_map() {
        let value = decode_scval_json(MAP_RESULT_B64).expect("map must decode");
        assert_eq!(value["status"], serde_json::json!("active"));
        assert_eq!(value["milestone_count"], serde_json::json!(3));
    }

    // --- #476: mock-server tests for simulate_transaction ---

    /// Spawn a one-shot mock JSON-RPC HTTP server on an ephemeral port and
    /// return its URL plus a join handle. It answers exactly one request with
    /// `status_line` and `body`, then exits (dropping the listener).
    fn spawn_mock_rpc(
        status_line: &'static str,
        body: String,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        let url = format!("http://127.0.0.1:{}/", addr.port());

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept mock request");

            // Drain the request headers so we can honor Content-Length when
            // reading (and discarding) the body.
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            let header_end = loop {
                let n = stream.read(&mut buf).expect("read request");
                if n == 0 {
                    break request.len();
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(pos) = find_subsequence(&request, b"\r\n\r\n") {
                    break pos + 4;
                }
            };

            let content_length = request[..header_end]
                .split(|&b| b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    if name.trim().eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            while request.len() < header_end + content_length {
                let n = stream.read(&mut buf).expect("read request body");
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }

            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write mock response");
            stream.flush().ok();
        });

        (url, handle)
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn cfg_for_url(url: &str) -> Config {
        Config {
            rpc_url: url.to_string(),
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            contract_id: "CAABC123".to_string(),
            source_key: "alice".to_string(),
        }
    }

    #[test]
    fn simulate_transaction_parses_successful_simulation() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionData": DOCS_TRANSACTION_DATA_B64,
                "minResourceFee": "90353",
                "events": ["AAAA"],
                "results": [ { "auth": [], "xdr": MAP_RESULT_B64 } ],
                "latestLedger": 2_552_139u64,
            }
        })
        .to_string();

        let (url, handle) = spawn_mock_rpc("200 OK", body);
        let cfg = cfg_for_url(&url);

        let sim = RpcClient::simulate_transaction(&cfg, "AAAAAgAAAAA=", true)
            .expect("simulation should succeed");

        assert_eq!(sim.min_resource_fee, 90_353);
        assert_eq!(sim.resource_fee, 3);
        assert_eq!(sim.instructions, 1_962_674);
        assert_eq!(sim.read_bytes, 1_416);
        assert_eq!(sim.write_bytes, 136);
        assert_eq!(sim.footprint.read_only.len(), 2);
        assert_eq!(sim.footprint.read_write.len(), 1);
        assert_eq!(sim.events, vec!["AAAA".to_string()]);
        assert_eq!(sim.latest_ledger, Some(2_552_139));
        assert!(sim.result_decode_error.is_none());

        let result = sim.result.expect("decoded result");
        assert_eq!(result["status"], serde_json::json!("active"));
        assert_eq!(result["milestone_count"], serde_json::json!(3));

        handle.join().expect("mock server thread");
    }

    #[test]
    fn simulate_transaction_surfaces_contract_error_distinctly() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "error": "Host function call failed: contract error #7",
                "events": ["ZGlhZw=="],
                "latestLedger": 10,
            }
        })
        .to_string();

        let (url, handle) = spawn_mock_rpc("200 OK", body);
        let cfg = cfg_for_url(&url);

        let err = RpcClient::simulate_transaction(&cfg, "AAAAAgAAAAA=", true)
            .expect_err("a reverted host call must fail the simulation");

        match &err {
            SimulationError::Contract { message, events } => {
                assert!(
                    message.contains("Host function call failed"),
                    "got: {message}"
                );
                assert_eq!(events, &vec!["ZGlhZw==".to_string()]);
            }
            other => panic!("expected Contract error, got {other:?}"),
        }
        // A contract-level failure must be distinguishable from a network one.
        assert!(!matches!(&err, SimulationError::Network(_)));
        assert!(!matches!(&err, SimulationError::Rpc { .. }));
        assert!(err.to_string().contains("contract error"));

        handle.join().expect("mock server thread");
    }

    #[test]
    fn simulate_transaction_reports_jsonrpc_error_separately() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32602, "message": "invalid transaction envelope" }
        })
        .to_string();

        let (url, handle) = spawn_mock_rpc("200 OK", body);
        let cfg = cfg_for_url(&url);

        let err = RpcClient::simulate_transaction(&cfg, "not-xdr", true)
            .expect_err("a JSON-RPC error must fail");

        match &err {
            SimulationError::Rpc { code, message } => {
                assert_eq!(*code, -32602);
                assert!(message.contains("invalid transaction envelope"), "got: {message}");
            }
            other => panic!("expected Rpc error, got {other:?}"),
        }
        assert!(!matches!(&err, SimulationError::Contract { .. }));
        assert!(err.to_string().contains("RPC error"));

        handle.join().expect("mock server thread");
    }

    #[test]
    fn simulate_transaction_reports_http_failure_as_network_error() {
        // 400 is deliberately *not* a transient status, so the retry loop does
        // not kick in and the test stays fast and deterministic.
        let (url, handle) = spawn_mock_rpc("400 Bad Request", String::new());
        let cfg = cfg_for_url(&url);

        let err = RpcClient::simulate_transaction(&cfg, "AAAAAgAAAAA=", true)
            .expect_err("HTTP 400 must fail");

        match err {
            SimulationError::Network(msg) => assert!(msg.contains("400"), "got: {msg}"),
            other => panic!("expected Network error, got {other:?}"),
        }

        handle.join().expect("mock server thread");
    }
}
