//! Acceptance rules and the join state machine.
//!
//! Input: an operation's author, hash and envelope plus the current group view.
//! Output: a list of [`Effect`]s for the caller to apply in order. MLS is
//! reached through the [`GroupCrypto`] trait so the rules are unit-testable
//! with no network and no clock.

use anyhow::Result;

use crate::envelope::{ChatBody, Envelope, VERSION};
use crate::identity::NodeId;
use crate::mls::{AddResult, DecryptError, Decrypted, Mls, RejectedMessage};
use crate::state::{JoinStatus, Outcome, Role, StoredMessage};

/// The MLS operations the rules need.
pub trait GroupCrypto {
    fn inspect_key_package(&self, key_package: &[u8]) -> Result<NodeId>;
    fn is_member(&self, id: &NodeId) -> bool;
    fn members(&self) -> Vec<NodeId>;
    fn epoch(&self) -> u64;
    fn add_member(&mut self, key_package: &[u8]) -> Result<AddResult>;
    fn join(&mut self, welcome: &[u8], group_id: &[u8], creator: &NodeId) -> Result<()>;
    fn process_commit(&mut self, commit: &[u8], expected_sender: &NodeId) -> Result<NodeId>;
    fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Decrypted, DecryptError>;
    fn message_epoch(&self, ciphertext: &[u8]) -> Option<u64>;
}

impl GroupCrypto for Mls {
    fn inspect_key_package(&self, key_package: &[u8]) -> Result<NodeId> {
        Mls::inspect_key_package(self, key_package)
    }
    fn is_member(&self, id: &NodeId) -> bool {
        Mls::is_member(self, id)
    }
    fn members(&self) -> Vec<NodeId> {
        Mls::members(self).unwrap_or_default()
    }
    fn epoch(&self) -> u64 {
        Mls::epoch(self).unwrap_or(0)
    }
    fn add_member(&mut self, key_package: &[u8]) -> Result<AddResult> {
        Mls::add_member(self, key_package)
    }
    fn join(&mut self, welcome: &[u8], group_id: &[u8], creator: &NodeId) -> Result<()> {
        Mls::join(self, welcome, group_id, creator)
    }
    fn process_commit(&mut self, commit: &[u8], expected_sender: &NodeId) -> Result<NodeId> {
        Mls::process_commit(self, commit, expected_sender)
    }
    fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Decrypted, DecryptError> {
        Mls::decrypt(self, ciphertext)
    }
    fn message_epoch(&self, ciphertext: &[u8]) -> Option<u64> {
        Mls::message_epoch(ciphertext)
    }
}

/// What the rules need to know about local state.
#[derive(Clone, Debug)]
pub struct GroupView {
    pub me: NodeId,
    pub role: Role,
    pub status: JoinStatus,
    pub creator: NodeId,
    pub group_id: Vec<u8>,
    pub joined_epoch: Option<u64>,
}

/// One operation as delivered by p2panda (author already signature-verified).
pub struct OpInput<'a> {
    pub author: NodeId,
    pub hash: [u8; 32],
    pub timestamp: u64,
    pub envelope: &'a Envelope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Publish an envelope on the topic (record its hash as processed).
    Publish(Envelope),
    /// Ask the user whether to admit this member. The operation stays
    /// unacknowledged until they answer.
    AskUser { member: NodeId },
    /// This member is now joined; persist status and epoch.
    SetJoined { epoch: u64 },
    /// Tell the user a member joined.
    Joined {
        member: NodeId,
        epoch: u64,
        members: usize,
    },
    /// The group moved to a new epoch through a commit.
    EpochChanged { epoch: u64, members: usize },
    /// Record the operation's outcome, with its decrypted message if any, in
    /// one `state.sqlite` transaction.
    Record {
        outcome: Outcome,
        reason: Option<String>,
        message: Option<StoredMessage>,
    },
    /// Keep a future-epoch application unacknowledged until a commit arrives.
    Defer,
    /// Acknowledge the operation to p2panda.
    Ack,
}

fn done(outcome: Outcome, reason: impl Into<String>) -> Vec<Effect> {
    vec![
        Effect::Record {
            outcome,
            reason: Some(reason.into()),
            message: None,
        },
        Effect::Ack,
    ]
}

fn skip(reason: impl Into<String>) -> Vec<Effect> {
    done(Outcome::Skipped, reason)
}

fn fail(reason: impl Into<String>) -> Vec<Effect> {
    done(Outcome::Failed, reason)
}

fn ok() -> [Effect; 2] {
    [
        Effect::Record {
            outcome: Outcome::Ok,
            reason: None,
            message: None,
        },
        Effect::Ack,
    ]
}

/// Applies the acceptance rules to one operation the app has not processed yet.
pub fn handle(
    view: &GroupView,
    op: &OpInput<'_>,
    crypto: &mut impl GroupCrypto,
    auto_accept: bool,
) -> Vec<Effect> {
    if op.envelope.version() != VERSION {
        return skip(format!(
            "unsupported envelope version {}",
            op.envelope.version()
        ));
    }
    if let Err(e) = op.envelope.check_size() {
        return skip(e.to_string());
    }
    match op.envelope {
        Envelope::KeyPackage { key_package, .. } => {
            on_key_package(view, op, key_package, crypto, auto_accept)
        }
        Envelope::Welcome { to, welcome, .. } => on_welcome(view, op, to, welcome, crypto),
        Envelope::Commit { commit, .. } => on_commit(view, op, commit, crypto),
        Envelope::App { ciphertext, .. } => on_app(view, op, ciphertext, crypto),
    }
}

/// Checks a KeyPackage against the rules without changing MLS state.
pub fn check_key_package(
    view: &GroupView,
    author: &NodeId,
    key_package: &[u8],
    crypto: &impl GroupCrypto,
) -> Result<(), String> {
    if view.role != Role::Creator {
        return Err("KeyPackage: I am not the creator".into());
    }
    let identity = crypto
        .inspect_key_package(key_package)
        .map_err(|e| format!("KeyPackage does not validate: {e}"))?;
    if identity != *author {
        return Err("KeyPackage credential identity does not match the operation author".into());
    }
    if crypto.is_member(author) {
        return Err("KeyPackage author is already a member".into());
    }
    Ok(())
}

fn on_key_package(
    view: &GroupView,
    op: &OpInput<'_>,
    key_package: &[u8],
    crypto: &mut impl GroupCrypto,
    auto_accept: bool,
) -> Vec<Effect> {
    if let Err(reason) = check_key_package(view, &op.author, key_package, crypto) {
        return skip(reason);
    }
    if auto_accept {
        accept_join(view, &op.author, key_package, crypto)
    } else {
        vec![Effect::AskUser { member: op.author }]
    }
}

/// Admits the member behind a KeyPackage, after the user (or `--auto-accept`) agreed.
pub fn accept_join(
    view: &GroupView,
    author: &NodeId,
    key_package: &[u8],
    crypto: &mut impl GroupCrypto,
) -> Vec<Effect> {
    // Re-check: state may have changed while the user was deciding.
    if let Err(reason) = check_key_package(view, author, key_package, crypto) {
        return skip(reason);
    }
    let added = match crypto.add_member(key_package) {
        Ok(a) => a,
        Err(e) => return fail(format!("add_members failed: {e}")),
    };
    let mut effects = Vec::new();
    if added.members_before > 1 {
        effects.push(Effect::Publish(Envelope::commit(added.commit)));
    }
    effects.push(Effect::Publish(Envelope::welcome(
        added.joiner,
        added.welcome,
    )));
    effects.push(Effect::Joined {
        member: added.joiner,
        epoch: added.epoch,
        members: added.members_before + 1,
    });
    effects.extend(ok());
    effects
}

/// The user declined a join request.
pub fn decline_join() -> Vec<Effect> {
    skip("join request declined by the user")
}

fn on_welcome(
    view: &GroupView,
    op: &OpInput<'_>,
    to: &[u8; 32],
    welcome: &[u8],
    crypto: &mut impl GroupCrypto,
) -> Vec<Effect> {
    if op.author != view.creator {
        return skip("Welcome not from the creator named in my invite");
    }
    if *to != view.me {
        return skip("Welcome addressed to another member");
    }
    if view.status != JoinStatus::Pending {
        return skip("Welcome while not pending");
    }
    if let Err(e) = crypto.join(welcome, &view.group_id, &view.creator) {
        if e.is::<RejectedMessage>() {
            return skip(e.to_string());
        }
        return fail(format!("joining from Welcome failed: {e}"));
    }
    let members = crypto.members();
    if !members.contains(&view.creator) {
        return fail("creator is not in the group's member list");
    }
    let epoch = crypto.epoch();
    let mut effects = vec![
        Effect::SetJoined { epoch },
        Effect::Joined {
            member: view.me,
            epoch,
            members: members.len(),
        },
    ];
    effects.extend(ok());
    effects
}

fn on_commit(
    view: &GroupView,
    op: &OpInput<'_>,
    commit: &[u8],
    crypto: &mut impl GroupCrypto,
) -> Vec<Effect> {
    if op.author != view.creator {
        return skip("Commit not from the creator");
    }
    if view.status != JoinStatus::Joined {
        return skip("Commit while not joined");
    }
    match crypto.process_commit(commit, &op.author) {
        Ok(sender) if sender == op.author => {}
        Ok(_) => return fail("Commit's MLS sender does not match the operation author"),
        Err(e) if e.is::<RejectedMessage>() => return skip(e.to_string()),
        Err(e) => return fail(format!("processing commit failed: {e}")),
    }
    let mut effects = vec![Effect::EpochChanged {
        epoch: crypto.epoch(),
        members: crypto.members().len(),
    }];
    effects.extend(ok());
    effects
}

fn on_app(
    view: &GroupView,
    op: &OpInput<'_>,
    ciphertext: &[u8],
    crypto: &mut impl GroupCrypto,
) -> Vec<Effect> {
    if view.status != JoinStatus::Joined {
        return skip("App message while not joined");
    }
    // Different author logs can arrive before the creator's commit log.
    // Do not consume or acknowledge a message whose epoch is still ahead.
    if crypto
        .message_epoch(ciphertext)
        .is_some_and(|epoch| epoch > crypto.epoch())
    {
        return vec![Effect::Defer];
    }
    let joined_epoch = view.joined_epoch.unwrap_or(0);
    let decrypted = match crypto.decrypt(ciphertext) {
        Ok(d) => d,
        Err(DecryptError::TooOld { message_epoch }) => {
            return skip(format!("App from epoch {message_epoch}, before I can read"));
        }
        Err(DecryptError::Other(e)) => {
            return match crypto.message_epoch(ciphertext) {
                Some(epoch) if epoch < joined_epoch => skip(format!(
                    "App from epoch {epoch}, before I joined at {joined_epoch}"
                )),
                _ => fail(format!("decrypt failed: {e}")),
            };
        }
    };
    if decrypted.sender != op.author {
        return skip("MLS sender does not match the operation author");
    }
    let body = match ChatBody::decode(&decrypted.plaintext) {
        Ok(b) => b,
        Err(e) => return skip(format!("bad chat body: {e}")),
    };
    vec![
        Effect::Record {
            outcome: Outcome::Ok,
            reason: None,
            message: Some(StoredMessage {
                op_hash: op.hash,
                author: op.author,
                sent_at_ms: body.sent_at_ms,
                op_ts: Some(op.timestamp),
                text: body.text,
            }),
        },
        Effect::Ack,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    const ME: NodeId = [1; 32];
    const CREATOR: NodeId = [2; 32];
    const JOINER: NodeId = [3; 32];
    const OTHER: NodeId = [4; 32];
    const GID: [u8; 32] = [9; 32];

    /// Fake MLS: a KeyPackage is `b"kp" ++ identity`; a Welcome is `b"wel"`;
    /// a ciphertext is `epoch(1) ++ sender(32) ++ plaintext`.
    #[derive(Default)]
    struct Fake {
        members: Vec<NodeId>,
        epoch: u64,
        joined: bool,
        join_epoch: u64,
        commits: usize,
        consumed: Vec<Vec<u8>>,
    }

    impl GroupCrypto for Fake {
        fn inspect_key_package(&self, kp: &[u8]) -> Result<NodeId> {
            match kp.strip_prefix(b"kp") {
                Some(id) if id.len() == 32 => Ok(id.try_into().unwrap()),
                _ => bail!("bad key package"),
            }
        }
        fn is_member(&self, id: &NodeId) -> bool {
            self.members.contains(id)
        }
        fn members(&self) -> Vec<NodeId> {
            self.members.clone()
        }
        fn epoch(&self) -> u64 {
            self.epoch
        }
        fn add_member(&mut self, kp: &[u8]) -> Result<AddResult> {
            let joiner = self.inspect_key_package(kp)?;
            let before = self.members.len();
            self.members.push(joiner);
            self.epoch += 1;
            Ok(AddResult {
                joiner,
                commit: b"commit".to_vec(),
                welcome: b"wel".to_vec(),
                members_before: before,
                epoch: self.epoch,
            })
        }
        fn join(&mut self, welcome: &[u8], group_id: &[u8], creator: &NodeId) -> Result<()> {
            if welcome != b"wel" || group_id != GID || *creator != CREATOR {
                bail!("bad welcome");
            }
            self.joined = true;
            self.members = vec![CREATOR, ME];
            self.epoch = 1;
            self.join_epoch = 1;
            Ok(())
        }
        fn process_commit(&mut self, commit: &[u8], expected_sender: &NodeId) -> Result<NodeId> {
            if commit == b"spoofed" {
                return Err(RejectedMessage("Commit MLS sender differs from author").into());
            }
            if commit != b"commit" || *expected_sender != CREATOR {
                bail!("bad commit");
            }
            self.commits += 1;
            self.epoch += 1;
            self.members.push(OTHER);
            Ok(CREATOR)
        }
        fn decrypt(&mut self, ct: &[u8]) -> Result<Decrypted, DecryptError> {
            if self.consumed.iter().any(|c| c == ct) {
                return Err(DecryptError::Other("secret reuse".into()));
            }
            let epoch = ct[0] as u64;
            if epoch + 2 < self.epoch {
                return Err(DecryptError::TooOld {
                    message_epoch: epoch,
                });
            }
            if epoch > self.epoch || epoch < self.join_epoch || !self.joined {
                return Err(DecryptError::Other("wrong epoch".into()));
            }
            self.consumed.push(ct.to_vec());
            Ok(Decrypted {
                sender: ct[1..33].try_into().unwrap(),
                plaintext: ct[33..].to_vec(),
            })
        }
        fn message_epoch(&self, ct: &[u8]) -> Option<u64> {
            ct.first().map(|e| *e as u64)
        }
    }

    fn creator_view() -> GroupView {
        GroupView {
            me: CREATOR,
            role: Role::Creator,
            status: JoinStatus::Joined,
            creator: CREATOR,
            group_id: GID.to_vec(),
            joined_epoch: Some(0),
        }
    }

    fn member_view(status: JoinStatus) -> GroupView {
        GroupView {
            me: ME,
            role: Role::Member,
            status,
            creator: CREATOR,
            group_id: GID.to_vec(),
            joined_epoch: (status == JoinStatus::Joined).then_some(1),
        }
    }

    fn creator_fake() -> Fake {
        Fake {
            members: vec![CREATOR],
            joined: true,
            ..Default::default()
        }
    }

    fn joined_fake() -> Fake {
        Fake {
            members: vec![CREATOR, ME],
            epoch: 1,
            joined: true,
            join_epoch: 1,
            ..Default::default()
        }
    }

    fn kp(id: NodeId) -> Envelope {
        let mut b = b"kp".to_vec();
        b.extend_from_slice(&id);
        Envelope::key_package(b)
    }

    fn app(epoch: u8, sender: NodeId, text: &str) -> Envelope {
        let mut b = vec![epoch];
        b.extend_from_slice(&sender);
        b.extend_from_slice(&ChatBody::new(text, 5).unwrap().encode().unwrap());
        Envelope::app(b)
    }

    fn run(
        view: &GroupView,
        author: NodeId,
        env: &Envelope,
        fake: &mut Fake,
        auto: bool,
    ) -> Vec<Effect> {
        let op = OpInput {
            author,
            hash: [7; 32],
            timestamp: 1_000,
            envelope: env,
        };
        handle(view, &op, fake, auto)
    }

    fn outcome(effects: &[Effect]) -> Option<Outcome> {
        effects.iter().find_map(|e| match e {
            Effect::Record { outcome, .. } => Some(*outcome),
            _ => None,
        })
    }

    fn acked(effects: &[Effect]) -> bool {
        effects.last() == Some(&Effect::Ack)
    }

    // --- KeyPackage row ---

    #[test]
    fn key_package_accepted_with_auto_accept() {
        let mut fake = creator_fake();
        let fx = run(&creator_view(), JOINER, &kp(JOINER), &mut fake, true);
        assert_eq!(outcome(&fx), Some(Outcome::Ok));
        assert!(acked(&fx));
        // Two members: Welcome only, no Commit.
        let publishes: Vec<_> = fx
            .iter()
            .filter(|e| matches!(e, Effect::Publish(_)))
            .collect();
        assert_eq!(publishes.len(), 1);
        assert!(
            matches!(publishes[0], Effect::Publish(Envelope::Welcome { to, .. }) if *to == JOINER)
        );
        assert!(fx.contains(&Effect::Joined {
            member: JOINER,
            epoch: 1,
            members: 2
        }));
    }

    #[test]
    fn key_package_asks_user_without_auto_accept() {
        let mut fake = creator_fake();
        let fx = run(&creator_view(), JOINER, &kp(JOINER), &mut fake, false);
        assert_eq!(fx, vec![Effect::AskUser { member: JOINER }]);
        assert_eq!(
            fake.members.len(),
            1,
            "nothing changes until the user answers"
        );
        let fx = accept_join(&creator_view(), &JOINER, b"kp\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03\x03", &mut fake);
        assert_eq!(outcome(&fx), Some(Outcome::Ok));
        assert_eq!(outcome(&decline_join()), Some(Outcome::Skipped));
    }

    #[test]
    fn key_package_skipped_when_not_creator() {
        let mut fake = joined_fake();
        let fx = run(
            &member_view(JoinStatus::Joined),
            JOINER,
            &kp(JOINER),
            &mut fake,
            true,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
        assert!(acked(&fx));
    }

    #[test]
    fn key_package_skipped_when_invalid() {
        let mut fake = creator_fake();
        let env = Envelope::key_package(b"junk".to_vec());
        let fx = run(&creator_view(), JOINER, &env, &mut fake, true);
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn key_package_skipped_when_identity_differs_from_author() {
        let mut fake = creator_fake();
        let fx = run(&creator_view(), OTHER, &kp(JOINER), &mut fake, true);
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
        assert_eq!(fake.members, vec![CREATOR]);
    }

    #[test]
    fn key_package_skipped_when_already_member() {
        let mut fake = creator_fake();
        fake.members.push(JOINER);
        let fx = run(&creator_view(), JOINER, &kp(JOINER), &mut fake, true);
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn third_member_gets_commit_before_welcome() {
        let mut fake = creator_fake();
        fake.members.push(JOINER);
        fake.epoch = 1;
        let fx = run(&creator_view(), OTHER, &kp(OTHER), &mut fake, true);
        let kinds: Vec<&str> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::Publish(env) => Some(env.kind()),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["commit", "welcome"]);
    }

    // --- Welcome row ---

    fn welcome_to(to: NodeId) -> Envelope {
        Envelope::welcome(to, b"wel".to_vec())
    }

    #[test]
    fn welcome_joins_when_pending() {
        let mut fake = Fake::default();
        let fx = run(
            &member_view(JoinStatus::Pending),
            CREATOR,
            &welcome_to(ME),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Ok));
        assert!(fx.contains(&Effect::SetJoined { epoch: 1 }));
        assert!(fx.contains(&Effect::Joined {
            member: ME,
            epoch: 1,
            members: 2
        }));
    }

    #[test]
    fn welcome_skipped_when_not_from_creator() {
        let mut fake = Fake::default();
        let fx = run(
            &member_view(JoinStatus::Pending),
            OTHER,
            &welcome_to(ME),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
        assert!(!fake.joined);
    }

    #[test]
    fn welcome_skipped_when_addressed_elsewhere() {
        let mut fake = Fake::default();
        let fx = run(
            &member_view(JoinStatus::Pending),
            CREATOR,
            &welcome_to(OTHER),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn welcome_skipped_when_not_pending() {
        let mut fake = joined_fake();
        let fx = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &welcome_to(ME),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn welcome_failed_when_mls_rejects_it() {
        let mut fake = Fake::default();
        let env = Envelope::welcome(ME, b"bad".to_vec());
        let fx = run(
            &member_view(JoinStatus::Pending),
            CREATOR,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Failed));
        assert!(acked(&fx));
    }

    // --- Commit row ---

    #[test]
    fn commit_processed_when_from_creator_and_joined() {
        let mut fake = joined_fake();
        let env = Envelope::commit(b"commit".to_vec());
        let fx = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Ok));
        assert_eq!(fake.commits, 1);
        assert!(fx.contains(&Effect::EpochChanged {
            epoch: 2,
            members: 3
        }));
    }

    #[test]
    fn commit_with_mismatched_mls_sender_is_skipped_without_merging() {
        let mut fake = joined_fake();
        let env = Envelope::commit(b"spoofed".to_vec());
        let effects = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&effects), Some(Outcome::Skipped));
        assert!(acked(&effects));
        assert_eq!(fake.epoch, 1);
        assert_eq!(fake.commits, 0);
    }

    #[test]
    fn commit_skipped_when_not_from_creator() {
        let mut fake = joined_fake();
        let env = Envelope::commit(b"commit".to_vec());
        let fx = run(
            &member_view(JoinStatus::Joined),
            OTHER,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
        assert_eq!(fake.commits, 0);
    }

    #[test]
    fn commit_skipped_when_not_joined() {
        let mut fake = Fake::default();
        let env = Envelope::commit(b"commit".to_vec());
        let fx = run(
            &member_view(JoinStatus::Pending),
            CREATOR,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    // --- App row ---

    #[test]
    fn app_decrypted_and_stored() {
        let mut fake = joined_fake();
        let fx = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &app(1, CREATOR, "hi"),
            &mut fake,
            false,
        );
        let Effect::Record {
            outcome,
            message: Some(m),
            ..
        } = &fx[0]
        else {
            panic!("expected a stored message: {fx:?}");
        };
        assert_eq!(*outcome, Outcome::Ok);
        assert_eq!(m.text, "hi");
        assert_eq!(m.author, CREATOR);
        assert_eq!(m.op_ts, Some(1_000));
        assert!(acked(&fx));
    }

    #[test]
    fn app_skipped_when_not_joined() {
        let mut fake = Fake::default();
        let fx = run(
            &member_view(JoinStatus::Pending),
            CREATOR,
            &app(0, CREATOR, "early"),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn app_skipped_when_mls_sender_differs_from_author() {
        let mut fake = joined_fake();
        let fx = run(
            &member_view(JoinStatus::Joined),
            OTHER,
            &app(1, CREATOR, "spoof"),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn app_from_before_join_is_skipped() {
        let mut fake = joined_fake();
        let fx = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &app(0, CREATOR, "old"),
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
    }

    #[test]
    fn replayed_app_after_consumed_key_is_failed() {
        let mut fake = joined_fake();
        let env = app(1, CREATOR, "once");
        let view = member_view(JoinStatus::Joined);
        assert_eq!(
            outcome(&run(&view, CREATOR, &env, &mut fake, false)),
            Some(Outcome::Ok)
        );
        // Crash gap: the state write was lost, so the op is replayed; the key is gone.
        assert_eq!(
            outcome(&run(&view, CREATOR, &env, &mut fake, false)),
            Some(Outcome::Failed)
        );
    }

    #[test]
    fn future_epoch_waits_for_commit_without_consuming_or_acking() {
        let mut fake = joined_fake();
        let view = member_view(JoinStatus::Joined);
        let env = app(2, OTHER, "after commit");
        let effects = run(&view, OTHER, &env, &mut fake, false);
        assert_eq!(effects, vec![Effect::Defer]);
        assert!(!acked(&effects));
        assert!(fake.consumed.is_empty());
        let commit = Envelope::commit(b"commit".to_vec());
        assert_eq!(
            outcome(&run(&view, CREATOR, &commit, &mut fake, false)),
            Some(Outcome::Ok)
        );
        let effects = run(&view, OTHER, &env, &mut fake, false);
        assert_eq!(outcome(&effects), Some(Outcome::Ok));
        assert!(acked(&effects));
        assert_eq!(fake.consumed.len(), 1);
    }

    #[test]
    fn oversized_envelope_is_skipped_before_mls() {
        let mut fake = joined_fake();
        let env = Envelope::app(vec![0; crate::envelope::MAX_ENVELOPE_BYTES]);
        let effects = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&effects), Some(Outcome::Skipped));
        assert!(acked(&effects));
        assert!(fake.consumed.is_empty());
    }

    #[test]
    fn unknown_version_is_skipped() {
        let mut fake = joined_fake();
        let env = Envelope::App {
            v: 2,
            ciphertext: serde_bytes::ByteBuf::from(vec![1]),
        };
        let fx = run(
            &member_view(JoinStatus::Joined),
            CREATOR,
            &env,
            &mut fake,
            false,
        );
        assert_eq!(outcome(&fx), Some(Outcome::Skipped));
        assert!(acked(&fx));
    }
}
