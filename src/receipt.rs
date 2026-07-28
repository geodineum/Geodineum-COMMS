//! Signed delivery receipts — COMMS as a receipt PRODUCER.
//!
//! COMMS is the first external producer on the receipt stream. Until now a
//! message's terminal outcome lived only in `writeback_status`: a per-message
//! hash the dashboard overlays, keyed by stream entry id. That hash is a
//! projection — mutable, unsigned, 30-day TTL, and readable only by whoever
//! already knows the key. A receipt is the durable, tamper-evident record of
//! the same outcome, and it is what observers (GeoV, gFlow, gDash) consume.
//!
//! WHY THIS IS A SECOND IMPLEMENTATION, DELIBERATELY
//! The gNode daemon has its own copy of this logic in Rust. This is not an
//! oversight: SB-8.88 commits the ecosystem to further independent producers in
//! Python, C and TypeScript, none of which can share a Rust crate. So the thing
//! that actually keeps producers from drifting cannot be shared code — it has
//! to be shared TEST VECTORS. `tests/receipt-vectors.json` holds them, a
//! byte-identical copy lives in gNode and in Geodineum-pro/CONTRACTS, and both
//! sides assert against it. Drift in a signed format is invisible in normal
//! operation and then surfaces as a signature failure, which reads as tampering
//! rather than as a regression — hence the vectors, not a comment.
//!
//! COMMS signs with its OWN key and its own fingerprint. It runs as
//! `geodineum-comms` and cannot read the daemon's key (0600 gnode:gnode), and
//! it should not: `signer` identifies the producer, so a receipt written by
//! COMMS must be attributable to COMMS.

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// Receipt schema version. Must match the daemon's `RECEIPT_SCHEMA_VERSION`.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// 30 days, matching the daemon's retention and COMMS's own status TTL.
pub const RECEIPT_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// A delivery receipt. Field names and order are wire contract — see
/// `canonical_bytes`.
#[derive(Debug, Clone)]
pub struct Receipt {
    pub correlation_id: String,
    pub command: String,
    pub status: String,
    pub error: Option<String>,
    pub site: String,
    pub node: String,
    pub ts_ms: u64,
    pub body_ref: String,
    pub body_hash: String,
    pub parent_id: Option<String>,
    pub flow_id: Option<String>,
    pub v: u32,
    pub alg: String,
    pub sig: String,
    pub signer: String,
}

impl Receipt {
    /// A receipt for one message-delivery outcome.
    #[allow(clippy::too_many_arguments)]
    pub fn for_delivery(
        correlation_id: impl Into<String>,
        command: impl Into<String>,
        status: impl Into<String>,
        error: Option<String>,
        site: impl Into<String>,
        node: impl Into<String>,
        ts_ms: u64,
        body_ref: impl Into<String>,
        body: &str,
    ) -> Self {
        Receipt {
            correlation_id: correlation_id.into(),
            command: command.into(),
            status: status.into(),
            error,
            site: site.into(),
            node: node.into(),
            ts_ms,
            body_ref: body_ref.into(),
            body_hash: body_hash(body),
            parent_id: None,
            flow_id: None,
            v: RECEIPT_SCHEMA_VERSION,
            alg: String::new(),
            sig: String::new(),
            signer: String::new(),
        }
    }

    /// The exact bytes a signature covers.
    ///
    /// Field ORDER is part of the signature, and an absent optional renders as
    /// the EMPTY STRING rather than an omitted line — a verifier that skips the
    /// line computes different bytes and rejects precisely the healthy
    /// receipts, the ones with no error and no lineage. Byte-identical to the
    /// daemon's `canonical_bytes`; both are pinned to receipt-vectors.json.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&format!("v={}\n", self.v));
        s.push_str(&format!("alg={}\n", self.alg));
        s.push_str(&format!("cid={}\n", self.correlation_id));
        s.push_str(&format!("cmd={}\n", self.command));
        s.push_str(&format!("st={}\n", self.status));
        s.push_str(&format!("e={}\n", self.error.as_deref().unwrap_or("")));
        s.push_str(&format!("ss={}\n", self.site));
        s.push_str(&format!("sn={}\n", self.node));
        s.push_str(&format!("ts={}\n", self.ts_ms));
        s.push_str(&format!("bref={}\n", self.body_ref));
        s.push_str(&format!("bh={}\n", self.body_hash));
        s.push_str(&format!("pid={}\n", self.parent_id.as_deref().unwrap_or("")));
        s.push_str(&format!("fid={}\n", self.flow_id.as_deref().unwrap_or("")));
        s.into_bytes()
    }

    /// Sign in place. `alg` is set BEFORE the canonical bytes are taken so the
    /// algorithm is covered by the signature and cannot be silently downgraded.
    pub fn sign(&mut self, signer: &NodeSigner) -> Result<(), String> {
        self.alg = "ed25519".to_string();
        self.sig = hex(&signer.sign(&self.canonical_bytes()));
        self.signer = signer.signer_id();
        Ok(())
    }

    /// Wire fields for XADD. Short names, matching the daemon's `to_fields`;
    /// empty optionals are omitted from the wire but still signed as empty.
    pub fn to_fields(&self) -> Vec<(String, String)> {
        let mut f = vec![
            ("v".to_string(), self.v.to_string()),
            ("cid".to_string(), self.correlation_id.clone()),
            ("cmd".to_string(), self.command.clone()),
            ("st".to_string(), self.status.clone()),
            ("ss".to_string(), self.site.clone()),
            ("sn".to_string(), self.node.clone()),
            ("ts".to_string(), self.ts_ms.to_string()),
            ("bref".to_string(), self.body_ref.clone()),
            ("bh".to_string(), self.body_hash.clone()),
        ];
        if let Some(e) = &self.error {
            f.push(("e".to_string(), e.clone()));
        }
        if let Some(p) = &self.parent_id {
            f.push(("pid".to_string(), p.clone()));
        }
        if let Some(fl) = &self.flow_id {
            f.push(("fid".to_string(), fl.clone()));
        }
        if !self.alg.is_empty() {
            f.push(("alg".to_string(), self.alg.clone()));
        }
        if !self.sig.is_empty() {
            f.push(("sig".to_string(), self.sig.clone()));
        }
        if !self.signer.is_empty() {
            f.push(("signer".to_string(), self.signer.clone()));
        }
        f
    }
}

/// This producer's signing identity. The private key never leaves the host.
pub struct NodeSigner {
    private_bytes: Vec<u8>,
    public_bytes: Vec<u8>,
}

impl NodeSigner {
    pub fn public_bytes(&self) -> &[u8] {
        &self.public_bytes
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        let seed: [u8; 32] = self.private_bytes[..32]
            .try_into()
            .expect("ed25519 seed is 32 bytes");
        SigningKey::from_bytes(&seed).sign(msg).to_bytes().to_vec()
    }

    /// Short fingerprint: first 8 bytes of sha256(public key), hex. Verifiers
    /// resolve this to the published key, so it must be derived identically
    /// everywhere.
    pub fn signer_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(&self.public_bytes);
        hex(&h.finalize()[..8])
    }
}

/// Load this producer's key, generating and persisting one on first use.
///
/// Format: one line `<alg>:<private_hex>`, matching the daemon's key files so
/// an operator sees one scheme. Written 0600 — a signing key readable beyond
/// its owner is a forgeable identity, and the perm sweep has flattened a 0600
/// key to group-readable before (fixed by an explicit exemption).
pub fn load_or_generate_signer(path: &Path) -> io::Result<NodeSigner> {
    if let Ok(contents) = std::fs::read_to_string(path) {
        if let Some((alg, priv_hex)) = contents.trim().split_once(':') {
            if alg == "ed25519" {
                if let Some(private_bytes) = unhex(priv_hex) {
                    if private_bytes.len() >= 32 {
                        let seed: [u8; 32] = private_bytes[..32].try_into().unwrap();
                        let sk = SigningKey::from_bytes(&seed);
                        return Ok(NodeSigner {
                            private_bytes: seed.to_vec(),
                            public_bytes: sk.verifying_key().to_bytes().to_vec(),
                        });
                    }
                }
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unreadable or non-ed25519 signing key at {}", path.display()),
        ));
    }

    // First use: generate, persist 0600, return.
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let sk = SigningKey::from_bytes(&seed);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("ed25519:{}\n", hex(&seed)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(NodeSigner {
        private_bytes: seed.to_vec(),
        public_bytes: sk.verifying_key().to_bytes().to_vec(),
    })
}

/// Where this producer's signing key lives.
///
/// /var/lib, NOT /etc/geodineum/components. Two reasons, and the first one is
/// not theoretical — the original /etc path failed on the very first live start:
///
///   WARN receipt signer unavailable — COMMS will emit NO receipts this run
///        error=Read-only file system (os error 30)
///
/// The unit runs ProtectSystem=strict and its ReadWritePaths does not include
/// /etc (the daemon's unit does grant itself /etc/geodineum, which is why the
/// same self-generating code works there — an asymmetry that is easy to miss
/// when copying a pattern between two services).
///
/// The fix is not to widen the sandbox. A private signing key is service STATE,
/// not operator CONFIG: /var/lib/geodineum-comms is owned by this service, is
/// already in ReadWritePaths, and keeps the component's config directory
/// non-writable by the component — which is the property the unit comments
/// explicitly want ("root owns so the daemon can't rewrite its own
/// credentials").
pub fn default_signer_path() -> PathBuf {
    PathBuf::from("/var/lib/geodineum-comms/receipt_signing.key")
}

/// Registry of verifier keys: field = signer fingerprint, value = `alg:pubkey_hex`.
pub fn pubkey_registry_key(topology_ns: &str) -> String {
    format!("{{{}}}:gnode:receipt_pubkeys", topology_ns)
}

/// `{site}:gnode:receipts:{env}` — same shape the daemon writes, so observers
/// need no special case for COMMS.
pub fn receipt_stream_key(site: &str, environment: &str) -> String {
    format!("{{{}}}:gnode:receipts:{}", site, environment)
}

pub fn body_hash(body: &str) -> String {
    let mut h = Sha256::new();
    h.update(body.as_bytes());
    hex(&h.finalize())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the independent-implementation decision: this MUST
    /// reproduce the shared vectors byte for byte, or COMMS is writing receipts
    /// nothing else can verify.
    #[test]
    fn matches_shared_receipt_vectors() {
        let raw = include_str!("../tests/receipt-vectors.json");
        // Deliberately parsed with a tiny hand-rolled extractor rather than a
        // serde struct: the test should fail if the FILE changes shape, not
        // silently deserialize a renamed field into a default.
        let grab = |key: &str| -> String {
            let pat = format!("\"{}\"", key);
            let i = raw.find(&pat).unwrap_or_else(|| panic!("missing key {}", key));
            let rest = &raw[i + pat.len()..];
            let c = rest.find(':').expect("no colon");
            let after = &rest[c + 1..];
            let q1 = after.find('"').expect("no opening quote");
            let mut out = String::new();
            let mut chars = after[q1 + 1..].chars();
            while let Some(ch) = chars.next() {
                match ch {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => out.push('\n'),
                        Some(other) => out.push(other),
                        None => break,
                    },
                    _ => out.push(ch),
                }
            }
            out
        };

        let seed_hex = grab("seed_hex");
        let expect_pub = grab("public_hex");
        let expect_signer = grab("signer_id");
        let expect_canon = grab("canonical_bytes_utf8");
        let expect_sig = grab("signature_hex");

        let seed_bytes = unhex(&seed_hex).expect("seed hex");
        let seed: [u8; 32] = seed_bytes[..32].try_into().unwrap();
        let sk = SigningKey::from_bytes(&seed);
        let signer = NodeSigner {
            private_bytes: seed.to_vec(),
            public_bytes: sk.verifying_key().to_bytes().to_vec(),
        };

        assert_eq!(hex(signer.public_bytes()), expect_pub, "public key derivation drifted");
        assert_eq!(signer.signer_id(), expect_signer, "signer fingerprint drifted");

        let mut r = Receipt::for_delivery(
            "req-1", "cmd", "ok", None, "site", "node", 42, "ref", "body",
        );
        r.sign(&signer).unwrap();

        assert_eq!(
            String::from_utf8(r.canonical_bytes()).unwrap(),
            expect_canon,
            "canonical bytes drifted from the shared vectors — every other \
             producer and verifier now disagrees with this one"
        );
        assert_eq!(r.sig, expect_sig, "signature drifted from the shared vectors");
    }

    #[test]
    fn absent_optionals_sign_as_empty_not_omitted() {
        let r = Receipt::for_delivery("c", "cmd", "ok", None, "s", "n", 1, "ref", "b");
        let canon = String::from_utf8(r.canonical_bytes()).unwrap();
        assert!(canon.contains("\ne=\n"), "absent error must sign as an empty line");
        assert!(canon.contains("\npid=\n"));
        assert!(canon.contains("\nfid=\n"));
        // ...and must NOT appear on the wire, where they are simply omitted.
        let keys: Vec<String> = r.to_fields().into_iter().map(|(k, _)| k).collect();
        assert!(!keys.contains(&"e".to_string()));
        assert!(!keys.contains(&"pid".to_string()));
    }

    #[test]
    fn body_hash_is_sha256_of_the_body() {
        assert_eq!(
            body_hash("body"),
            "230d8358dc8e8890b4c58deeb62912ee2f20357ae92a5cc861b98e68fe31acb5"
        );
        assert_ne!(body_hash("body"), body_hash("bodx"));
    }
}

// ─────────────────────────── producer context ──────────────────────────────
// Mirrors the daemon's ReceiptContext: initialised once at startup, then every
// emission site reads it. The builder REFUSES to produce an unsigned receipt —
// no context or a signing failure means no receipt at all, never an
// unverifiable one on a shared stream that observers are told they can trust.

use std::sync::OnceLock;

pub struct ReceiptContext {
    pub signer: NodeSigner,
    pub node_id: String,
    pub environment: String,
}

static RECEIPT_CTX: OnceLock<ReceiptContext> = OnceLock::new();

pub fn init_receipt_context(signer: NodeSigner, node_id: String, environment: String) {
    let _ = RECEIPT_CTX.set(ReceiptContext { signer, node_id, environment });
}

pub fn receipt_context() -> Option<&'static ReceiptContext> {
    RECEIPT_CTX.get()
}

/// Build a SIGNED delivery receipt, or nothing. Returns None when no context is
/// initialised — deliberately, so a misconfigured producer is silent on the
/// stream rather than writing rows nobody can verify.
#[allow(clippy::too_many_arguments)]
pub fn signed_delivery_receipt(
    correlation_id: &str,
    command: &str,
    status: &str,
    error: Option<String>,
    site: &str,
    body_ref: &str,
    body: &str,
    ts_ms: u64,
) -> Option<Receipt> {
    let ctx = receipt_context()?;
    let mut r = Receipt::for_delivery(
        correlation_id, command, status, error, site, &ctx.node_id, ts_ms, body_ref, body,
    );
    r.sign(&ctx.signer).ok()?;
    Some(r)
}

/// Publish this producer's public key so verifiers can resolve its `signer`.
/// Idempotent; safe to call on every start.
pub async fn publish_pubkey(
    conn: &mut redis::aio::MultiplexedConnection,
    topology_ns: &str,
    signer: &NodeSigner,
) -> redis::RedisResult<()> {
    let key = pubkey_registry_key(topology_ns);
    let value = format!("ed25519:{}", hex(signer.public_bytes()));
    redis::cmd("HSET")
        .arg(&key)
        .arg(signer.signer_id())
        .arg(&value)
        .query_async(conn)
        .await
}

/// XADD the receipt to `{site}:gnode:receipts:{env}`, trimmed by age.
pub async fn emit_receipt(
    conn: &mut redis::aio::MultiplexedConnection,
    receipt: &Receipt,
    site: &str,
    environment: &str,
    now_ms: u64,
) -> redis::RedisResult<String> {
    let stream = receipt_stream_key(site, environment);
    let min_id = format!("{}-0", now_ms.saturating_sub(RECEIPT_RETENTION_MS));
    let mut cmd = redis::cmd("XADD");
    cmd.arg(&stream).arg("MINID").arg("~").arg(&min_id).arg("*");
    for (k, v) in receipt.to_fields() {
        cmd.arg(k).arg(v);
    }
    cmd.query_async(conn).await
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
