//! Tools that act on the channel a turn runs in: `channel.topic` opens a
//! topic under it. Offered only in channel turns ([`Scope::ChannelOnly`]).
//! Like any tool, a member needs a grant for it; the host and admins have
//! every tool.
//!
//! Opening a topic needs the session's spaces, which this crate never
//! touches: the binary implements [`Channels`] over them, as it does
//! [`crate::assistant::Store`] over `state.sqlite`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::context::Kind;
use crate::tools::{BoxFuture, CallContext, Scope, ToolSpec, Tools};

/// What the channel tools do to the session. Errors are text for the model.
pub trait Channels: Send + Sync {
    /// Opens a topic called `name` under the channel this node calls
    /// `channel`. Returns the topic's full name (`channel/name`).
    fn open_topic<'a>(
        &'a self,
        channel: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// The channel tools over a [`Channels`].
pub struct ChannelTools {
    channels: Arc<dyn Channels>,
}

impl ChannelTools {
    pub fn new(channels: Arc<dyn Channels>) -> Self {
        ChannelTools { channels }
    }
}

impl Tools for ChannelTools {
    fn list(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "channel.topic".into(),
            description: "Opens a topic: a sub-channel under this channel, for one subject. \
                          Every member of this channel joins it. The name is 1 to 32 \
                          characters of a-z, 0-9, '-' and '_'."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
            scope: Scope::ChannelOnly,
        }]
    }

    fn call<'a>(
        &'a self,
        cx: &'a CallContext,
        name: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            if name != "channel.topic" {
                return Err(format!("no tool named {name}"));
            }
            if cx.kind != Kind::Channel {
                return Err("channel.topic works only in a channel".into());
            }
            let topic = args
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .ok_or("channel.topic needs a `name`")?;
            let full = self.channels.open_topic(&cx.channel, topic).await?;
            Ok(format!(
                "Opened #{full}. Members of #{} join it on their own.",
                cx.channel
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Fake(Mutex<Vec<(String, String)>>);

    impl Channels for Fake {
        fn open_topic<'a>(
            &'a self,
            channel: &'a str,
            name: &'a str,
        ) -> BoxFuture<'a, Result<String, String>> {
            self.0.lock().unwrap().push((channel.into(), name.into()));
            Box::pin(async move { Ok(format!("{channel}/{name}")) })
        }
    }

    fn cx(kind: Kind) -> CallContext {
        CallContext {
            kind,
            channel: "design".into(),
            asker: "7c1d09ee".into(),
            asker_tier: "host".into(),
        }
    }

    #[tokio::test]
    async fn opens_a_topic_in_the_turns_channel_only() {
        let fake = Arc::new(Fake::default());
        let tools = ChannelTools::new(fake.clone());
        let out = tools
            .call(
                &cx(Kind::Channel),
                "channel.topic",
                json!({"name": "ideas"}),
            )
            .await
            .unwrap();
        assert!(out.contains("#design/ideas"), "{out}");
        assert!(
            tools
                .call(&cx(Kind::Dm), "channel.topic", json!({"name": "x"}))
                .await
                .is_err()
        );
        assert!(
            tools
                .call(&cx(Kind::Channel), "channel.topic", json!({}))
                .await
                .is_err()
        );
        assert_eq!(*fake.0.lock().unwrap(), [("design".into(), "ideas".into())]);
    }
}
