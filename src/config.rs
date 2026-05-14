//! Configuration loading and validation (§8).
//!
//! Two passes:
//!   1. `${ENV_VAR}` interpolation on the raw YAML text (one regex, no defaults,
//!      no nesting — astonishment risk zero).
//!   2. `serde_yaml` → `RawConfig`, then `Config::try_from(RawConfig)` does
//!      structural validation.
//!
//! "Parse at the boundary, trust inside." After `Config::load` returns Ok,
//! nothing in the program re-checks these invariants.

// Many fields here are loaded and validated in M1 but not consumed by any
// handler yet — they're carried forward for M2 (DB), M3 (OIDC), M5 (proxy).
// Removing the allow as those milestones land is part of their definition of
// done.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, anyhow};
use regex::Regex;
use serde::Deserialize;
use url::Url;

use crate::secret::Secret;

// -- public types -------------------------------------------------------------

/// A validated service id. Matches `^[a-z0-9][a-z0-9-]{0,31}$`.
///
/// Only constructible via the config loader, which guarantees the regex.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServiceId(String);

impl ServiceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How Pietro injects the operator-supplied credential into forwarded
/// requests (§8 `auth:` block).
#[derive(Debug, Clone)]
pub enum ServiceAuth {
    Bearer {
        value: Secret<String>,
    },
    Header {
        header: String,
        value: Secret<String>,
    },
    Query {
        param: String,
        value: Secret<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Service {
    pub id: ServiceId,
    pub display_name: String,
    pub description: Option<String>,
    pub upstream_url: Url,
    pub auth: ServiceAuth,
}

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer_url: Url,
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub allowed_email_domains: Vec<String>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub public_url: Url,
    pub database_path: String,
    pub cookie_key: Secret<String>,
    pub api_key_pepper: Secret<String>,
    pub oidc: OidcConfig,
    pub services: Vec<Service>,
}

impl Config {
    /// Read the file, perform `${VAR}` interpolation, parse, validate.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file: {}", path.display()))?;
        Self::from_yaml_str(&raw)
    }

    /// Parse a config from an already-loaded YAML string. Public so tests and
    /// future callers (e.g. a `pietro check` subcommand) can reuse it.
    pub fn from_yaml_str(text: &str) -> anyhow::Result<Self> {
        let expanded = interpolate_env(text)?;
        let parsed: RawConfig =
            serde_yaml::from_str(&expanded).context("parsing config YAML")?;
        Config::try_from(parsed)
    }
}

// -- env interpolation -------------------------------------------------------

/// Replace every `${VAR}` (uppercase + underscores + digits, must start with
/// letter or underscore) with `std::env::var(VAR)`. Missing vars → error.
///
/// Deliberately tiny: no defaults, no shell quoting, no `$VAR` without braces.
pub(crate) fn interpolate_env(text: &str) -> anyhow::Result<String> {
    // `^[A-Z_]` then `[A-Z0-9_]*` per §8.
    let re = Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").expect("static regex compiles");

    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let m = caps.get(0).expect("regex always has group 0");
        let var = caps.get(1).expect("regex has group 1").as_str();
        let value = std::env::var(var).map_err(|_| {
            anyhow!("config references ${{{var}}} but the environment variable is not set")
        })?;
        out.push_str(&text[last..m.start()]);
        out.push_str(&value);
        last = m.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

// -- raw (untrusted) shape ---------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawConfig {
    listen: String,
    public_url: String,
    database_path: String,
    cookie_key: String,
    api_key_pepper: String,
    oidc: RawOidc,
    services: Vec<RawService>,
}

#[derive(Debug, Deserialize)]
struct RawOidc {
    issuer_url: String,
    client_id: String,
    client_secret: String,
    #[serde(default)]
    allowed_email_domains: Vec<String>,
    #[serde(default = "default_scopes")]
    scopes: Vec<String>,
}

fn default_scopes() -> Vec<String> {
    vec!["profile".into(), "email".into()]
}

#[derive(Debug, Deserialize)]
struct RawService {
    id: String,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    upstream_url: String,
    auth: RawAuth,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RawAuth {
    Bearer {
        value: String,
    },
    Header {
        header: String,
        value: String,
    },
    Query {
        param: String,
        value: String,
    },
}

// -- validation --------------------------------------------------------------

const KEY_MATERIAL_MIN_BYTES: usize = 32;

impl TryFrom<RawConfig> for Config {
    type Error = anyhow::Error;

    fn try_from(raw: RawConfig) -> anyhow::Result<Self> {
        if raw.services.is_empty() {
            return Err(anyhow!("`services` must list at least one service"));
        }

        let public_url = Url::parse(&raw.public_url)
            .with_context(|| format!("invalid public_url: {:?}", raw.public_url))?;

        check_key_material("cookie_key", &raw.cookie_key)?;
        check_key_material("api_key_pepper", &raw.api_key_pepper)?;

        let oidc = OidcConfig {
            issuer_url: Url::parse(&raw.oidc.issuer_url)
                .with_context(|| format!("invalid oidc.issuer_url: {:?}", raw.oidc.issuer_url))?,
            client_id: raw.oidc.client_id,
            client_secret: Secret::new(raw.oidc.client_secret),
            allowed_email_domains: raw.oidc.allowed_email_domains,
            scopes: raw.oidc.scopes,
        };

        let id_re = Regex::new(r"^[a-z0-9][a-z0-9-]{0,31}$").expect("static regex");
        let mut seen = HashSet::new();
        let mut services = Vec::with_capacity(raw.services.len());

        for s in raw.services {
            if !id_re.is_match(&s.id) {
                return Err(anyhow!(
                    "service id {:?} must match ^[a-z0-9][a-z0-9-]{{0,31}}$",
                    s.id
                ));
            }
            if !seen.insert(s.id.clone()) {
                return Err(anyhow!("duplicate service id: {:?}", s.id));
            }
            let upstream_url = Url::parse(&s.upstream_url).with_context(|| {
                format!(
                    "service {:?}: invalid upstream_url {:?}",
                    s.id, s.upstream_url
                )
            })?;
            match upstream_url.scheme() {
                "http" | "https" => {}
                other => {
                    return Err(anyhow!(
                        "service {:?}: upstream_url scheme must be http or https, got {:?}",
                        s.id,
                        other
                    ));
                }
            }
            let auth = match s.auth {
                RawAuth::Bearer { value } => ServiceAuth::Bearer {
                    value: Secret::new(value),
                },
                RawAuth::Header { header, value } => {
                    if header.is_empty() {
                        return Err(anyhow!(
                            "service {:?}: auth.kind=header requires non-empty `header`",
                            s.id
                        ));
                    }
                    ServiceAuth::Header {
                        header,
                        value: Secret::new(value),
                    }
                }
                RawAuth::Query { param, value } => {
                    if param.is_empty() {
                        return Err(anyhow!(
                            "service {:?}: auth.kind=query requires non-empty `param`",
                            s.id
                        ));
                    }
                    ServiceAuth::Query {
                        param,
                        value: Secret::new(value),
                    }
                }
            };
            services.push(Service {
                id: ServiceId(s.id),
                display_name: s.display_name,
                description: s.description,
                upstream_url,
                auth,
            });
        }

        Ok(Config {
            listen: raw.listen,
            public_url,
            database_path: raw.database_path,
            cookie_key: Secret::new(raw.cookie_key),
            api_key_pepper: Secret::new(raw.api_key_pepper),
            oidc,
            services,
        })
    }
}

/// Both `cookie_key` and `api_key_pepper` must decode (hex or base64) to at
/// least 32 bytes. We accept either encoding without forcing the operator to
/// pick one; we also allow >=32 raw ASCII bytes for ergonomic dev configs.
fn check_key_material(name: &str, value: &str) -> anyhow::Result<()> {
    let len = decoded_len(value);
    if len < KEY_MATERIAL_MIN_BYTES {
        return Err(anyhow!(
            "`{name}` must decode to >= {KEY_MATERIAL_MIN_BYTES} bytes (got {len})"
        ));
    }
    Ok(())
}

/// Best-effort decoded length: try hex, then base64, then fall back to byte
/// length. We don't need to actually use the bytes here — just verify there
/// are enough of them.
fn decoded_len(s: &str) -> usize {
    decode_key_material(s).map(|v| v.len()).unwrap_or(s.len())
}

/// Decode a config key-material string (hex, base64, or raw bytes) to bytes.
/// Used at startup to turn `cookie_key` / `api_key_pepper` into the byte
/// arrays the cookie-signing and key-hashing layers actually want.
///
/// Tries hex first (deterministic), then base64 (standard and URL-safe), then
/// falls back to the raw bytes. Returns `None` only if every attempt yields
/// something shorter than the input could plausibly be — never silently
/// truncates.
pub fn decode_key_material(s: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = try_hex(s) {
        return Some(bytes);
    }
    if let Some(bytes) = try_base64(s) {
        return Some(bytes);
    }
    Some(s.as_bytes().to_vec())
}

fn try_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.is_empty() {
        return None;
    }
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    hex::decode(s).ok()
}

fn try_base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    if s.is_empty() {
        return None;
    }
    let ok = s.bytes().all(|b| {
        b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'-' || b == b'_' || b == b'='
    });
    if !ok {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
        .ok()
}

// -- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_replaces_known_vars() {
        unsafe { std::env::set_var("PIETRO_TEST_FOO", "bar") };
        let out = interpolate_env("x=${PIETRO_TEST_FOO} y").unwrap();
        assert_eq!(out, "x=bar y");
    }

    #[test]
    fn interpolation_fails_on_unknown_var() {
        let err = interpolate_env("${PIETRO_DEFINITELY_UNSET_XYZ}").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("PIETRO_DEFINITELY_UNSET_XYZ"), "got: {msg}");
    }

    #[test]
    fn interpolation_ignores_lowercase_and_bare_dollar() {
        // §8: only ${UPPER_SNAKE}. `$VAR` and `${lower}` are passed through.
        let s = "$NOPE ${nope_too} literal";
        assert_eq!(interpolate_env(s).unwrap(), s);
    }

    #[test]
    fn service_id_regex_rejects_uppercase() {
        let yaml = sample_yaml().replace("openai", "OpenAI");
        let raw: RawConfig = serde_yaml::from_str(&yaml).unwrap();
        let err = Config::try_from(raw).unwrap_err();
        assert!(format!("{err}").contains("must match"));
    }

    #[test]
    fn service_id_regex_rejects_dup() {
        // Two services with id "openai".
        let yaml = format!(
            "{}\n  - id: \"openai\"\n    display_name: \"x\"\n    upstream_url: \"http://x\"\n    auth: {{ kind: bearer, value: \"y\" }}\n",
            sample_yaml().trim_end()
        );
        let raw: RawConfig = serde_yaml::from_str(&yaml).unwrap();
        let err = Config::try_from(raw).unwrap_err();
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn accepts_well_formed_config() {
        let raw: RawConfig = serde_yaml::from_str(sample_yaml()).unwrap();
        let cfg = Config::try_from(raw).unwrap();
        assert_eq!(cfg.services.len(), 1);
        assert_eq!(cfg.services[0].id.as_str(), "openai");
    }

    #[test]
    fn decode_key_material_round_trips_hex_and_base64() {
        // 32 zero bytes, encoded three ways. All decode to the same bytes.
        let want = vec![0u8; 32];
        let hex_str = "0".repeat(64);
        let b64_str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert_eq!(decode_key_material(&hex_str).unwrap(), want);
        assert_eq!(decode_key_material(b64_str).unwrap(), want);
        // Raw ASCII falls through: bytes returned as-is.
        assert_eq!(decode_key_material("hello").unwrap(), b"hello".to_vec());
    }

    fn sample_yaml() -> &'static str {
        // 64 hex chars = 32 bytes, satisfies key material check.
        r#"
listen: "0.0.0.0:8080"
public_url: "http://localhost:8080"
database_path: "./pietro.db"
cookie_key: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
api_key_pepper: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
oidc:
  issuer_url: "http://localhost:9000"
  client_id: "pietro"
  client_secret: "shhh"
services:
  - id: "openai"
    display_name: "OpenAI"
    upstream_url: "https://api.openai.com"
    auth:
      kind: bearer
      value: "sk-test"
"#
    }
}
