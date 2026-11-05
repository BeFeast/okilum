//! Provider marks mirror the configured driver, while instance badges make
//! separate accounts distinguishable without a wide provider-name column.
use super::*;
use okilum_t3::ProviderCapability;

// T3's Claude mark explicitly uses this brand foreground. The other mask
// glyphs inherit the theme; multi-color marks use an image instead of a mask.
const CLAUDE_BRAND: u32 = 0xD97757;
const ANTIGRAVITY_COLOR: &str = "icons/providers/antigravity.png";

/// Button's built-in icon/label content centers itself. Give every rail row
/// one full-width content row so icons and names share the same left edge.
pub(super) fn rail_label(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    named: bool,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .debug_selector(move || format!("project-model-rail-icon-{id}"))
                .child(Icon::new(icon).size(px(18.))),
        )
        .when(named, |row| {
            row.child(
                div()
                    .debug_selector(move || format!("project-model-rail-label-{id}"))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(crate::project::design::LABEL))
                    .line_height(px(crate::project::design::LINE))
                    .child(label),
            )
        })
}

pub(super) fn provider_glyph(driver: &str) -> Icon {
    let glyph = if let Some(path) = provider_asset(driver) {
        Icon::default().path(path)
    } else {
        Icon::new(IconName::SquareTerminal)
    };
    if driver == "claudeAgent" {
        glyph.text_color(gpui::rgb(CLAUDE_BRAND))
    } else {
        glyph
    }
}

pub(super) fn provider_visual(driver: &str, size: Pixels) -> AnyElement {
    if driver == "antigravity" {
        // img's embedded resource path retains the source RGBA channels. An
        // Icon/Svg would use those pixels as a single-color foreground mask.
        gpui::img(ANTIGRAVITY_COLOR)
            .size(size)
            .flex_shrink_0()
            .into_any_element()
    } else {
        provider_glyph(driver).size(size).into_any_element()
    }
}

fn provider_asset(driver: &str) -> Option<&'static str> {
    match driver {
        "codex" => Some("icons/providers/openai.svg"),
        "claudeAgent" => Some("icons/providers/claude.svg"),
        "grok" => Some("icons/providers/grok.svg"),
        "cursor" => Some("icons/providers/cursor.svg"),
        "opencode" => Some("icons/providers/opencode.svg"),
        "antigravity" => Some("icons/providers/antigravity.svg"),
        _ => None,
    }
}

fn initials(label: &str) -> String {
    let words = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let characters = if words.len() > 1 {
        words
            .iter()
            .take(2)
            .filter_map(|word| word.chars().next())
            .collect::<Vec<_>>()
    } else {
        label
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .collect()
    };
    characters
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect()
}

pub(super) fn instance_badge(
    provider: &ProviderCapability,
    providers: &[ProviderCapability],
) -> Option<String> {
    let siblings = providers
        .iter()
        .filter(|candidate| candidate.driver_kind == provider.driver_kind)
        .collect::<Vec<_>>();
    if siblings.len() < 2 {
        return None;
    }
    let label = |provider: &ProviderCapability| {
        provider
            .display_name
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or(&provider.provider_instance_id)
            .to_owned()
    };
    let badge = initials(&label(provider));
    if !badge.is_empty()
        && siblings
            .iter()
            .filter(|candidate| initials(&label(candidate)) == badge)
            .count()
            == 1
    {
        return Some(badge);
    }
    // Equal display names still need separate, visible account identities.
    siblings
        .iter()
        .position(|candidate| candidate.provider_instance_id == provider.provider_instance_id)
        .map(|index| (index + 1).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::AssetSource as _;

    fn provider(id: &str, name: &str) -> ProviderCapability {
        ProviderCapability {
            provider_instance_id: id.into(),
            driver_kind: "claudeAgent".into(),
            display_name: Some(name.into()),
            models: vec![],
            runtime_modes: vec![],
        }
    }

    #[test]
    fn configured_accounts_have_distinct_badges_even_with_equal_labels() {
        let providers = vec![
            provider("default", "Claude"),
            provider("relay", "Claude → Astra"),
        ];
        assert_eq!(
            instance_badge(&providers[0], &providers).as_deref(),
            Some("CL")
        );
        assert_eq!(
            instance_badge(&providers[1], &providers).as_deref(),
            Some("CA")
        );
        let equal = vec![provider("work", "Claude"), provider("personal", "Claude")];
        assert_eq!(instance_badge(&equal[0], &equal).as_deref(), Some("1"));
        assert_eq!(instance_badge(&equal[1], &equal).as_deref(), Some("2"));
        assert!(instance_badge(&equal[0], &equal[..1]).is_none());
    }

    #[test]
    fn provider_glyphs_are_distinct_embedded_assets() {
        let drivers = [
            "codex",
            "claudeAgent",
            "grok",
            "cursor",
            "opencode",
            "antigravity",
        ];
        let paths = drivers
            .iter()
            .map(|driver| provider_asset(driver).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), drivers.len());
        for path in paths {
            assert!(
                brand::Assets.load(path).unwrap().is_some(),
                "missing provider glyph {path}"
            );
        }
        assert!(provider_asset("an-unrecognized-driver").is_none());
    }

    #[test]
    fn claude_uses_the_upstream_brand_color_and_antigravity_retains_original_rgba() {
        assert_eq!(CLAUDE_BRAND, 0xD97757);
        let claude = brand::Assets
            .load("icons/providers/claude.svg")
            .unwrap()
            .unwrap();
        assert!(std::str::from_utf8(&claude)
            .unwrap()
            .contains("fill=\"#D97757\""));
        let original = brand::Assets
            .load("icons/providers/antigravity.svg")
            .unwrap()
            .unwrap();
        let original = std::str::from_utf8(&original).unwrap();
        let encoded = original
            .split("data:image/png;base64,")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        use base64::Engine as _;
        let original = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let color = brand::Assets.load(ANTIGRAVITY_COLOR).unwrap().unwrap();
        assert_eq!(
            color.as_ref(),
            original,
            "full-color asset is the original upstream PNG without recoloring"
        );
        let rgba = image::load_from_memory(&color).unwrap().to_rgba8();
        assert_eq!(rgba.dimensions(), (128, 128));
        let colored = rgba
            .pixels()
            .filter(|pixel| {
                pixel[3] > 0
                    && pixel[0]
                        .max(pixel[1])
                        .max(pixel[2])
                        .saturating_sub(pixel[0].min(pixel[1]).min(pixel[2]))
                        > 32
            })
            .count();
        assert!(
            colored > 128,
            "positive control: source contains distinct RGB channels, not a monochrome mask"
        );
    }
}
