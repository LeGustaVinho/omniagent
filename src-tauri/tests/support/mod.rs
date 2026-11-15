use std::{fs, path::Path};

use anyhow::Result;
use uuid::Uuid;

pub fn write_skill(checkout: &Path, directory: &str) -> Result<String> {
    let skill = checkout.join(directory).join("omniagent-skill-fixture");
    fs::create_dir_all(&skill)?;
    let reply = format!(
        "OMNIAGENT_SKILL_{}",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    fs::write(
        skill.join("SKILL.md"),
        format!(
            "---\nname: omniagent-skill-fixture\ndescription: Check explicit skill invocation.\ndisable-model-invocation: true\n---\nReply with exactly {reply} and no punctuation. Do not use tools.\n"
        ),
    )?;
    Ok(reply)
}
