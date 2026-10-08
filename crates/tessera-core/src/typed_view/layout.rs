//! Declarative Tasks presentation; canonical Markdown is never rewritten.
use super::TasksSection;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Compact,
    Comfortable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Grouping {
    /// Preserve the grouping authored in each Tasks query.
    #[default]
    Query,
    Note,
    None,
}

/// App preferences can supply defaults without modifying the note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub density: Density,
    pub grouping: Grouping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub density: Density,
    pub grouping: Grouping,
    /// Permutation of the original Tasks sections. Source identities stay intact.
    pub order: Vec<usize>,
}

#[derive(Debug)]
pub struct Dashboard {
    pub sections: Vec<TasksSection>,
    pub layout: Layout,
}

/// After selecting the Tasks view, parse its queries and layout from the same
/// canonical snapshot. Both stages fail back to Markdown without partial views.
pub fn parse(source: &str, today: time::Date, defaults: Defaults) -> Result<Dashboard, String> {
    let sections = super::tasks_sections(source, today)?;
    let layout = resolve(source, &sections, defaults)?;
    Ok(Dashboard { sections, layout })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionRef {
    heading: String,
    occurrence: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    density: Option<Density>,
    grouping: Option<Grouping>,
    #[serde(default)]
    order: Vec<SectionRef>,
}

/// Call after typed-view selection and Tasks-section validation. An error is a
/// Markdown fallback reason, never permission to silently ignore bad settings.
fn resolve(source: &str, sections: &[TasksSection], defaults: Defaults) -> Result<Layout, String> {
    let overrides = match crate::properties::frontmatter_block(source) {
        None => Overrides::default(),
        Some(yaml) => {
            let frontmatter: serde_yaml::Mapping = serde_yaml::from_str(yaml)
                .map_err(|_| "The note's properties could not be read.")?;
            match frontmatter.get("tasks_view") {
                None => Overrides::default(),
                Some(value) => {
                    let fields = value
                        .as_mapping()
                        .ok_or("The Tasks layout must be a mapping. Showing Markdown.")?;
                    if ["density", "grouping"]
                        .iter()
                        .any(|key| fields.get(*key).is_some_and(|value| !value.is_string()))
                    {
                        return Err("The Tasks layout is invalid. Showing Markdown.".into());
                    }
                    if let Some(order) = fields.get("order") {
                        let order = order
                            .as_sequence()
                            .ok_or("Tasks section order must be a list. Showing Markdown.")?;
                        for entry in order {
                            let entry = entry
                                .as_mapping()
                                .ok_or("A Tasks section reference is invalid. Showing Markdown.")?;
                            if !entry
                                .get("heading")
                                .is_some_and(serde_yaml::Value::is_string)
                                || entry
                                    .get("occurrence")
                                    .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
                            {
                                return Err(
                                    "A Tasks section reference is invalid. Showing Markdown."
                                        .into(),
                                );
                            }
                        }
                    }
                    serde_yaml::from_value::<Overrides>(value.clone())
                        .map_err(|_| "The Tasks layout is invalid. Showing Markdown.")?
                }
            }
        }
    };
    let mut order = Vec::with_capacity(sections.len());
    for reference in overrides.order {
        let matches: Vec<_> = sections
            .iter()
            .enumerate()
            .filter(|(_, section)| section.title.as_deref() == Some(reference.heading.as_str()))
            .map(|(index, _)| index)
            .collect();
        let index = match reference.occurrence {
            Some(n) => n.checked_sub(1).and_then(|n| matches.get(n)).copied(),
            None if matches.len() == 1 => Some(matches[0]),
            None => None,
        }
        .ok_or("A Tasks section reference is missing or ambiguous. Showing Markdown.")?;
        if order.contains(&index) {
            return Err("A Tasks section is ordered more than once. Showing Markdown.".into());
        }
        order.push(index);
    }
    for index in 0..sections.len() {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    Ok(Layout {
        density: overrides.density.unwrap_or(defaults.density),
        grouping: overrides.grouping.unwrap_or(defaults.grouping),
        order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const BODY: &str = "# Today\n```tasks\nnot done\n```\n# Later\n```tasks\ndone\n```\n# Today\n```tasks\nnot done\n```\n";
    fn sections() -> Vec<TasksSection> {
        super::super::tasks_sections(BODY, time::macros::date!(2026 - 10 - 07)).unwrap()
    }
    fn source(layout: &str) -> String {
        format!("---\nview: tasks\ntasks_view: {layout}\n---\n{BODY}")
    }
    #[test]
    fn defaults_and_partial_order_keep_every_source_section() {
        let sections = sections();
        let defaults = Defaults {
            density: Density::Comfortable,
            grouping: Grouping::Note,
        };
        let normal = resolve(BODY, &sections, defaults).unwrap();
        assert_eq!(normal.order, [0, 1, 2]);
        assert_eq!(normal.density, Density::Comfortable);
        assert_eq!(normal.grouping, Grouping::Note);
        let overridden = source("{density: compact, order: [{heading: Today, occurrence: 2}]}");
        let layout = resolve(&overridden, &sections, defaults).unwrap();
        assert_eq!(layout.order, [2, 0, 1]);
        assert_eq!(layout.density, Density::Compact);
        assert_eq!(layout.grouping, Grouping::Note);
        assert_eq!(sections[0].source_line, 2);
        assert_eq!(sections[2].source_line, 10);
        let layout = resolve(&source("{grouping: none}"), &sections, defaults).unwrap();
        assert_eq!(layout.grouping, Grouping::None);
    }
    #[test]
    fn invalid_layouts_fall_back_instead_of_guessing_or_dropping_sections() {
        let sections = sections();
        for layout in [
            "null",
            "[]",
            "{density: null}",
            "{grouping: null}",
            "{density: dense}",
            "{grouping: path}",
            "{densitty: compact}",
            "{order: null}",
            "{order: [Today]}",
            "{order: [[Later, 1]]}",
            "{order: [{heading: 123}]}",
            "{order: [{heading: Later, occurrence: null}]}",
            "{order: [{heading: Missing}]}",
            "{order: [{heading: Today}]}",
            "{order: [{heading: Today, occurrence: 0}]}",
            "{order: [{heading: Today, occurrence: 3}]}",
            "{order: [{heading: Later}, {heading: Later, occurrence: 1}]}",
            "{order: [{heading: Later, extra: true}]}",
        ] {
            assert!(
                resolve(&source(layout), &sections, Defaults::default()).is_err(),
                "{layout}"
            );
        }
    }

    #[test]
    fn dashboard_parses_one_snapshot_and_preserves_unicode_source_lines() {
        let source = "---\r\nview: tasks\r\ntasks_view:\r\n  order: [{heading: Завтра}]\r\n---\r\n# Сегодня\r\n```tasks\r\nnot done\r\n```\r\n# Завтра\r\n```tasks\r\ndone\r\n```\r\n";
        let today = time::macros::date!(2026 - 10 - 07);
        let dashboard = parse(source, today, Defaults::default()).unwrap();
        assert_eq!(dashboard.layout.order, [1, 0]);
        assert_eq!(dashboard.sections[0].source_line, 7);
        assert_eq!(dashboard.sections[1].source_line, 11);
        assert!(parse(
            &source.replace("not done", "unknown query"),
            today,
            Defaults::default()
        )
        .is_err());
        assert!(parse(
            "---\nview: tasks\n---\nNo sections",
            today,
            Defaults::default()
        )
        .is_err());
    }
}
