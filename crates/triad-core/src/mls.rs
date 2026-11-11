//! OpenMLS wrapper. No OpenMLS type appears in this module's public signatures:
//! everything crosses the boundary as bytes or node IDs.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use openmls::prelude::{tls_codec::Deserialize as _, *};
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::RustCrypto;
use openmls_sqlite_storage::{Codec, SqliteStorageProvider};
use rusqlite::{Connection, OptionalExtension};

use crate::identity::NodeId;

pub const MLS_FILE: &str = "mls.sqlite";

const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
const MAX_PAST_EPOCHS: usize = 2;

/// `serde_json` codec for the SQLite storage provider.
#[derive(Default)]
pub struct JsonCodec;

impl Codec for JsonCodec {
    type Error = serde_json::Error;

    fn to_vec<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(value)
    }

    fn from_slice<T: serde::de::DeserializeOwned>(slice: &[u8]) -> Result<T, Self::Error> {
        serde_json::from_slice(slice)
    }
}

type Storage = SqliteStorageProvider<JsonCodec, Connection>;

struct Provider {
    crypto: RustCrypto,
    storage: Storage,
}

impl OpenMlsProvider for Provider {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = Storage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

/// Result of adding a member: the commit for existing members and the Welcome for the joiner.
pub struct AddResult {
    pub joiner: NodeId,
    pub commit: Vec<u8>,
    pub welcome: Vec<u8>,
    /// Number of members before the add.
    pub members_before: usize,
    pub epoch: u64,
}

/// A decrypted application message.
pub struct Decrypted {
    pub sender: NodeId,
    pub plaintext: Vec<u8>,
}

/// A parsed MLS object failed an application acceptance rule.
#[derive(Debug)]
pub struct RejectedMessage(pub &'static str);

impl std::fmt::Display for RejectedMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for RejectedMessage {}

/// Why a decrypt failed: the message predates what we can read, or something else.
#[derive(Debug)]
pub enum DecryptError {
    /// The message's epoch is older than what this member holds keys for.
    TooOld { message_epoch: u64 },
    /// Any other failure (consumed key, bad ciphertext, wrong group, ...).
    Other(String),
}

impl std::fmt::Display for DecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecryptError::TooOld { message_epoch } => {
                write!(f, "message from epoch {message_epoch} predates our keys")
            }
            DecryptError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DecryptError {}

pub struct Mls {
    provider: Provider,
    signer: SignatureKeyPair,
    identity: NodeId,
    group: Option<MlsGroup>,
}

impl Mls {
    /// Opens `mls.sqlite` in the data directory.
    pub fn open_dir(data_dir: &Path, identity: NodeId) -> Result<Self> {
        Self::open(&data_dir.join(MLS_FILE), identity)
    }

    /// Opens (or creates) the MLS store and its signature key.
    pub fn open(path: &Path, identity: NodeId) -> Result<Self> {
        // Our own small table remembers which signature key is ours.
        let meta_conn = Connection::open(path)?;
        meta_conn.busy_timeout(std::time::Duration::from_secs(5))?;
        meta_conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS triad_signer (id INTEGER PRIMARY KEY CHECK (id = 1),
                                                      public BLOB NOT NULL);",
        )?;
        let stored_public: Option<Vec<u8>> = meta_conn
            .query_row("SELECT public FROM triad_signer WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?;

        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        {
            let mut migrator = SqliteStorageProvider::<JsonCodec, &mut Connection>::new(&mut conn);
            migrator
                .run_migrations()
                .map_err(|e| anyhow!("mls.sqlite migrations: {e}"))?;
        }
        let provider = Provider {
            crypto: RustCrypto::default(),
            storage: SqliteStorageProvider::new(conn),
        };

        let scheme = CIPHERSUITE.signature_algorithm();
        let signer = match stored_public
            .and_then(|public| SignatureKeyPair::read(provider.storage(), &public, scheme))
        {
            Some(signer) => signer,
            None => {
                let signer = SignatureKeyPair::new(scheme)
                    .map_err(|e| anyhow!("generating MLS signature key: {e:?}"))?;
                signer
                    .store(provider.storage())
                    .context("storing MLS signature key")?;
                meta_conn.execute(
                    "INSERT OR REPLACE INTO triad_signer (id, public) VALUES (1, ?1)",
                    [signer.public()],
                )?;
                signer
            }
        };

        Ok(Mls {
            provider,
            signer,
            identity,
            group: None,
        })
    }

    fn credential_with_key(&self) -> CredentialWithKey {
        CredentialWithKey {
            credential: BasicCredential::new(self.identity.to_vec()).into(),
            signature_key: self.signer.to_public_vec().into(),
        }
    }

    fn group(&self) -> Result<&MlsGroup> {
        self.group
            .as_ref()
            .ok_or_else(|| anyhow!("no MLS group loaded"))
    }

    fn join_config() -> MlsGroupJoinConfig {
        MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .max_past_epochs(MAX_PAST_EPOCHS)
            .build()
    }

    /// Creates a new group with the given ID and this member as the only member.
    pub fn create_group(&mut self, group_id: &[u8]) -> Result<()> {
        if self.group.is_some() {
            bail!("a group is already loaded");
        }
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .max_past_epochs(MAX_PAST_EPOCHS)
            .build();
        let group = MlsGroup::new_with_group_id(
            &self.provider,
            &self.signer,
            &config,
            GroupId::from_slice(group_id),
            self.credential_with_key(),
        )
        .map_err(|e| anyhow!("creating MLS group: {e}"))?;
        self.group = Some(group);
        Ok(())
    }

    /// Loads a stored group. Returns false if none is stored under this ID.
    pub fn load_group(&mut self, group_id: &[u8]) -> Result<bool> {
        let group = MlsGroup::load(self.provider.storage(), &GroupId::from_slice(group_id))
            .context("loading MLS group")?;
        let found = group.is_some();
        self.group = group;
        Ok(found)
    }

    pub fn has_group(&self) -> bool {
        self.group.is_some()
    }

    /// Generates a fresh KeyPackage (private keys stay in `mls.sqlite`) and
    /// returns it TLS-serialized as an MLS message.
    pub fn new_key_package(&self) -> Result<Vec<u8>> {
        let bundle = KeyPackage::builder()
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential_with_key(),
            )
            .map_err(|e| anyhow!("building KeyPackage: {e}"))?;
        let msg = MlsMessageOut::from(bundle.key_package().clone());
        Ok(msg.to_bytes()?)
    }

    fn parse_key_package(&self, bytes: &[u8]) -> Result<KeyPackage> {
        let msg = MlsMessageIn::tls_deserialize_exact(bytes).context("KeyPackage is not TLS")?;
        let MlsMessageBodyIn::KeyPackage(kp_in) = msg.extract() else {
            bail!("message is not a KeyPackage");
        };
        let kp = kp_in
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| anyhow!("KeyPackage does not validate: {e}"))?;
        if kp.ciphersuite() != CIPHERSUITE {
            bail!("KeyPackage uses ciphersuite {:?}", kp.ciphersuite());
        }
        Ok(kp)
    }

    /// Validates a KeyPackage and returns the identity in its credential.
    pub fn inspect_key_package(&self, bytes: &[u8]) -> Result<NodeId> {
        let kp = self.parse_key_package(bytes)?;
        credential_identity(kp.leaf_node().credential())
    }

    /// Whether the given identity is a current member.
    pub fn is_member(&self, id: &NodeId) -> bool {
        self.members().map(|m| m.contains(id)).unwrap_or(false)
    }

    /// Current member identities, in leaf order.
    pub fn members(&self) -> Result<Vec<NodeId>> {
        self.group()?
            .members()
            .map(|m| credential_identity(&m.credential))
            .collect()
    }

    pub fn epoch(&self) -> Result<u64> {
        Ok(self.group()?.epoch().as_u64())
    }

    /// Adds the member in the KeyPackage and merges the commit.
    pub fn add_member(&mut self, key_package: &[u8]) -> Result<AddResult> {
        let kp = self.parse_key_package(key_package)?;
        let joiner = credential_identity(kp.leaf_node().credential())?;
        let members_before = self.members()?.len();
        let provider = &self.provider;
        let signer = &self.signer;
        let group = self
            .group
            .as_mut()
            .ok_or_else(|| anyhow!("no MLS group loaded"))?;
        let (commit, welcome, _group_info) = group
            .add_members(provider, signer, std::slice::from_ref(&kp))
            .map_err(|e| anyhow!("add_members: {e}"))?;
        group
            .merge_pending_commit(provider)
            .map_err(|e| anyhow!("merge_pending_commit: {e}"))?;
        Ok(AddResult {
            joiner,
            commit: commit.to_bytes()?,
            welcome: welcome.to_bytes()?,
            members_before,
            epoch: group.epoch().as_u64(),
        })
    }

    /// Joins a group from a Welcome. The group ID must match.
    pub fn join(
        &mut self,
        welcome: &[u8],
        expected_group_id: &[u8],
        creator: &NodeId,
    ) -> Result<()> {
        if self.group.is_some() {
            bail!("a group is already loaded");
        }
        let msg = MlsMessageIn::tls_deserialize_exact(welcome).context("Welcome is not TLS")?;
        let MlsMessageBodyIn::Welcome(welcome) = msg.extract() else {
            bail!("message is not a Welcome");
        };
        let staged =
            StagedWelcome::new_from_welcome(&self.provider, &Self::join_config(), welcome, None)
                .map_err(|e| anyhow!("staging Welcome: {e}"))?;
        if staged.group_context().group_id().as_slice() != expected_group_id {
            return Err(RejectedMessage("Welcome is for a different group").into());
        }
        // Validate the authenticated Welcome before installing its group state.
        if credential_identity(staged.welcome_sender()?.credential())? != *creator {
            return Err(RejectedMessage("Welcome MLS sender is not the invited creator").into());
        }
        let members = staged
            .members()
            .map(|m| credential_identity(&m.credential))
            .collect::<Result<Vec<_>>>()?;
        if !members.contains(creator) || !members.contains(&self.identity) {
            return Err(RejectedMessage("Welcome is missing the creator or our identity").into());
        }
        let group = staged
            .into_group(&self.provider)
            .map_err(|e| anyhow!("joining from Welcome: {e}"))?;
        self.group = Some(group);
        Ok(())
    }

    /// Processes and merges a commit from another member.
    pub fn process_commit(&mut self, commit: &[u8], expected_sender: &NodeId) -> Result<NodeId> {
        let protocol = parse_protocol_message(commit)?;
        if protocol.content_type() != ContentType::Commit {
            bail!("expected a commit");
        }
        let provider = &self.provider;
        let group = self
            .group
            .as_mut()
            .ok_or_else(|| anyhow!("no MLS group loaded"))?;
        let processed = group
            .process_message(provider, protocol)
            .map_err(|e| anyhow!("processing commit: {e}"))?;
        let sender = credential_identity(processed.credential())?;
        if sender != *expected_sender {
            return Err(
                RejectedMessage("Commit MLS sender does not match the operation author").into(),
            );
        }
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                group
                    .merge_staged_commit(provider, *staged)
                    .map_err(|e| anyhow!("merging commit: {e}"))?;
                Ok(sender)
            }
            _ => bail!("expected a commit"),
        }
    }

    /// Encrypts an application message in the current epoch.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let provider = &self.provider;
        let signer = &self.signer;
        let group = self
            .group
            .as_mut()
            .ok_or_else(|| anyhow!("no MLS group loaded"))?;
        let msg = group
            .create_message(provider, signer, plaintext)
            .map_err(|e| anyhow!("create_message: {e}"))?;
        Ok(msg.to_bytes()?)
    }

    /// Decrypts an application message and returns its MLS sender's identity.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Decrypted, DecryptError> {
        let protocol =
            parse_protocol_message(ciphertext).map_err(|e| DecryptError::Other(e.to_string()))?;
        if protocol.wire_format() != WireFormat::PrivateMessage
            || protocol.content_type() != ContentType::Application
        {
            return Err(DecryptError::Other(
                "expected a private application message".into(),
            ));
        }
        let message_epoch = protocol.epoch().as_u64();
        let provider = &self.provider;
        let group = self
            .group
            .as_mut()
            .ok_or_else(|| DecryptError::Other("no MLS group loaded".into()))?;
        let current = group.epoch().as_u64();
        let processed = match group.process_message(provider, protocol) {
            Ok(p) => p,
            Err(e) => {
                if message_epoch.saturating_add(MAX_PAST_EPOCHS as u64) < current {
                    return Err(DecryptError::TooOld { message_epoch });
                }
                return Err(DecryptError::Other(e.to_string()));
            }
        };
        let sender = credential_identity(processed.credential())
            .map_err(|e| DecryptError::Other(e.to_string()))?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Decrypted {
                sender,
                plaintext: app.into_bytes(),
            }),
            _ => Err(DecryptError::Other(
                "expected an application message".into(),
            )),
        }
    }

    /// The epoch a serialized protocol message claims, if it parses.
    pub fn message_epoch(bytes: &[u8]) -> Option<u64> {
        parse_protocol_message(bytes)
            .ok()
            .map(|p| p.epoch().as_u64())
    }
}

fn parse_protocol_message(bytes: &[u8]) -> Result<ProtocolMessage> {
    let msg = MlsMessageIn::tls_deserialize_exact(bytes).context("message is not TLS")?;
    msg.try_into_protocol_message()
        .map_err(|e| anyhow!("not a protocol message: {e}"))
}

fn credential_identity(credential: &Credential) -> Result<NodeId> {
    let basic = BasicCredential::try_from(credential.clone())
        .map_err(|e| anyhow!("not a basic credential: {e}"))?;
    basic
        .identity()
        .try_into()
        .map_err(|_| anyhow!("credential identity is not 32 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Member {
        _dir: tempfile::TempDir,
        mls: Mls,
        id: NodeId,
    }

    fn member(byte: u8) -> Member {
        let dir = tempfile::tempdir().unwrap();
        let id = [byte; 32];
        let mls = Mls::open_dir(dir.path(), id).unwrap();
        Member { _dir: dir, mls, id }
    }

    fn pair() -> (Member, Member) {
        let mut a = member(0xaa);
        let mut b = member(0xbb);
        a.mls.create_group(&[7; 32]).unwrap();
        assert_eq!(a.mls.epoch().unwrap(), 0);
        let kp = b.mls.new_key_package().unwrap();
        assert_eq!(a.mls.inspect_key_package(&kp).unwrap(), b.id);
        let added = a.mls.add_member(&kp).unwrap();
        assert_eq!(added.joiner, b.id);
        assert_eq!(added.members_before, 1);
        assert_eq!(added.epoch, 1);
        b.mls.join(&added.welcome, &[7; 32], &a.id).unwrap();
        (a, b)
    }

    #[test]
    fn create_add_join_and_message_both_ways() {
        let (mut a, mut b) = pair();
        assert_eq!(a.mls.epoch().unwrap(), 1);
        assert_eq!(b.mls.epoch().unwrap(), 1);
        assert_eq!(b.mls.members().unwrap(), vec![a.id, b.id]);
        assert!(b.mls.is_member(&a.id));

        for i in 0..10 {
            let ct = a.mls.encrypt(format!("a{i}").as_bytes()).unwrap();
            let d = b.mls.decrypt(&ct).unwrap();
            assert_eq!(d.sender, a.id);
            assert_eq!(d.plaintext, format!("a{i}").as_bytes());

            let ct = b.mls.encrypt(format!("b{i}").as_bytes()).unwrap();
            let d = a.mls.decrypt(&ct).unwrap();
            assert_eq!(d.sender, b.id);
            assert_eq!(d.plaintext, format!("b{i}").as_bytes());
        }
    }

    #[test]
    fn ciphertext_is_single_use_and_not_plaintext() {
        let (mut a, mut b) = pair();
        let ct = a.mls.encrypt(b"secret words").unwrap();
        assert!(!ct.windows(12).any(|w| w == b"secret words"));
        b.mls.decrypt(&ct).unwrap();
        assert!(matches!(b.mls.decrypt(&ct), Err(DecryptError::Other(_))));
        // A member cannot decrypt its own ciphertext.
        assert!(a.mls.decrypt(&ct).is_err());
    }

    #[test]
    fn wrong_group_welcome_is_rejected() {
        let mut a = member(1);
        let mut b = member(2);
        a.mls.create_group(&[7; 32]).unwrap();
        let added = a.mls.add_member(&b.mls.new_key_package().unwrap()).unwrap();
        assert!(b.mls.join(&added.welcome, &[8; 32], &a.id).is_err());
    }

    #[test]
    fn reopen_store_mid_conversation() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (ida, idb) = ([1; 32], [2; 32]);
        let gid = [9u8; 32];
        {
            let mut a = Mls::open_dir(dir_a.path(), ida).unwrap();
            let mut b = Mls::open_dir(dir_b.path(), idb).unwrap();
            a.create_group(&gid).unwrap();
            let added = a.add_member(&b.new_key_package().unwrap()).unwrap();
            b.join(&added.welcome, &gid, &ida).unwrap();
            let ct = a.encrypt(b"before").unwrap();
            assert_eq!(b.decrypt(&ct).unwrap().plaintext, b"before");
        }
        // Close and reopen both stores.
        let mut a = Mls::open_dir(dir_a.path(), ida).unwrap();
        let mut b = Mls::open_dir(dir_b.path(), idb).unwrap();
        assert!(a.load_group(&gid).unwrap());
        assert!(b.load_group(&gid).unwrap());
        assert_eq!(a.epoch().unwrap(), 1);
        for i in 0..3 {
            let ct = a.encrypt(format!("after{i}").as_bytes()).unwrap();
            assert_eq!(
                b.decrypt(&ct).unwrap().plaintext,
                format!("after{i}").as_bytes()
            );
            let ct = b.encrypt(b"reply").unwrap();
            assert_eq!(a.decrypt(&ct).unwrap().sender, idb);
        }
    }

    #[test]
    fn key_package_survives_restart_before_welcome() {
        let dir_b = tempfile::tempdir().unwrap();
        let idb = [2; 32];
        let kp = {
            let b = Mls::open_dir(dir_b.path(), idb).unwrap();
            b.new_key_package().unwrap()
        };
        let mut a = member(1);
        a.mls.create_group(&[3; 32]).unwrap();
        let added = a.mls.add_member(&kp).unwrap();
        let mut b = Mls::open_dir(dir_b.path(), idb).unwrap();
        b.join(&added.welcome, &[3; 32], &a.id).unwrap();
        let ct = a.mls.encrypt(b"hi").unwrap();
        assert_eq!(b.decrypt(&ct).unwrap().plaintext, b"hi");
    }

    #[test]
    fn three_members_with_commit_and_past_epoch() {
        let (mut a, mut b) = pair();
        let mut c = member(0xcc);
        // B sends at epoch 1 while A is committing C's add.
        let late = b.mls.encrypt(b"sent during commit").unwrap();
        let added = a.mls.add_member(&c.mls.new_key_package().unwrap()).unwrap();
        assert_eq!(added.members_before, 2);
        assert_eq!(added.epoch, 2);
        assert_eq!(b.mls.process_commit(&added.commit, &a.id).unwrap(), a.id);
        c.mls.join(&added.welcome, &[7; 32], &a.id).unwrap();
        // The epoch-1 message still decrypts at A thanks to max_past_epochs.
        assert_eq!(
            a.mls.decrypt(&late).unwrap().plaintext,
            b"sent during commit"
        );
        for (sender, others) in [(0usize, [1usize, 2]), (1, [0, 2]), (2, [0, 1])] {
            let ms = [&mut a, &mut b, &mut c];
            let ct = ms[sender].mls.encrypt(b"hello three").unwrap();
            let sid = ms[sender].id;
            for o in others {
                let d = ms[o].mls.decrypt(&ct).unwrap();
                assert_eq!(d.sender, sid);
            }
        }
    }

    #[test]
    fn commit_author_is_checked_before_merging() {
        let (mut a, mut b) = pair();
        let c = member(3);
        let added = a.mls.add_member(&c.mls.new_key_package().unwrap()).unwrap();
        let before = b.mls.members().unwrap();
        assert!(b.mls.process_commit(&added.commit, &c.id).is_err());
        assert_eq!(b.mls.epoch().unwrap(), 1);
        assert_eq!(b.mls.members().unwrap(), before);
        // The rejected commit must not change the persisted epoch either.
        let mut reopened = Mls::open_dir(b._dir.path(), b.id).unwrap();
        assert!(reopened.load_group(&[7; 32]).unwrap());
        assert_eq!(reopened.epoch().unwrap(), 1);
        assert_eq!(reopened.members().unwrap(), before);
    }

    #[test]
    fn welcome_creator_is_checked_before_installing_group() {
        let mut a = member(1);
        let mut b = member(2);
        a.mls.create_group(&[7; 32]).unwrap();
        let added = a.mls.add_member(&b.mls.new_key_package().unwrap()).unwrap();
        assert!(b.mls.join(&added.welcome, &[7; 32], &[3; 32]).is_err());
        assert!(!b.mls.has_group());
        assert!(!b.mls.load_group(&[7; 32]).unwrap());
    }

    #[test]
    fn misrouted_commit_does_not_consume_its_message_key() {
        let (mut a, mut b) = pair();
        let c = member(3);
        let added = a.mls.add_member(&c.mls.new_key_package().unwrap()).unwrap();
        assert!(b.mls.decrypt(&added.commit).is_err());
        assert_eq!(b.mls.epoch().unwrap(), 1);
        b.mls.process_commit(&added.commit, &a.id).unwrap();
        assert_eq!(b.mls.epoch().unwrap(), 2);
    }

    #[test]
    fn misrouted_application_does_not_consume_its_message_key() {
        let (mut a, mut b) = pair();
        let ct = a.mls.encrypt(b"still readable").unwrap();
        assert!(b.mls.process_commit(&ct, &a.id).is_err());
        assert_eq!(b.mls.decrypt(&ct).unwrap().plaintext, b"still readable");
    }

    #[test]
    fn envelope_sizes_stay_under_limit() {
        use crate::envelope::{ChatBody, Envelope, MAX_TEXT_BYTES};
        let mut a = member(1);
        let mut b = member(2);
        let c = member(3);
        a.mls.create_group(&[7; 32]).unwrap();
        let kp = Envelope::key_package(b.mls.new_key_package().unwrap());
        let added = a.mls.add_member(&b.mls.new_key_package().unwrap()).unwrap();
        b.mls.join(&added.welcome, &[7; 32], &a.id).unwrap();
        let welcome2 = Envelope::welcome([2; 32], added.welcome);
        let body = ChatBody::new(&"x".repeat(MAX_TEXT_BYTES), u64::MAX).unwrap();
        let app_max = Envelope::app(a.mls.encrypt(&body.encode().unwrap()).unwrap());
        let short = ChatBody::new("hello", 1).unwrap();
        let app_short = Envelope::app(a.mls.encrypt(&short.encode().unwrap()).unwrap());
        let added3 = a.mls.add_member(&c.mls.new_key_package().unwrap()).unwrap();
        let commit3 = Envelope::commit(added3.commit);
        let welcome3 = Envelope::welcome([3; 32], added3.welcome);
        for (name, env) in [
            ("key_package", &kp),
            ("welcome_2_members", &welcome2),
            ("welcome_3_members", &welcome3),
            ("commit_3_members", &commit3),
            ("app_5_bytes", &app_short),
            ("app_2000_bytes", &app_max),
        ] {
            eprintln!("SIZE {name}: {} bytes", env.encoded_len());
            env.check_size().unwrap();
        }
    }

    #[test]
    fn invalid_key_package_is_rejected() {
        let a = member(1);
        assert!(a.mls.inspect_key_package(b"garbage").is_err());
    }
}
