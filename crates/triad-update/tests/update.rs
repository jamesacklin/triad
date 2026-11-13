//! The updater against a release served over plain HTTP on localhost. The
//! "binaries" are shell scripts that answer `--version`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use triad_update::{
    Check, Outcome, Replace, Slot, TARGET, check, generate_key, is_slot_binary, load_signing_key,
    publish, replace,
};

const FAR_FUTURE: u64 = 4_000_000_000;
const NOW: u64 = 1_800_000_000;

/// Serves the files of `dir` with GET, an ETag and If-None-Match, which is
/// all the updater asks of a source.
fn serve(dir: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut if_none_match = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("if-none-match:") {
                    if_none_match = Some(v.trim().to_owned());
                }
            }
            let path = request
                .split(' ')
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/');
            let response = match fs::read(dir.join(path)) {
                Ok(body) => {
                    // Content and length identify a file well enough here.
                    let etag = format!(
                        "\"{}-{}\"",
                        body.len(),
                        body.iter()
                            .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(*b as u64))
                    );
                    if if_none_match.as_deref() == Some(&etag) {
                        b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                    } else {
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        out.extend(body);
                        out
                    }
                }
                Err(_) => {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                }
            };
            let _ = stream.write_all(&response);
        }
    });
    url
}

struct Fixture {
    tmp: TempDir,
    url: String,
    keys: Vec<p2panda_core::VerifyingKey>,
    client: reqwest::Client,
}

impl Fixture {
    fn new() -> Fixture {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tmp = TempDir::new().unwrap();
        let (secret, public) = generate_key();
        fs::write(tmp.path().join("key"), secret).unwrap();
        fs::create_dir(tmp.path().join("site")).unwrap();
        fs::write(
            tmp.path().join("factory"),
            "the binary the image shipped with",
        )
        .unwrap();
        let url = serve(tmp.path().join("site"));
        Fixture {
            url,
            keys: triad_update::parse_keys(&public).unwrap(),
            client: reqwest::Client::new(),
            tmp,
        }
    }

    fn site(&self) -> PathBuf {
        self.tmp.path().join("site")
    }

    fn slots(&self) -> PathBuf {
        self.tmp.path().join("slots")
    }

    /// A stand-in binary that prints `triad <printed>` for `--version`.
    fn binary(&self, printed: &str) -> PathBuf {
        let path = self.tmp.path().join(format!("triad-{printed}"));
        fs::write(&path, format!("#!/bin/sh\necho \"triad {printed}\"\n")).unwrap();
        path
    }

    fn release(&self, seq: u64, version: &str, expires: u64) {
        self.release_of(seq, version, expires, &self.binary(version));
    }

    fn release_of(&self, seq: u64, version: &str, expires: u64, binary: &Path) {
        let key = load_signing_key(&self.tmp.path().join("key")).unwrap();
        publish(
            &self.site(),
            &key,
            version,
            seq,
            expires,
            &[(TARGET.to_owned(), binary.to_owned())],
        )
        .unwrap();
    }

    async fn check_as(&self, running: &Path, slots: bool) -> anyhow::Result<Outcome> {
        let dir = self.slots();
        check(Check {
            client: &self.client,
            url: &self.url,
            keys: &self.keys,
            running,
            slots: slots.then_some(dir.as_path()),
            now: NOW,
        })
        .await
    }

    /// A check made by the binary the image shipped with.
    async fn check(&self) -> anyhow::Result<Outcome> {
        self.check_as(&self.tmp.path().join("factory"), true).await
    }

    /// A plain install of version `version`, at `<tmp>/bin/triad`.
    fn installed(&self, version: &str) -> PathBuf {
        let bin = self.tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = bin.join("triad");
        fs::copy(self.binary(version), &path).unwrap();
        path
    }

    async fn replace(&self, running: &Path, version: &str) -> anyhow::Result<Outcome> {
        replace(Replace {
            client: &self.client,
            url: &self.url,
            keys: &self.keys,
            running,
            version,
            now: NOW,
        })
        .await
    }

    fn read(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.slots().join(name)).ok()
    }
}

fn staged(version: &str, slot: Slot) -> Outcome {
    Outcome::Staged {
        version: version.into(),
        slot,
    }
}

#[tokio::test]
async fn a_release_is_installed_into_the_slot_that_is_not_running() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);

    assert_eq!(f.check().await.unwrap(), staged("0.2.0", Slot::A));
    assert_eq!(f.read("current").as_deref(), Some("a"));
    assert_eq!(f.read("pending").as_deref(), Some("factory"));
    assert!(f.read("a").unwrap().contains("triad 0.2.0"));

    // On trial: nothing more happens until the supervisor confirms it.
    assert_eq!(f.check().await.unwrap(), Outcome::Busy);
    fs::remove_file(f.slots().join("pending")).unwrap();

    // Now running from slot a, and the source has nothing newer.
    let a = f.slots().join("a");
    assert_eq!(f.check_as(&a, true).await.unwrap(), Outcome::Current);

    f.release(2, "0.3.0", FAR_FUTURE);
    assert_eq!(
        f.check_as(&a, true).await.unwrap(),
        staged("0.3.0", Slot::B)
    );
    assert_eq!(f.read("current").as_deref(), Some("b"));
    assert_eq!(f.read("pending").as_deref(), Some("a"));
    assert!(
        f.read("a").unwrap().contains("triad 0.2.0"),
        "slot a is kept to go back to"
    );
}

#[tokio::test]
async fn without_a_slot_directory_a_release_is_only_reported() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);
    let factory = f.tmp.path().join("factory");
    assert_eq!(
        f.check_as(&factory, false).await.unwrap(),
        Outcome::Available {
            version: "0.2.0".into()
        }
    );
    assert!(!f.slots().exists());
}

#[tokio::test]
async fn a_binary_that_differs_from_the_manifest_is_not_installed() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);
    // Someone on the path swaps the binary for another of the same size.
    let blob = fs::read_dir(f.site().join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let evil = fs::read_to_string(&blob).unwrap().replace("0.2.0", "6.6.6");
    fs::write(&blob, evil).unwrap();

    let err = f.check().await.unwrap_err();
    assert!(format!("{err:#}").contains("hash"), "{err:#}");
    assert_eq!(f.read("current"), None);
    assert_eq!(f.read("a"), None);
    assert_eq!(f.read("a.partial"), None);
}

#[tokio::test]
async fn a_manifest_signed_by_another_key_is_refused() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);
    let (secret, _) = generate_key();
    fs::write(f.tmp.path().join("key"), secret).unwrap();
    f.release(2, "6.6.6", FAR_FUTURE);

    let err = f.check().await.unwrap_err();
    assert!(format!("{err:#}").contains("not signed"), "{err:#}");
    assert_eq!(f.read("current"), None);
}

#[tokio::test]
async fn an_older_manifest_is_refused_once_a_newer_one_was_seen() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);
    let old_manifest = fs::read(f.site().join("manifest")).unwrap();
    f.release(2, "0.3.0", FAR_FUTURE);
    assert_eq!(f.check().await.unwrap(), staged("0.3.0", Slot::A));
    fs::remove_file(f.slots().join("pending")).unwrap();

    // The source (or someone in front of it) replays the genuine old release.
    fs::write(f.site().join("manifest"), old_manifest).unwrap();
    let err = f.check_as(&f.slots().join("a"), true).await.unwrap_err();
    assert!(format!("{err:#}").contains("older"), "{err:#}");
    assert_eq!(f.read("current").as_deref(), Some("a"));
    assert_eq!(f.read("b"), None);
}

#[tokio::test]
async fn an_expired_manifest_is_refused() {
    let f = Fixture::new();
    f.release(1, "0.2.0", NOW - 1);
    let err = f.check().await.unwrap_err();
    assert!(format!("{err:#}").contains("expired"), "{err:#}");
    assert_eq!(f.read("current"), None);
}

#[tokio::test]
async fn a_release_that_was_rolled_back_is_not_installed_again() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);
    assert_eq!(f.check().await.unwrap(), staged("0.2.0", Slot::A));

    // What the supervisor does when the trial fails.
    fs::remove_file(f.slots().join("current")).unwrap();
    fs::rename(f.slots().join("pending"), f.slots().join("rolled-back")).unwrap();

    let rejected = Outcome::Rejected {
        version: "0.2.0".into(),
    };
    assert_eq!(f.check().await.unwrap(), rejected);
    assert_eq!(f.read("current"), None);
    assert_eq!(f.read("rolled-back"), None);
    // The next check is answered "not modified" and changes nothing.
    assert_eq!(f.check().await.unwrap(), Outcome::Current);
    assert_eq!(f.read("current"), None);

    // A later release is installed as usual.
    f.release(2, "0.2.1", FAR_FUTURE);
    assert_eq!(f.check().await.unwrap(), staged("0.2.1", Slot::A));
}

#[tokio::test]
async fn a_binary_that_is_not_the_version_it_claims_is_not_installed() {
    let f = Fixture::new();
    f.release_of(1, "0.2.0", FAR_FUTURE, &f.binary("0.1.9"));

    let err = f.check().await.unwrap_err();
    assert!(format!("{err:#}").contains("does not run here"), "{err:#}");
    assert_eq!(f.read("current"), None);
    assert_eq!(f.read("a"), None);
    assert_eq!(
        f.check().await.unwrap(),
        Outcome::Current,
        "not downloaded again"
    );
}

/// Everything in the directory of `path` but `path` itself.
fn others(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| Some(n.as_str()) != path.file_name().and_then(|f| f.to_str()))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_plain_install_is_replaced_by_a_newer_release() {
    let f = Fixture::new();
    let triad = f.installed("0.1.0");
    f.release(1, "0.2.0", FAR_FUTURE);

    assert_eq!(
        f.replace(&triad, "0.1.0").await.unwrap(),
        Outcome::Installed {
            version: "0.2.0".into()
        }
    );
    assert!(fs::read_to_string(&triad).unwrap().contains("triad 0.2.0"));
    assert_eq!(
        others(&triad),
        Vec::<String>::new(),
        "nothing left beside it"
    );

    // Running the new binary, there is nothing to do.
    assert_eq!(f.replace(&triad, "0.2.0").await.unwrap(), Outcome::Current);
}

#[tokio::test]
async fn a_plain_install_is_never_taken_back_to_an_older_release() {
    let f = Fixture::new();
    let triad = f.installed("0.3.0");
    f.release(5, "0.2.0", FAR_FUTURE);

    assert_eq!(f.replace(&triad, "0.3.0").await.unwrap(), Outcome::Current);
    assert!(fs::read_to_string(&triad).unwrap().contains("triad 0.3.0"));
}

#[tokio::test]
async fn a_plain_install_keeps_its_binary_when_the_release_does_not_run() {
    let f = Fixture::new();
    let triad = f.installed("0.1.0");
    f.release_of(1, "0.2.0", FAR_FUTURE, &f.binary("0.1.9"));

    let err = f.replace(&triad, "0.1.0").await.unwrap_err();
    assert!(format!("{err:#}").contains("does not run here"), "{err:#}");
    assert!(fs::read_to_string(&triad).unwrap().contains("triad 0.1.0"));
    assert_eq!(
        others(&triad),
        Vec::<String>::new(),
        "the download is removed"
    );
}

#[tokio::test]
async fn a_slot_binary_is_recognised() {
    let f = Fixture::new();
    f.release(1, "0.2.0", FAR_FUTURE);
    assert!(!is_slot_binary(&f.tmp.path().join("factory")));
    f.check().await.unwrap();
    assert!(is_slot_binary(&f.slots().join("a")));
    assert!(!is_slot_binary(&f.installed("0.1.0")));
}
