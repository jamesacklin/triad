//! Whether a bot answers a message, and with which tools. Pure: no I/O and
//! no clock. The caller passes the member list, grants, blocks, counts and
//! the time.

use std::collections::{BTreeMap, BTreeSet};

pub type NodeId = [u8; 32];

/// A member's access to a space, as far as the bot cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// `Manage`: an admin.
    Manage,
    /// `Write`, `Read` or `Pull`: a member.
    Member,
}

/// What an asker is, from the host's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Host,
    Admin,
    Member,
    NotMember,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Host => "host",
            Tier::Admin => "admin",
            Tier::Member => "member",
            Tier::NotMember => "not a member",
        }
    }
}

/// The tools a turn may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolSet {
    /// Every tool the host's bot has.
    All,
    /// Only tools matching these patterns (`server.tool` or `server.*`).
    /// Empty means no tools.
    Only(BTreeSet<String>),
}

impl ToolSet {
    pub fn none() -> Self {
        ToolSet::Only(BTreeSet::new())
    }

    /// Whether a tool named `server.tool` may be used.
    pub fn allows(&self, tool: &str) -> bool {
        match self {
            ToolSet::All => true,
            ToolSet::Only(patterns) => patterns.iter().any(|p| pattern_matches(p, tool)),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, ToolSet::Only(p) if p.is_empty())
    }

    /// A compact form for storage and logs: `*` or the patterns joined by
    /// commas (empty for none).
    pub fn render(&self) -> String {
        match self {
            ToolSet::All => "*".into(),
            ToolSet::Only(p) => p.iter().cloned().collect::<Vec<_>>().join(","),
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "*" {
            return ToolSet::All;
        }
        ToolSet::Only(
            s.split(',')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `p` is a grant pattern: `server.tool` or `server.*`.
pub fn pattern_valid(p: &str) -> bool {
    match p.split_once('.') {
        Some((server, tool)) => is_ident(server) && (tool == "*" || is_ident(tool)),
        None => false,
    }
}

/// Whether the pattern `p` covers the tool `server.tool`.
pub fn pattern_matches(p: &str, tool: &str) -> bool {
    match (p.split_once('.'), tool.split_once('.')) {
        (Some((ps, pt)), Some((ts, tt))) => ps == ts && (pt == "*" || pt == tt),
        _ => false,
    }
}

/// The configurable limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// A message older than this when processed is not answered.
    pub max_age_ms: u64,
    /// Answers per member per channel per hour.
    pub asker_per_hour: u64,
    /// Answers per channel per day, the host's included.
    pub channel_per_day: u64,
    /// Answers per channel per minute.
    pub channel_per_minute: u64,
    /// Tool calls per turn.
    pub max_steps: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_age_ms: 600_000,
            asker_per_hour: 10,
            channel_per_day: 100,
            channel_per_minute: 6,
            max_steps: 8,
        }
    }
}

/// Answers already counted against the limits, from the job table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub asker_last_hour: u64,
    pub channel_last_day: u64,
    pub channel_last_minute: u64,
}

/// The parts of a received message the policy reads. It never reads text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub author: NodeId,
    pub bot: bool,
    pub ask: bool,
    pub ctl: bool,
    /// The operation's timestamp, in milliseconds.
    pub at_ms: u64,
}

/// Why an addressed message gets no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Blocked,
    NotMember,
    TooOld,
    AskerLimit,
    Budget,
    Burst,
}

impl Refusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Refusal::Blocked => "blocked",
            Refusal::NotMember => "not a member",
            Refusal::TooOld => "too old",
            Refusal::AskerLimit => "asker limit",
            Refusal::Budget => "channel budget",
            Refusal::Burst => "channel burst",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Not addressed to this node's bot: no job at all.
    Ignore,
    /// Addressed, but no model call and no reply.
    Refuse(Refusal),
    /// Answer, offering exactly these tools.
    Answer(ToolSet),
}

/// The host's view of one channel when a message is processed.
#[derive(Clone, Copy, Debug)]
pub struct ChannelView<'a> {
    /// This node, which runs the bot.
    pub host: NodeId,
    /// Whether this node runs the channel's bot right now.
    pub bot_enabled: bool,
    pub members: &'a BTreeMap<NodeId, Access>,
    /// `(node, pattern)` grants in this channel.
    pub grants: &'a [(NodeId, String)],
    pub blocked: &'a BTreeSet<NodeId>,
    /// A job for this message already exists.
    pub handled: bool,
    pub counts: Counts,
}

pub fn tier(view: &ChannelView, node: &NodeId) -> Tier {
    if *node == view.host {
        return Tier::Host;
    }
    match view.members.get(node) {
        Some(Access::Manage) => Tier::Admin,
        Some(Access::Member) => Tier::Member,
        None => Tier::NotMember,
    }
}

/// The tools `node` may use in this channel.
pub fn tools_for(view: &ChannelView, node: &NodeId) -> ToolSet {
    match tier(view, node) {
        Tier::Host | Tier::Admin => ToolSet::All,
        Tier::Member => ToolSet::Only(
            view.grants
                .iter()
                .filter(|(n, _)| n == node)
                .map(|(_, p)| p.clone())
                .collect(),
        ),
        Tier::NotMember => ToolSet::none(),
    }
}

fn too_old(msg: &Incoming, limits: &Limits, now_ms: u64) -> bool {
    now_ms.saturating_sub(msg.at_ms) > limits.max_age_ms
}

/// A message in a channel this node hosts.
pub fn decide_channel(
    msg: &Incoming,
    view: &ChannelView,
    limits: &Limits,
    now_ms: u64,
) -> Decision {
    if !view.bot_enabled || !msg.ask || msg.bot || msg.ctl || view.handled {
        return Decision::Ignore;
    }
    if view.blocked.contains(&msg.author) && msg.author != view.host {
        return Decision::Refuse(Refusal::Blocked);
    }
    if tier(view, &msg.author) == Tier::NotMember {
        return Decision::Refuse(Refusal::NotMember);
    }
    if too_old(msg, limits, now_ms) {
        return Decision::Refuse(Refusal::TooOld);
    }
    let c = view.counts;
    if c.asker_last_hour >= limits.asker_per_hour {
        return Decision::Refuse(Refusal::AskerLimit);
    }
    if c.channel_last_day >= limits.channel_per_day {
        return Decision::Refuse(Refusal::Budget);
    }
    if c.channel_last_minute >= limits.channel_per_minute {
        return Decision::Refuse(Refusal::Burst);
    }
    Decision::Answer(tools_for(view, &msg.author))
}

/// A message in the owner's DM. The owner is its only member, so every line
/// the owner types is answered with every tool. The count limits do not
/// apply; the age and once rules do.
pub fn decide_dm(
    msg: &Incoming,
    owner: &NodeId,
    handled: bool,
    limits: &Limits,
    now_ms: u64,
) -> Decision {
    if msg.bot || msg.ctl || handled {
        return Decision::Ignore;
    }
    if msg.author != *owner {
        return Decision::Refuse(Refusal::NotMember);
    }
    if too_old(msg, limits, now_ms) {
        return Decision::Refuse(Refusal::TooOld);
    }
    Decision::Answer(ToolSet::All)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: NodeId = [1; 32];
    const ADMIN: NodeId = [2; 32];
    const GRANTED: NodeId = [3; 32];
    const MEMBER: NodeId = [4; 32];
    const BLOCKED: NodeId = [5; 32];
    const STRANGER: NodeId = [6; 32];
    const NOW: u64 = 10_000_000;

    struct Fixture {
        members: BTreeMap<NodeId, Access>,
        grants: Vec<(NodeId, String)>,
        blocked: BTreeSet<NodeId>,
    }

    fn fixture() -> Fixture {
        Fixture {
            members: BTreeMap::from([
                (HOST, Access::Manage),
                (ADMIN, Access::Manage),
                (GRANTED, Access::Member),
                (MEMBER, Access::Member),
                (BLOCKED, Access::Member),
            ]),
            grants: vec![
                (GRANTED, "calendar.create_event".into()),
                (GRANTED, "linear.*".into()),
            ],
            blocked: BTreeSet::from([BLOCKED]),
        }
    }

    fn view(f: &Fixture) -> ChannelView<'_> {
        ChannelView {
            host: HOST,
            bot_enabled: true,
            members: &f.members,
            grants: &f.grants,
            blocked: &f.blocked,
            handled: false,
            counts: Counts::default(),
        }
    }

    fn ask(author: NodeId) -> Incoming {
        Incoming {
            author,
            bot: false,
            ask: true,
            ctl: false,
            at_ms: NOW - 1_000,
        }
    }

    fn decide(f: &Fixture, msg: Incoming) -> Decision {
        decide_channel(&msg, &view(f), &Limits::default(), NOW)
    }

    fn only(patterns: &[&str]) -> ToolSet {
        ToolSet::Only(patterns.iter().map(|s| s.to_string()).collect())
    }

    // One test per row of "What an asker may do".

    #[test]
    fn the_host_gets_all_tools() {
        assert_eq!(
            decide(&fixture(), ask(HOST)),
            Decision::Answer(ToolSet::All)
        );
    }

    #[test]
    fn an_admin_gets_all_tools() {
        assert_eq!(
            decide(&fixture(), ask(ADMIN)),
            Decision::Answer(ToolSet::All)
        );
    }

    #[test]
    fn a_member_with_grants_gets_only_those() {
        let d = decide(&fixture(), ask(GRANTED));
        assert_eq!(
            d,
            Decision::Answer(only(&["calendar.create_event", "linear.*"]))
        );
        let Decision::Answer(tools) = d else { panic!() };
        assert!(tools.allows("calendar.create_event"));
        assert!(!tools.allows("calendar.delete_event"));
        assert!(tools.allows("linear.create_issue"));
        assert!(!tools.allows("github.create_issue"));
    }

    #[test]
    fn a_member_gets_an_answer_and_no_tools() {
        let d = decide(&fixture(), ask(MEMBER));
        assert_eq!(d, Decision::Answer(ToolSet::none()));
        let Decision::Answer(tools) = d else { panic!() };
        assert!(tools.is_none());
        assert!(!tools.allows("calendar.create_event"));
    }

    #[test]
    fn a_blocked_node_gets_nothing() {
        assert_eq!(
            decide(&fixture(), ask(BLOCKED)),
            Decision::Refuse(Refusal::Blocked)
        );
        // The host cannot block itself out of its own bot.
        let mut f = fixture();
        f.blocked.insert(HOST);
        assert_eq!(decide(&f, ask(HOST)), Decision::Answer(ToolSet::All));
    }

    #[test]
    fn a_non_member_gets_nothing() {
        assert_eq!(
            decide(&fixture(), ask(STRANGER)),
            Decision::Refuse(Refusal::NotMember)
        );
    }

    #[test]
    fn grants_follow_the_member_list_at_processing_time() {
        // A granted member promoted to admin gets everything; one removed
        // from the space gets nothing, grants or not.
        let mut f = fixture();
        f.members.insert(GRANTED, Access::Manage);
        assert_eq!(decide(&f, ask(GRANTED)), Decision::Answer(ToolSet::All));
        f.members.remove(&GRANTED);
        assert_eq!(
            decide(&f, ask(GRANTED)),
            Decision::Refuse(Refusal::NotMember)
        );
    }

    #[test]
    fn only_addressed_human_messages_are_answered() {
        let f = fixture();
        let mut m = ask(MEMBER);
        m.ask = false;
        assert_eq!(decide(&f, m), Decision::Ignore);
        let mut m = ask(MEMBER);
        m.bot = true;
        assert_eq!(decide(&f, m), Decision::Ignore);
        let mut m = ask(MEMBER);
        m.ctl = true;
        assert_eq!(decide(&f, m), Decision::Ignore);
        let mut v = view(&f);
        v.bot_enabled = false;
        assert_eq!(
            decide_channel(&ask(HOST), &v, &Limits::default(), NOW),
            Decision::Ignore
        );
    }

    #[test]
    fn a_message_is_answered_at_most_once() {
        let f = fixture();
        let mut v = view(&f);
        v.handled = true;
        assert_eq!(
            decide_channel(&ask(HOST), &v, &Limits::default(), NOW),
            Decision::Ignore
        );
    }

    #[test]
    fn old_messages_are_not_answered() {
        let f = fixture();
        let mut m = ask(MEMBER);
        m.at_ms = NOW - 600_000;
        assert!(matches!(decide(&f, m), Decision::Answer(_)));
        m.at_ms = NOW - 600_001;
        assert_eq!(decide(&f, m), Decision::Refuse(Refusal::TooOld));
    }

    #[test]
    fn limits() {
        let f = fixture();
        let limits = Limits::default();
        let with = |counts: Counts| {
            let mut v = view(&f);
            v.counts = counts;
            decide_channel(&ask(MEMBER), &v, &limits, NOW)
        };
        let ok = Counts {
            asker_last_hour: 9,
            channel_last_day: 99,
            channel_last_minute: 5,
        };
        assert!(matches!(with(ok), Decision::Answer(_)));
        assert_eq!(
            with(Counts {
                asker_last_hour: 10,
                ..ok
            }),
            Decision::Refuse(Refusal::AskerLimit)
        );
        assert_eq!(
            with(Counts {
                channel_last_day: 100,
                ..ok
            }),
            Decision::Refuse(Refusal::Budget)
        );
        assert_eq!(
            with(Counts {
                channel_last_minute: 6,
                ..ok
            }),
            Decision::Refuse(Refusal::Burst)
        );
    }

    #[test]
    fn dm_answers_only_its_owner() {
        let l = Limits::default();
        let mut m = ask(HOST);
        m.ask = false;
        assert_eq!(
            decide_dm(&m, &HOST, false, &l, NOW),
            Decision::Answer(ToolSet::All)
        );
        assert_eq!(decide_dm(&m, &HOST, true, &l, NOW), Decision::Ignore);
        m.bot = true;
        assert_eq!(decide_dm(&m, &HOST, false, &l, NOW), Decision::Ignore);
        assert_eq!(
            decide_dm(&ask(STRANGER), &HOST, false, &l, NOW),
            Decision::Refuse(Refusal::NotMember)
        );
        let mut old = ask(HOST);
        old.at_ms = 0;
        assert_eq!(
            decide_dm(&old, &HOST, false, &l, NOW),
            Decision::Refuse(Refusal::TooOld)
        );
    }

    #[test]
    fn patterns() {
        for good in ["calendar.create_event", "linear.*", "a-b.c_d"] {
            assert!(pattern_valid(good), "{good}");
        }
        for bad in ["calendar", "*.*", ".x", "x.", "a.b.c", "cal endar.x"] {
            assert!(!pattern_valid(bad), "{bad}");
        }
        assert!(pattern_matches("linear.*", "linear.create_issue"));
        assert!(!pattern_matches("linear.*", "linearx.create_issue"));
        assert!(!pattern_matches("calendar.create", "calendar.create_event"));
    }

    #[test]
    fn tool_sets_round_trip() {
        for t in [ToolSet::All, ToolSet::none(), only(&["a.b", "c.*"])] {
            assert_eq!(ToolSet::parse(&t.render()), t);
        }
    }
}
