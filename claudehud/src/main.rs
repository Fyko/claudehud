mod fmt;
mod git;
mod incidents;
mod input;
mod render;
mod time;

use std::io::{self, Read};
use std::path::Path;

fn main() {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);

    if raw.trim().is_empty() {
        print!("Claude");
        return;
    }

    let input: input::Input = serde_json::from_str(&raw).unwrap_or_default();
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| git::branch_and_dirty(Path::new(cwd)));

    let incident = incidents::read_incident();
    print!("{}", render::render(&input, git, incident.as_ref()));
}
