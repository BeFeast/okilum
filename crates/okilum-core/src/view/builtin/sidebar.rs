use crate::{vault_descriptor::Layout, view::ViewDoc};
use serde_json::Value;
use std::collections::BTreeMap;

pub struct SidebarDomain {
    pub key: String,
    pub label: String,
    pub args: BTreeMap<String, Value>,
}

/// The built-in Areas sidebar lists every declared domain, including empty ones.
pub fn sidebar_domains<'a>(
    layout: &'a Layout,
    template: &'a ViewDoc,
) -> impl Iterator<Item = SidebarDomain> + 'a {
    layout.domains.iter().map(|domain| SidebarDomain {
        key: format!("builtin-sidebar/{}", domain.id),
        label: domain.name.clone(),
        args: template.node_args(&[("area".into(), Value::String(domain.name.clone()))]),
    })
}
