//! One verifier for every release file the app hands over: the "Download"
//! button (file saved for the user to open) and "Update Now" (file swapped
//! in by gpui-updater-core). Both go through [`ReleaseProof::verify_file`]
//! before the file leaves the private temp directory it was downloaded to.
//!
//! What "valid" means, for a release `X.Y.Z` and its asset `name`:
//! 1. `SHA256SUMS` is signed (`SHA256SUMS.minisig`) by the key in
//!    `release-key.pub`, and the signature's trusted comment says
//!    `version:X.Y.Z`;
//! 2. the file's SHA-256 equals the line for `name` in that list;
//! 3. `name.minisig` is a valid signature of the file by the same key, and
//!    its trusted comment also says `version:X.Y.Z` (so an older, validly
//!    signed file cannot be served as a newer one).
//!
//! A release missing any of these (unsigned, no checksum line) is refused.
//! The Release workflow has produced exactly this since signing was
//! introduced, so updaters already in the field accept the same releases.

use std::path::{Path, PathBuf};

use gpui_updater_core::verify;

/// Name of the checksum list in every release.
pub const SUMS_NAME: &str = "SHA256SUMS";

/// Name of a file's detached minisign signature.
pub fn signature_name(asset: &str) -> String {
    format!("{asset}.minisig")
}

/// Base64 minisign public key (last non-empty line of `release-key.pub`).
pub fn release_public_key() -> &'static str {
    public_key_line(include_str!("../../../release-key.pub"))
}

fn public_key_line(file: &str) -> &str {
    file.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// The checksum and signature one asset must match, taken from a release
/// whose `SHA256SUMS` signature already checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseProof {
    pub asset: String,
    pub version: String,
    pub sha256: String,
    pub signature: String,
}

impl ReleaseProof {
    /// Check the signed checksum list and pick `asset`'s line from it.
    /// `sums` / `sums_sig` are the bodies of `SHA256SUMS` and its
    /// `.minisig`, `asset_sig` the body of `<asset>.minisig`.
    pub fn new(
        asset: &str,
        version: &str,
        sums: &str,
        sums_sig: &str,
        asset_sig: &str,
        public_key: &str,
    ) -> Result<Self, String> {
        let comment = verify::verify_minisign_bytes(sums.as_bytes(), &[public_key], sums_sig)
            .map_err(|e| format!("{SUMS_NAME} signature is not valid: {e}"))?;
        check_signed_version(&comment, version, SUMS_NAME)?;
        let sha256 = ora_core::update::sha256_from_sums(sums, asset)
            .ok_or_else(|| format!("{SUMS_NAME} has no checksum for {asset}"))?;
        if asset_sig.trim().is_empty() {
            return Err(format!("{asset} is not signed"));
        }
        Ok(Self {
            asset: asset.to_string(),
            version: version.to_string(),
            sha256,
            signature: asset_sig.to_string(),
        })
    }

    /// Check the downloaded file: checksum first, then signature and the
    /// version it was signed for.
    pub fn verify_file(&self, path: &Path, public_key: &str) -> Result<(), String> {
        verify::verify_sha256(path, &self.sha256)
            .map_err(|e| format!("{} failed the checksum: {e}", self.asset))?;
        let comment = verify::verify_minisign_file(path, &[public_key], &self.signature)
            .map_err(|e| format!("{} signature is not valid: {e}", self.asset))?;
        check_signed_version(&comment, &self.version, &self.asset)
    }

    /// The Download button's hand-over: verify `downloaded` (still in its
    /// private temp dir), then copy it to `dir` under its release name,
    /// first as `<name>.part`, checked against the checksum again, then
    /// renamed. Nothing appears under the final name unless it verified.
    pub fn verify_and_deliver(
        &self,
        downloaded: &Path,
        dir: &Path,
        public_key: &str,
    ) -> Result<PathBuf, String> {
        self.verify_file(downloaded, public_key)?;
        let (proof, verified) = (self, downloaded);
        let target = dir.join(&proof.asset);
        let part = dir.join(format!("{}.part", proof.asset));
        let result = (|| {
            std::fs::copy(verified, &part).map_err(|e| e.to_string())?;
            // The temp copy is owner-only; the delivered file gets normal
            // permissions (executable for the bare Linux binary).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = if proof.asset.ends_with("linux-x86_64") {
                    0o755
                } else {
                    0o644
                };
                std::fs::set_permissions(&part, std::fs::Permissions::from_mode(mode))
                    .map_err(|e| e.to_string())?;
            }
            verify::verify_sha256(&part, &proof.sha256)
                .map_err(|e| format!("{} changed while saving: {e}", proof.asset))?;
            std::fs::rename(&part, &target).map_err(|e| e.to_string())
        })();
        if let Err(err) = result {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
        Ok(target)
    }
}

/// The trusted comment must carry `version:<version>` exactly.
fn check_signed_version(comment: &str, version: &str, what: &str) -> Result<(), String> {
    match verify::signed_version(comment) {
        Some(signed) if signed.trim_start_matches('v') == version.trim_start_matches('v') => Ok(()),
        Some(signed) => Err(format!(
            "{what} is signed for version {signed}, expected {version}"
        )),
        None => Err(format!("{what} signature names no version")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real files from the v0.1.35 release, signed by John's release key.
    const SUMS: &str = include_str!("../tests/fixtures/release-0.1.35/SHA256SUMS");
    const SUMS_SIG: &str = include_str!("../tests/fixtures/release-0.1.35/SHA256SUMS.minisig");
    const DMG: &str = "Kilat-0.1.35-macos-arm64.dmg";
    const DMG_SIG: &str =
        include_str!("../tests/fixtures/release-0.1.35/Kilat-0.1.35-macos-arm64.dmg.minisig");

    fn key() -> &'static str {
        release_public_key()
    }

    fn write(dir: &tempfile::TempDir, name: &str, data: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn key_comes_from_release_key_pub() {
        assert_eq!(
            key(),
            "RWSd38OyrpNl6/nIAtXkpkaV+Yg+mmnKJBFXMVYZVosPNOR1B8ZzeuPX"
        );
        assert_eq!(public_key_line("untrusted comment: x\nKEY\n\n"), "KEY");
    }

    #[test]
    fn accepts_a_real_signed_release() {
        let proof = ReleaseProof::new(DMG, "0.1.35", SUMS, SUMS_SIG, DMG_SIG, key()).unwrap();
        assert_eq!(
            proof.sha256,
            "b15099241e150f6aaa97c2fcd3ff2cd0d39c9269755825174ce41ef065301843"
        );
        // The signed list itself verifies as a file too (same code path as
        // a downloaded asset).
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, SUMS_NAME, SUMS.as_bytes());
        let sums_proof = ReleaseProof {
            asset: SUMS_NAME.into(),
            version: "0.1.35".into(),
            sha256: verify::sha256_hex(&path).unwrap(),
            signature: SUMS_SIG.into(),
        };
        sums_proof.verify_file(&path, key()).unwrap();
    }

    #[test]
    fn rejects_a_changed_checksum_list() {
        let tampered = SUMS.replacen("b15099", "000000", 1);
        let err =
            ReleaseProof::new(DMG, "0.1.35", &tampered, SUMS_SIG, DMG_SIG, key()).unwrap_err();
        assert!(err.contains("signature is not valid"), "{err}");
    }

    #[test]
    fn rejects_unsigned_or_unknown_assets() {
        let err = ReleaseProof::new(DMG, "0.1.35", SUMS, SUMS_SIG, "  ", key()).unwrap_err();
        assert!(err.contains("not signed"), "{err}");
        let err = ReleaseProof::new(DMG, "0.1.35", SUMS, "", DMG_SIG, key()).unwrap_err();
        assert!(err.contains("SHA256SUMS signature"), "{err}");
        let err = ReleaseProof::new("Kilat-evil.dmg", "0.1.35", SUMS, SUMS_SIG, DMG_SIG, key())
            .unwrap_err();
        assert!(err.contains("no checksum"), "{err}");
    }

    #[test]
    fn rejects_another_key_and_another_version() {
        // A valid minisign key that is not ours.
        let other = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let err = ReleaseProof::new(DMG, "0.1.35", SUMS, SUMS_SIG, DMG_SIG, other).unwrap_err();
        assert!(err.contains("signature is not valid"), "{err}");
        // Signed for 0.1.35 but offered as 0.1.36: replay of an old release.
        let err = ReleaseProof::new(DMG, "0.1.36", SUMS, SUMS_SIG, DMG_SIG, key()).unwrap_err();
        assert!(err.contains("signed for version 0.1.35"), "{err}");
    }

    #[test]
    fn rejects_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, SUMS_NAME, SUMS.as_bytes());
        let good = ReleaseProof {
            asset: SUMS_NAME.into(),
            version: "0.1.35".into(),
            sha256: verify::sha256_hex(&path).unwrap(),
            signature: SUMS_SIG.into(),
        };
        // Same bytes but one flipped: checksum fails first.
        let mut bytes = SUMS.as_bytes().to_vec();
        bytes[0] ^= 1;
        let changed = write(&dir, "changed", &bytes);
        let err = good.verify_file(&changed, key()).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        // Checksum updated to match the changed file: the signature still
        // catches it.
        let forged = ReleaseProof {
            sha256: verify::sha256_hex(&changed).unwrap(),
            ..good.clone()
        };
        let err = forged.verify_file(&changed, key()).unwrap_err();
        assert!(err.contains("signature is not valid"), "{err}");
        // A signature of a different file.
        let swapped = ReleaseProof {
            signature: DMG_SIG.into(),
            ..good
        };
        assert!(swapped.verify_file(&path, key()).is_err());
    }

    fn sums_proof(path: &Path) -> ReleaseProof {
        ReleaseProof {
            asset: SUMS_NAME.into(),
            version: "0.1.35".into(),
            sha256: verify::sha256_hex(path).unwrap(),
            signature: SUMS_SIG.into(),
        }
    }

    #[test]
    fn delivers_only_verified_files() {
        let staging = tempfile::tempdir().unwrap();
        let downloads = tempfile::tempdir().unwrap();
        let good = write(&staging, SUMS_NAME, SUMS.as_bytes());
        let proof = sums_proof(&good);
        let saved = proof
            .verify_and_deliver(&good, downloads.path(), key())
            .unwrap();
        assert_eq!(saved, downloads.path().join(SUMS_NAME));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), SUMS);
        assert!(!downloads.path().join("SHA256SUMS.part").exists());

        // A changed download leaves nothing in Downloads, not even a .part.
        let empty = tempfile::tempdir().unwrap();
        let bad = write(&staging, "bad", format!("{SUMS}x").as_bytes());
        assert!(proof.verify_and_deliver(&bad, empty.path(), key()).is_err());
        assert_eq!(std::fs::read_dir(empty.path()).unwrap().count(), 0);
    }

    #[test]
    fn version_must_be_named_and_equal() {
        assert!(check_signed_version("version:0.1.35", "0.1.35", "x").is_ok());
        assert!(check_signed_version("version:v0.1.35", "0.1.35", "x").is_ok());
        assert!(check_signed_version("timestamp:1 file:x", "0.1.35", "x").is_err());
        assert!(check_signed_version("version:0.1.3", "0.1.35", "x").is_err());
    }
}
