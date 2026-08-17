//! `--private-net` file: point arti at the VM network's own directory
//! authorities and fallback caches instead of the real Tor network.
//!
//! Line format: `authority <name> <v3ident-hex>` and
//! `fallback <rsa-id-hex> <ed-id-base64> <ip:orport>`. `#` starts a comment.

use arti_client::config::TorClientConfigBuilder;
use arti_client::config::dir;
use tor_llcrypto::pk::ed25519::Ed25519Identity;
use tor_llcrypto::pk::rsa::RsaIdentity;

#[allow(dead_code)]
pub(crate) struct Authority {
    pub(crate) name: String,
    pub(crate) v3ident: String,
}

#[allow(dead_code)]
pub(crate) struct Fallback {
    pub(crate) rsa: String,
    pub(crate) ed: String,
    pub(crate) orport: String,
}

#[allow(dead_code)]
pub(crate) struct PrivateNet {
    pub(crate) authorities: Vec<Authority>,
    pub(crate) fallbacks: Vec<Fallback>,
}

impl PrivateNet {
    #[allow(dead_code)]
    pub(crate) fn parse(text: &str) -> Result<PrivateNet, String> {
        let mut authorities = Vec::new();
        let mut fallbacks = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["authority", name, v3ident] => {
                    if !v3ident.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Err(format!("line {}: v3ident is not hex", n + 1));
                    }
                    authorities.push(Authority {
                        name: (*name).to_owned(),
                        v3ident: (*v3ident).to_owned(),
                    });
                }
                ["fallback", rsa, ed, orport] => fallbacks.push(Fallback {
                    rsa: (*rsa).to_owned(),
                    ed: (*ed).to_owned(),
                    orport: (*orport).to_owned(),
                }),
                _ => return Err(format!("line {}: unrecognized directive", n + 1)),
            }
        }
        if authorities.is_empty() {
            return Err("no authorities in private-net file".into());
        }
        if fallbacks.is_empty() {
            return Err("no fallback caches in private-net file".into());
        }
        Ok(PrivateNet {
            authorities,
            fallbacks,
        })
    }

    /// Apply onto arti's config builder: custom authorities + fallbacks and
    /// testing-network directory tolerances.
    #[allow(dead_code)]
    pub(crate) fn apply(&self, b: &mut TorClientConfigBuilder) -> Result<(), String> {
        let mut authorities = dir::AuthorityContacts::builder();
        for a in &self.authorities {
            // The name is parsed and validated but unused in the apply step.
            // (AuthorityContacts holds parallel vectors of identities, not named authority objects.)
            let rsa_id = RsaIdentity::from_hex(&a.v3ident).ok_or_else(|| {
                format!("failed to parse v3ident {}: invalid hex or length", a.name)
            })?;
            authorities.v3idents().push(rsa_id);
        }

        let mut fbs = Vec::new();
        for f in &self.fallbacks {
            let mut fb = dir::FallbackDir::builder();
            let rsa_id = RsaIdentity::from_hex(&f.rsa)
                .ok_or_else(|| "fallback rsa: invalid hex or length".to_string())?;
            let ed_id = Ed25519Identity::from_base64(&f.ed)
                .ok_or_else(|| "fallback ed: invalid base64 or length".to_string())?;
            let orport_addr = f
                .orport
                .parse()
                .map_err(|e: std::net::AddrParseError| format!("fallback orport: {e}"))?;

            fb.rsa_identity(rsa_id).ed_identity(ed_id);
            fb.orports().push(orport_addr);
            fbs.push(fb);
        }

        let net = b.tor_network();
        *net.authorities() = authorities;
        net.set_fallback_caches(fbs);

        // A freshly-started testing network votes on short consensus
        // lifetimes; be tolerant of clock skew between VMs.
        b.directory_tolerance()
            .pre_valid_tolerance(std::time::Duration::from_secs(300));
        b.directory_tolerance()
            .post_valid_tolerance(std::time::Duration::from_secs(300));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# test net
authority test-da1 27102BC123E7AF1D4741AE047E160C91ADC76B21
authority test-da2 5B5A54A6C2778775E11B7E00A0C7DF562AF9AFE9

fallback 27102BC123E7AF1D4741AE047E160C91ADC76B21 xGYRXQ2b1SDpLoNjKilDNzrqAX2XCEBEyYlVmIGSjTo 192.168.1.11:9001
";

    #[test]
    fn parses_authorities_and_fallbacks() {
        let net = PrivateNet::parse(SAMPLE).unwrap();
        assert_eq!(net.authorities.len(), 2);
        assert_eq!(net.fallbacks.len(), 1);
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(PrivateNet::parse("authority only-two-fields\n").is_err());
        assert!(PrivateNet::parse("unknown x y z\n").is_err());
        assert!(PrivateNet::parse("authority da NOT-HEX\n").is_err());
    }

    /// The parsed net applies onto a TorClientConfigBuilder without error.
    #[test]
    fn applies_to_builder() {
        let net = PrivateNet::parse(SAMPLE).unwrap();
        let mut b = TorClientConfigBuilder::default();
        net.apply(&mut b).unwrap();
    }

    /// The parsed net produces a valid in-memory config that builds successfully.
    /// This test constructs a full config builder with state/cache directories and
    /// calls build() to validate runtime config construction (no network access).
    #[test]
    fn built_config_validates() {
        use std::env;
        use std::process;

        let net = PrivateNet::parse(SAMPLE).unwrap();

        // Create temporary state and cache directories unique to this test run.
        let pid = process::id();
        let temp_base = env::temp_dir();
        let temp_state = temp_base.join(format!("fungi-e2e-test-state-{}", pid));
        let temp_cache = temp_base.join(format!("fungi-e2e-test-cache-{}", pid));

        std::fs::create_dir_all(&temp_state).expect("failed to create temp state dir");
        std::fs::create_dir_all(&temp_cache).expect("failed to create temp cache dir");

        let mut b = TorClientConfigBuilder::from_directories(&temp_state, &temp_cache);
        net.apply(&mut b).unwrap();

        // The build() call validates all config constraints in-memory.
        // This ensures authorities + fallbacks are consistent per arti's validation.
        b.build().expect("private-net config should build");

        // Clean up temporary directories.
        let _ = std::fs::remove_dir_all(&temp_state);
        let _ = std::fs::remove_dir_all(&temp_cache);
    }
}
