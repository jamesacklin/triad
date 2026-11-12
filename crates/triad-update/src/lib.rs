//! Over-the-air updates.
//!
//! A release is a directory of static files that any HTTP server can host:
//!
//! ```text
//! <url>/manifest         signed, the only file that changes
//! <url>/blobs/<sha256>   one triad binary per target, never changes
//! ```
//!
//! The manifest is a line of hex (an Ed25519 signature) followed by the JSON
//! it signs. The client trusts the signature, not the transport, so the
//! source can be plain HTTP: the release keys are compiled into the binary,
//! a binary is accepted only if its size and SHA-256 match the signed
//! manifest, a manifest with a lower `seq` than one already seen is refused
//! (no replay of an old release), and one past `expires` is refused (a
//! source that stopped updating is noticed).
//!
//! Binaries are installed into two slots, `a` and `b`, in one directory:
//!
//! ```text
//! a, b          the binaries
//! current       the slot to run: "a" or "b"
//! pending       present while `current` is on trial; holds the slot to
//!               go back to ("a", "b" or "factory")
//! rolled-back   left by the supervisor when a trial failed
//! update.json   what this updater has seen
//! ```
//!
//! [`check`] downloads into the slot that is not running, tests it, and
//! switches `current`. Whatever starts triad (the VM guest's init,
//! `scripts/vm/init.c`) runs the slot named by `current`, deletes `pending`
//! once the new binary has stayed up, and otherwise switches back and
//! renames `pending` to `rolled-back`. A release that was rolled back is
//! not installed again.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use p2panda_core::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The target triple of this build, the key of its entry in a manifest.
pub const TARGET: &str = env!("TRIAD_TARGET");

/// Exit status of `triad update` when it installed a release and triad
/// should be restarted.
pub const STAGED_EXIT: u8 = 10;

const MANIFEST_MAX: usize = 64 * 1024;
const BLOB_MAX: u64 = 256 * 1024 * 1024;
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);
const BLOB_TIMEOUT: Duration = Duration::from_secs(900);

const CURRENT: &str = "current";
const PENDING: &str = "pending";
const ROLLED_BACK: &str = "rolled-back";
const STATE: &str = "update.json";

/// One binary of a release.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    /// Lowercase hex. Also the file name under `blobs/`.
    pub sha256: String,
    pub size: u64,
}

/// The signed description of a release.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Counts releases. A client never goes back to a lower one.
    pub seq: u64,
    /// What the binaries print for `--version`, after `triad `.
    pub version: String,
    /// Unix seconds after which this manifest is no longer accepted.
    pub expires: u64,
    /// Target triple to binary.
    pub targets: BTreeMap<String, Blob>,
}

impl Manifest {
    /// The file to publish: the signature in hex, a newline, the JSON.
    pub fn sign(&self, key: &SigningKey) -> Result<Vec<u8>> {
        self.validate()?;
        let payload = serde_json::to_vec_pretty(self)?;
        let mut out = key.sign(&payload).to_hex().into_bytes();
        out.push(b'\n');
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Parses a published manifest, accepting it only if one of `keys`
    /// signed it. Nothing in it is read before the signature is checked.
    pub fn verify(bytes: &[u8], keys: &[VerifyingKey]) -> Result<Manifest> {
        let split = bytes
            .iter()
            .position(|b| *b == b'\n')
            .context("manifest has no signature line")?;
        let (signature, payload) = (&bytes[..split], &bytes[split + 1..]);
        let signature: Signature = std::str::from_utf8(signature)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .context("manifest signature is not valid hex")?;
        ensure!(
            keys.iter().any(|key| key.verify(payload, &signature)),
            "manifest is not signed by a trusted release key"
        );
        let manifest: Manifest =
            serde_json::from_slice(payload).context("signed manifest is not valid")?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            !self.version.is_empty() && self.version.bytes().all(|b| b.is_ascii_graphic()),
            "manifest version {:?} is not printable ASCII",
            self.version
        );
        for (target, blob) in &self.targets {
            ensure!(
                blob.sha256.len() == 64
                    && blob
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "manifest hash for {target} is not lowercase SHA-256 hex"
            );
            ensure!(
                blob.size > 0 && blob.size <= BLOB_MAX,
                "manifest size for {target} is out of range"
            );
        }
        Ok(())
    }
}

/// Parses release public keys: hex, separated by commas or whitespace.
pub fn parse_keys(text: &str) -> Result<Vec<VerifyingKey>> {
    let keys = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<VerifyingKey>()
                .map_err(|_| anyhow!("{s:?} is not a release public key"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!keys.is_empty(), "no release public key given");
    Ok(keys)
}

/// The release keys this binary trusts: those compiled in (`TRIAD_UPDATE_KEYS`
/// when it was built), or, only in a build with none, `TRIAD_UPDATE_KEYS` in
/// the environment. A released build cannot be pointed at another key.
pub fn trusted_keys() -> Result<Vec<VerifyingKey>> {
    if let Some(built_in) = option_env!("TRIAD_UPDATE_KEYS")
        && !built_in.trim().is_empty()
    {
        return parse_keys(built_in).context("the compiled-in TRIAD_UPDATE_KEYS");
    }
    match std::env::var("TRIAD_UPDATE_KEYS") {
        Ok(keys) => parse_keys(&keys).context("TRIAD_UPDATE_KEYS"),
        Err(_) => bail!(
            "no release key: this build has none compiled in and TRIAD_UPDATE_KEYS is not set"
        ),
    }
}

/// Where releases come from: `flag`, else `TRIAD_UPDATE_URL` in the
/// environment, else the `TRIAD_UPDATE_URL` this binary was built with.
/// Unlike the keys this may always be overridden, because a source is not
/// trusted.
pub fn source_url(flag: Option<String>) -> Result<String> {
    flag.or_else(|| std::env::var("TRIAD_UPDATE_URL").ok())
        .or_else(|| option_env!("TRIAD_UPDATE_URL").map(str::to_owned))
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty())
        .context("no update source: pass --url or set TRIAD_UPDATE_URL")
}

/// A new release signing key: `(secret, public)`, both hex.
pub fn generate_key() -> (String, String) {
    let key = SigningKey::generate();
    (key.to_hex(), key.verifying_key().to_hex())
}

/// Reads a signing key written by `triad release keygen`.
pub fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let bytes = hex::decode(text.trim())
        .ok()
        .context("key file is not hex")?;
    SigningKey::try_from(bytes.as_slice()).map_err(|_| anyhow!("key file is not a signing key"))
}

/// Writes a release into `dir`: each binary under `blobs/`, then the signed
/// manifest, last, so the manifest never names a blob that is not there.
/// Upload in the same order.
pub fn publish(
    dir: &Path,
    key: &SigningKey,
    version: &str,
    seq: u64,
    expires: u64,
    binaries: &[(String, PathBuf)],
) -> Result<Manifest> {
    ensure!(!binaries.is_empty(), "a release needs at least one binary");
    let blobs = dir.join("blobs");
    fs::create_dir_all(&blobs)?;
    let mut targets = BTreeMap::new();
    for (target, path) in binaries {
        let (sha256, size) =
            sha256_file(path).with_context(|| format!("reading {}", path.display()))?;
        let dest = blobs.join(&sha256);
        let tmp = blobs.join(format!("{sha256}.tmp"));
        fs::copy(path, &tmp)?;
        fs::rename(&tmp, &dest)?;
        ensure!(
            targets
                .insert(target.clone(), Blob { sha256, size })
                .is_none(),
            "two binaries given for {target}"
        );
    }
    let manifest = Manifest {
        seq,
        version: version.to_owned(),
        expires,
        targets,
    };
    write_atomic(&dir.join("manifest"), &manifest.sign(key)?)?;
    Ok(manifest)
}

/// One of the two places a binary is installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }

    fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    fn parse(text: &str) -> Option<Slot> {
        match text.trim() {
            "a" => Some(Slot::A),
            "b" => Some(Slot::B),
            _ => None,
        }
    }
}

/// What this updater has seen, kept in `update.json`.
#[derive(Serialize, Deserialize, Default)]
struct State {
    /// The highest manifest `seq` accepted.
    seq: u64,
    /// The source's ETag for the last manifest dealt with in full.
    etag: Option<String>,
    /// The hash of the binary last put on trial.
    staged_sha: Option<String>,
    /// A binary that failed its test or was rolled back: not to be installed.
    bad_sha: Option<String>,
}

impl State {
    fn load(dir: &Path) -> Result<State> {
        match fs::read(dir.join(STATE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("update.json is corrupt"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).context("reading update.json"),
        }
    }

    fn save(&self, dir: &Path) -> Result<()> {
        write_atomic(&dir.join(STATE), &serde_json::to_vec_pretty(self)?)
    }
}

/// What one [`check`] needs.
pub struct Check<'a> {
    pub client: &'a reqwest::Client,
    /// The source, without a trailing slash.
    pub url: &'a str,
    pub keys: &'a [VerifyingKey],
    /// The binary in use, compared with the release by hash.
    pub running: &'a Path,
    /// The slot directory to install into. Without one, only look.
    pub slots: Option<&'a Path>,
    /// Unix seconds, for `expires`.
    pub now: u64,
}

/// The result of a [`check`].
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The running binary is the release.
    Current,
    /// A newer release exists; nothing was installed (no slot directory).
    Available { version: String },
    /// The release was installed and `current` switched to it. Restart triad.
    Staged { version: String, slot: Slot },
    /// The release is one that failed here before, and was left alone.
    Rejected { version: String },
    /// An earlier update is still on trial.
    Busy,
}

/// Asks the source for its release once and, given a slot directory,
/// installs it if it is not what is running.
pub async fn check(c: Check<'_>) -> Result<Outcome> {
    let mut state = State::default();
    if let Some(dir) = c.slots {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        if dir.join(PENDING).exists() {
            return Ok(Outcome::Busy);
        }
        state = State::load(dir)?;
        if dir.join(ROLLED_BACK).exists() {
            if let Some(sha) = state.staged_sha.take() {
                state.bad_sha = Some(sha);
            }
            // Fetch the manifest again, so this check reports the rejection.
            state.etag = None;
            state.save(dir)?;
            fs::remove_file(dir.join(ROLLED_BACK))?;
        }
    }

    let mut request = c
        .client
        .get(format!("{}/manifest", c.url))
        .timeout(MANIFEST_TIMEOUT);
    if let Some(etag) = &state.etag {
        request = request.header("If-None-Match", etag);
    }
    let mut response = request.send().await.context("fetching the manifest")?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Outcome::Current);
    }
    ensure!(
        response.status().is_success(),
        "fetching the manifest: HTTP {}",
        response.status()
    );
    let etag = response
        .headers()
        .get("ETag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context("reading the manifest")? {
        ensure!(
            bytes.len() + chunk.len() <= MANIFEST_MAX,
            "manifest is too large"
        );
        bytes.extend_from_slice(&chunk);
    }

    let manifest = Manifest::verify(&bytes, c.keys)?;
    ensure!(
        manifest.seq >= state.seq,
        "manifest is release {}, older than release {} already seen; refusing it",
        manifest.seq,
        state.seq
    );
    ensure!(
        c.now < manifest.expires,
        "manifest expired; the update source is stale"
    );
    let blob = manifest
        .targets
        .get(TARGET)
        .with_context(|| format!("release {} has no build for {TARGET}", manifest.version))?;
    let version = manifest.version.clone();

    // From here the manifest is dealt with in full unless installing fails,
    // so the next check may be answered with "not modified".
    state.seq = manifest.seq;
    let settled = |state: &mut State| -> Result<()> {
        state.etag = etag.clone();
        match c.slots {
            Some(dir) => state.save(dir),
            None => Ok(()),
        }
    };

    let (running_sha, _) =
        sha256_file(c.running).with_context(|| format!("reading {}", c.running.display()))?;
    if running_sha == blob.sha256 {
        settled(&mut state)?;
        return Ok(Outcome::Current);
    }
    if state.bad_sha.as_deref() == Some(&blob.sha256) {
        settled(&mut state)?;
        return Ok(Outcome::Rejected { version });
    }
    let Some(dir) = c.slots else {
        return Ok(Outcome::Available { version });
    };
    state.save(dir)?;

    let live = fs::read_to_string(dir.join(CURRENT))
        .ok()
        .and_then(|s| Slot::parse(&s));
    let slot = live.map_or(Slot::A, Slot::other);
    let partial = dir.join(format!("{}.partial", slot.name()));
    let fetched = download(
        c.client,
        &format!("{}/blobs/{}", c.url, blob.sha256),
        &partial,
        blob,
    )
    .await;
    if let Err(e) = fetched {
        let _ = fs::remove_file(&partial);
        return Err(e.context(format!("downloading release {version}")));
    }
    if let Err(e) = self_test(&partial, &version) {
        let _ = fs::remove_file(&partial);
        state.bad_sha = Some(blob.sha256.clone());
        settled(&mut state)?;
        return Err(e.context(format!("release {version} does not run here")));
    }

    fs::rename(&partial, dir.join(slot.name()))?;
    state.staged_sha = Some(blob.sha256.clone());
    settled(&mut state)?;
    // `pending` before `current`: a crash in between leaves the old binary
    // on a trial it will pass.
    write_atomic(
        &dir.join(PENDING),
        live.map_or("factory", Slot::name).as_bytes(),
    )?;
    write_atomic(&dir.join(CURRENT), slot.name().as_bytes())?;
    sync_dir(dir);
    Ok(Outcome::Staged { version, slot })
}

/// Streams `url` into `dest`, never holding more than a chunk in memory, and
/// accepts it only if it is exactly the blob the manifest describes.
async fn download(client: &reqwest::Client, url: &str, dest: &Path, blob: &Blob) -> Result<()> {
    let mut response = client.get(url).timeout(BLOB_TIMEOUT).send().await?;
    ensure!(response.status().is_success(), "HTTP {}", response.status());
    let mut file =
        fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = response.chunk().await? {
        size += chunk.len() as u64;
        ensure!(size <= blob.size, "binary is larger than the manifest says");
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    ensure!(
        size == blob.size,
        "binary is shorter than the manifest says"
    );
    ensure!(
        hex::encode(hasher.finalize()) == blob.sha256,
        "binary does not match the manifest's hash"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    file.sync_all()?;
    Ok(())
}

/// Runs the downloaded binary once: it must start on this machine and be
/// the version the manifest names.
fn self_test(binary: &Path, version: &str) -> Result<()> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .context("running it with --version")?;
    ensure!(
        output.status.success(),
        "--version exited with {}",
        output.status
    );
    let printed = String::from_utf8_lossy(&output.stdout);
    ensure!(
        printed.trim() == format!("triad {version}"),
        "--version printed {:?}",
        printed.trim()
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), size))
}

/// Replaces `path` in one step, so a reader sees the old or the new content.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            seq: 3,
            version: "0.2.0".into(),
            expires: 2_000_000_000,
            targets: BTreeMap::from([(
                TARGET.to_owned(),
                Blob {
                    sha256: "ab".repeat(32),
                    size: 10,
                },
            )]),
        }
    }

    #[test]
    fn a_signed_manifest_verifies_only_with_its_key_and_unchanged() {
        let key = SigningKey::generate();
        let other = SigningKey::generate();
        let signed = manifest().sign(&key).unwrap();
        assert_eq!(
            Manifest::verify(&signed, &[other.verifying_key(), key.verifying_key()]).unwrap(),
            manifest()
        );
        assert!(Manifest::verify(&signed, &[other.verifying_key()]).is_err());

        let text = String::from_utf8(signed).unwrap();
        let changed = text.replace("\"seq\": 3", "\"seq\": 4");
        assert_ne!(text, changed);
        assert!(Manifest::verify(changed.as_bytes(), &[key.verifying_key()]).is_err());
    }

    #[test]
    fn a_hash_that_could_leave_the_blob_directory_is_refused() {
        let key = SigningKey::generate();
        let mut bad = manifest();
        bad.targets.get_mut(TARGET).unwrap().sha256 = format!("../{}", "a".repeat(61));
        assert!(bad.sign(&key).is_err());
    }

    #[test]
    fn keys_parse_from_a_list() {
        let (_, a) = generate_key();
        let (_, b) = generate_key();
        assert_eq!(parse_keys(&format!("{a}, {b}\n")).unwrap().len(), 2);
        assert!(parse_keys("").is_err());
        assert!(parse_keys("nothex").is_err());
    }
}
