//! Tools the model calls to act on the PC. Each has a JSON schema for the
//! prompt and a risk level: anything above `Safe` needs the user's consent.

mod pc;
mod web;

use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Runs at once: media keys, opening a web page.
    Safe,
    /// Asks the user first: launching programs.
    Confirm,
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON schema of the arguments.
    pub parameters: fn() -> Value,
    pub risk: Risk,
    pub run: fn(&Value) -> Result<String>,
}

pub fn all() -> Vec<Tool> {
    let mut tools = pc::tools();
    if crate::settings::get().web_search {
        tools.extend(web::tools());
    }
    tools
}

pub fn find(name: &str) -> Option<Tool> {
    all().into_iter().find(|t| t.name == name)
}
