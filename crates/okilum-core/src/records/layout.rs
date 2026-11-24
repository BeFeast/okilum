//! Project lifecycle paths described by the vault.
use crate::{vault_conventions, vault_descriptor};
use anyhow::{bail, ensure, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_yaml::Value as Yaml;
use std::collections::BTreeMap;
use time::{Date, Month};

#[derive(Clone, Debug)]
pub(super) struct Layout {
    templates: Vec<(String, Regex)>,
    domains: BTreeMap<String, String>,
    legacy_domains: Vec<String>,
}

#[derive(Default)]
pub(super) struct PathFields<'a> {
    pub layer: Option<&'a str>,
    pub area: Option<&'a str>,
    pub context: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub date: Option<Date>,
    pub domain_root: bool,
    pub context_root: bool,
    pub legacy: bool,
}

#[derive(Deserialize)]
struct Domain {
    area: String,
    dir: String,
}

impl Layout {
    pub fn compile(templates: &Yaml, domains: &Yaml) -> Result<Self> {
        let domains: Vec<Domain> =
            serde_yaml::from_value(domains.clone()).context("layout domains")?;
        let domains: BTreeMap<_, _> = domains.into_iter().map(|d| (d.dir, d.area)).collect();
        let templates: BTreeMap<String, String> = serde_yaml::from_value(templates.clone())?;
        Self::build(templates, domains)
    }

    pub fn from_descriptor(layout: &vault_descriptor::Layout) -> Result<Self> {
        let roots = &layout.roots;
        let templates = [
            ("inbox", format!("{}/{{rest}}", roots.inbox)),
            (
                "sources",
                format!(
                    "{}/{{channel}}/{{yyyy}}/{{mm}}/{{dd}}/{{rest}}",
                    roots.sources
                ),
            ),
            ("knowledge", format!("{}/{{practice}}.md", roots.knowledge)),
            (
                "state",
                format!("{}/{{domain}}/{{context}}/{{rest}}", roots.state),
            ),
            (
                "log",
                format!(
                    "{}/{{domain}}/{{context}}/{{kind}}/{{date}}-{{slug}}.md",
                    roots.log
                ),
            ),
            ("views", format!("{}/{{rest}}", roots.views)),
            ("schema", "@Schema/{rest}".into()),
        ]
        .map(|(layer, template)| (layer.to_owned(), template))
        .into();
        let domains = layout
            .domains
            .iter()
            .flat_map(|domain| {
                let state = vault_descriptor::suffix(&domain.path, &roots.state)
                    .filter(|path| !path.contains('/'));
                std::iter::once(domain.id.as_str())
                    .chain(state)
                    .map(|id| (id.to_owned(), domain.name.clone()))
            })
            .collect();
        Self::build(templates, domains)
    }

    fn build(
        templates: BTreeMap<String, String>,
        domains: BTreeMap<String, String>,
    ) -> Result<Self> {
        let templates = templates
            .into_iter()
            .map(|(layer, template)| Ok((layer.clone(), template_regex(&layer, &template)?)))
            .collect::<Result<_>>()?;
        let legacy_domains = domains.values().cloned().collect();
        Ok(Self {
            templates,
            domains,
            legacy_domains,
        })
    }

    pub fn project<'a>(&'a self, path: &'a str) -> PathFields<'a> {
        for (layer, template) in &self.templates {
            let Some(captures) = template.captures(path) else {
                continue;
            };
            let grab = |key| captures.name(key).map(|value| value.as_str());
            let area =
                grab("domain").and_then(|domain| self.domains.get(domain).map(String::as_str));
            let context = grab("context");
            let domain_root = grab("domain_root").is_some();
            let kind = grab("kind");
            let channel = grab("channel");
            let date = match layer.as_str() {
                "sources" => (|| {
                    Date::from_calendar_date(
                        grab("yyyy")?.parse().ok()?,
                        Month::try_from(grab("mm")?.parse::<u8>().ok()?).ok()?,
                        grab("dd")?.parse().ok()?,
                    )
                    .ok()
                })(),
                "log" => grab("entry").and_then(|entry| entry.rsplit('/').find_map(leading_date)),
                _ => None,
            };
            return PathFields {
                layer: Some(layer),
                area,
                context,
                kind,
                channel,
                date,
                domain_root,
                context_root: layer == "state"
                    && context.is_some()
                    && grab("rest").is_some_and(|rest| !rest.contains('/')),
                legacy: false,
            };
        }
        let mut parts = path.split('/');
        let first = parts.next().unwrap_or_default();
        let profile = vault_conventions::legacy();
        let bucket = |name| profile.bucket_exact(name) || profile.archive_bucket_exact(name);
        PathFields {
            legacy: bucket(first)
                || first == vault_conventions::LEGACY_DAILY_FOLDER
                || (self.legacy_domains.iter().any(|name| name == first)
                    && parts.next().is_some_and(bucket)),
            ..PathFields::default()
        }
    }
}

pub(super) fn leading_date(name: &str) -> Option<Date> {
    let value = name.get(..10)?;
    if name.as_bytes().get(10).is_some_and(|b| !b"-.".contains(b)) {
        return None;
    }
    super::value::date(value)
}

fn template_regex(layer: &str, template: &str) -> Result<Regex> {
    let mut pattern = String::from("^");
    let mut rest = template;
    while let Some((literal, field)) = rest.split_once('{') {
        let (field, tail) = field
            .split_once('}')
            .context("Unclosed layout placeholder")?;
        if layer == "state" && field == "context" && tail == "/{rest}" {
            ensure!(
                literal == "/",
                "Expected the state context after its domain"
            );
            // A vault may also have a note directly under the domain.
            pattern.push_str(r"/(?:(?P<context>[^/]+)/(?P<rest>.+)|(?P<domain_root>[^/]+))");
            rest = "";
            break;
        }
        pattern.push_str(&regex::escape(literal));
        if layer == "log" && field == "date" && tail == "-{slug}.md" {
            // Dated bundles and final archives keep the source member's name.
            pattern.push_str(r"(?P<entry>.+\.[mM][dD])");
            rest = "";
            break;
        }
        pattern.push_str(match field {
            "domain" => "(?P<domain>[^/]+)",
            "context" => "(?P<context>[^/]+)",
            "kind" => "(?P<kind>[^/]+)",
            "channel" => "(?P<channel>[^/]+)",
            "yyyy" => "(?P<yyyy>[0-9]{4})",
            "mm" => "(?P<mm>[0-9]{2})",
            "dd" => "(?P<dd>[0-9]{2})",
            "practice" | "slug" => "[^/]+",
            "date" => "[0-9]{4}-[0-9]{2}-[0-9]{2}",
            "rest" => "(?P<rest>.+)",
            _ => bail!("Unknown layout placeholder {field:?}"),
        });
        rest = tail;
    }
    pattern.push_str(&regex::escape(rest));
    pattern.push('$');
    Ok(Regex::new(&pattern)?)
}
