//! Skills: standing instructions the host writes as `skills/<name>.md` in
//! the data directory. Each turn's system prompt lists their names and
//! one-line summaries; the model reads one with `skills.read` when it
//! applies. The folder is read again at every turn, so a skill added or
//! edited while the bots run applies from the next turn; `skills.read` is
//! there whenever the folder exists when the bots start.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::tools::{BoxFuture, CallContext, Scope, ToolSpec, Tools};

/// The folder in the data directory that holds the skills.
pub const SKILLS_DIR: &str = "skills";
/// Most skills listed.
const MAX_SKILLS: usize = 50;
/// Most characters of a summary.
const MAX_SUMMARY_CHARS: usize = 200;
/// Most bytes of a skill read.
const MAX_SKILL_BYTES: u64 = 64_000;

/// A skill's name and one-line summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub summary: String,
}

/// Whether `name` is a skill name: letters, digits, `-` and `_`, so it
/// cannot leave the skills folder.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The summary of a skill file: `description:` in YAML front matter, else
/// its first line of text, without a heading's `#`.
pub fn summary(text: &str) -> String {
    let mut lines = text.lines().map(str::trim);
    let mut body_start = text;
    if text.trim_start().starts_with("---") {
        let mut front = lines.by_ref().skip_while(|l| l.is_empty()).skip(1);
        for l in front.by_ref() {
            if l == "---" {
                break;
            }
            if let Some(d) = l.strip_prefix("description:") {
                let d = d.trim().trim_matches(['"', '\'']);
                if !d.is_empty() {
                    return cut(d);
                }
            }
        }
        body_start = "";
    }
    let first = if body_start.is_empty() {
        lines.find(|l| !l.is_empty())
    } else {
        text.lines().map(str::trim).find(|l| !l.is_empty())
    };
    cut(first.unwrap_or("").trim_start_matches('#').trim())
}

fn cut(s: &str) -> String {
    let mut out: String = s.chars().take(MAX_SUMMARY_CHARS).collect();
    if s.chars().count() > MAX_SUMMARY_CHARS {
        out.push('…');
    }
    out
}

/// The skills in `data_dir`, sorted by name. Files whose names are not
/// skill names are skipped.
pub fn list(data_dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(data_dir.join(SKILLS_DIR)) else {
        return Vec::new();
    };
    let mut list = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".md"))
        else {
            continue;
        };
        if !valid_name(name) || !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        list.push(Skill {
            name: name.to_string(),
            summary: summary(&text),
        });
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list.truncate(MAX_SKILLS);
    list
}

/// The path of the skill `name`, checked to be a skill name.
pub fn path(data_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.strip_suffix(".md").unwrap_or(name);
    if !valid_name(name) {
        return Err(format!(
            "{name:?} is not a skill name (letters, digits, - and _ only)"
        ));
    }
    Ok(data_dir.join(SKILLS_DIR).join(format!("{name}.md")))
}

/// The `skills.read` tool over the skills folder.
pub struct Skills {
    dir: PathBuf,
}

impl Skills {
    /// The skills tool, if `data_dir` has a skills folder.
    pub fn load(data_dir: &Path) -> Option<Skills> {
        let dir = data_dir.join(SKILLS_DIR);
        dir.is_dir().then_some(Skills { dir })
    }

    fn read(&self, args: &Value) -> Result<String, String> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or("skills.read needs a `name`")?;
        let name = name.strip_suffix(".md").unwrap_or(name);
        if !valid_name(name) {
            return Err(format!(
                "skills.read: {name:?} is not a skill name (letters, digits, - and _ only)"
            ));
        }
        let path = self.dir.join(format!("{name}.md"));
        let meta = std::fs::metadata(&path).map_err(|_| format!("skills.read: no skill {name}"))?;
        if !meta.is_file() {
            return Err(format!("skills.read: no skill {name}"));
        }
        if meta.len() > MAX_SKILL_BYTES {
            return Err(format!(
                "skills.read: {name} is over {MAX_SKILL_BYTES} bytes"
            ));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("skills.read: {e}"))?;
        Ok(format!("Skill {name}, written by the host:\n{text}"))
    }
}

impl Tools for Skills {
    fn list(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "skills.read".into(),
            description: "Reads one of the host's skills: standing instructions for a kind of \
                          task. The system prompt lists them; read one before a task it covers."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
            scope: Scope::Any,
        }]
    }

    fn call<'a>(
        &'a self,
        _: &'a CallContext,
        name: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            match name {
                "skills.read" => self.read(&args),
                _ => Err(format!("no tool named {name}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        assert_eq!(
            summary("---\nname: x\ndescription: \"Runs standup.\"\n---\n# Body\n"),
            "Runs standup."
        );
        assert_eq!(
            summary("---\nname: x\n---\n\n# Weekly review\nmore"),
            "Weekly review"
        );
        assert_eq!(summary("\n\n## Packing list\n- socks"), "Packing list");
        assert_eq!(summary(""), "");
        assert_eq!(summary(&"y".repeat(300)).chars().count(), 201);
    }

    #[tokio::test]
    async fn reads_only_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Skills::load(dir.path()).is_none());
        assert!(list(dir.path()).is_empty());
        let skills = dir.path().join(SKILLS_DIR);
        std::fs::create_dir(&skills).unwrap();
        std::fs::write(
            skills.join("standup.md"),
            "# Daily standup\nAsk three things.",
        )
        .unwrap();
        std::fs::write(skills.join("bad name.md"), "x").unwrap();
        std::fs::write(skills.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.path().join("secret.md"), "secret").unwrap();
        let s = Skills::load(dir.path()).unwrap();
        assert_eq!(
            list(dir.path()),
            [Skill {
                name: "standup".into(),
                summary: "Daily standup".into()
            }]
        );
        let cx = CallContext::dm();
        let read = |name: &'static str| s.call(&cx, "skills.read", json!({ "name": name }));
        assert!(read("standup").await.unwrap().contains("Ask three things."));
        assert!(read("standup.md").await.is_ok());
        for bad in ["../secret", "..", "a/b", "/etc/passwd", ""] {
            let e = read(bad).await.unwrap_err();
            assert!(e.contains("not a skill name"), "{bad}: {e}");
        }
        assert!(read("missing").await.unwrap_err().contains("no skill"));
    }
}
