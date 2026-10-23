use super::*;
use serde_json::json;

#[test]
fn monogram_letters_follow_the_words_of_the_name() {
    for (name, letters) in [
        ("Okilum", "O"),
        ("OK Folio", "OF"),
        ("ai-bills", "AB"),
        ("HomeLab", "HL"),
        ("kossoy finance tracker", "KF"),
        ("  grisaille ", "G"),
        ("3d-print", "3P"),
        ("музыка студия", "МС"),
        ("—", "?"),
        ("", "?"),
    ] {
        assert_eq!(monogram(name), letters, "{name:?}");
    }
}

#[test]
fn monogram_colour_is_stable_and_spread_over_the_palette() {
    // Pinned values: a change here re-colours every project for every user.
    assert_eq!(monogram_color("Okilum"), monogram_color(" okilum "));
    let pinned: Vec<_> = ["Okilum", "HomeLab", "Maestro", "OK Folio"]
        .into_iter()
        .map(|name| monogram_color(name).name())
        .collect();
    assert_eq!(pinned, expected_pinned_colours());
    for _ in 0..3 {
        assert_eq!(
            monogram_color("Maestro"),
            monogram_color("Maestro"),
            "deterministic"
        );
    }
    let used: std::collections::BTreeSet<_> = (0..200)
        .map(|i| monogram_color(&format!("project {i}")).name())
        .collect();
    assert!(used.len() >= 15, "spread over the 18 colours: {used:?}");
}

/// FNV-1a by hand for the four names, so the pin is checked against the
/// algorithm rather than against itself.
fn expected_pinned_colours() -> Vec<&'static str> {
    ["okilum", "homelab", "maestro", "ok folio"]
        .into_iter()
        .map(|key| {
            let mut hash: u32 = 0x811c9dc5;
            for unit in key.encode_utf16() {
                hash ^= unit as u32;
                hash = hash.wrapping_mul(0x01000193);
            }
            IconColor::ALL[(hash % 18) as usize].name()
        })
        .collect()
}

#[test]
fn colours_are_t3s_eighteen_in_t3s_order() {
    let names: Vec<_> = IconColor::ALL.iter().map(|c| c.name()).collect();
    assert_eq!(
        names,
        [
            "gray", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal", "cyan",
            "sky", "blue", "indigo", "violet", "purple", "fuchsia", "pink", "rose"
        ]
    );
    for color in IconColor::ALL {
        assert_eq!(
            serde_json::to_value(color).unwrap(),
            json!(color.name()),
            "serialised as T3 writes it"
        );
    }
    assert_eq!(IconColor::Emerald.label(), "Emerald");
}

#[test]
fn priority_is_manual_then_logo_then_monogram() {
    let manual = ProjectIcon::Emoji {
        emoji: "🚀".into()
    };
    let logo = Path::new("/vault/Dev/App/logo.svg");
    assert_eq!(
        resolve(Some(&manual), Some(logo), "App"),
        Avatar::Icon(manual.clone())
    );
    assert_eq!(
        resolve(None, Some(logo), "App"),
        Avatar::Logo(logo.to_owned())
    );
    assert_eq!(
        resolve(None, None, "ai-bills"),
        Avatar::Monogram {
            text: "AB".into(),
            color: monogram_color("ai-bills")
        }
    );
}

#[test]
fn t3_project_icon_round_trips_in_t3s_shape() {
    let cases = [
        (
            json!({"kind": "lucide", "name": "rocket", "color": "violet"}),
            ProjectIcon::Lucide {
                name: "rocket".into(),
                color: IconColor::Violet,
            },
        ),
        (
            json!({"kind": "emoji", "emoji": "🧠"}),
            ProjectIcon::Emoji {
                emoji: "🧠".into()
            },
        ),
        (
            json!({"kind": "monogram", "text": "OK", "color": "sky"}),
            ProjectIcon::Monogram {
                text: "OK".into(),
                color: IconColor::Sky,
            },
        ),
    ];
    for (wire, icon) in cases {
        assert_eq!(ProjectIcon::from_json(&wire), Some(icon.clone()));
        assert_eq!(serde_json::to_value(&icon).unwrap(), wire);
        let yaml: serde_yaml::Value = serde_yaml::from_str(&icon.to_yaml_flow()).unwrap();
        assert_eq!(ProjectIcon::from_yaml(&yaml), Some(icon), "flow YAML");
    }
    for bad in [
        json!(null),
        json!({"kind": "image", "url": "x.png"}),
        json!({"kind": "lucide", "name": "rocket", "color": "chartreuse"}),
        json!({"kind": "lucide", "name": "../x", "color": "red"}),
        json!({"kind": "monogram", "text": "ABC", "color": "red"}),
        json!({"kind": "monogram", "text": "", "color": "red"}),
        json!({"kind": "emoji", "emoji": "  "}),
    ] {
        assert_eq!(ProjectIcon::from_json(&bad), None, "{bad}");
    }
}

#[test]
fn a_t3_project_record_carries_icon_and_favicon_and_survives_unknown_kinds() {
    let project: crate::Project = serde_json::from_value(json!({
        "id": "44b5", "title": "Okilum", "workspaceRoot": "/home/god/src/okilum",
        "defaultModelSelection": null,
        "faviconPath": "crates/okilum-shell/assets/brand/app-icon-light.svg",
        "projectIcon": {"kind": "monogram", "text": "Ok", "color": "teal"},
        "scripts": [], "autoPull": false
    }))
    .unwrap();
    assert_eq!(
        project.favicon_path.as_deref(),
        Some("crates/okilum-shell/assets/brand/app-icon-light.svg")
    );
    assert_eq!(
        project.project_icon,
        Some(ProjectIcon::Monogram {
            text: "Ok".into(),
            color: IconColor::Teal
        })
    );
    // An older T3 has neither field; a newer one may have a new kind.
    let older: crate::Project = serde_json::from_value(
        json!({"id": "a", "title": "A", "workspaceRoot": "/a", "defaultModelSelection": null}),
    )
    .unwrap();
    assert_eq!((older.favicon_path, older.project_icon), (None, None));
    let newer: crate::Project = serde_json::from_value(json!({
        "id": "a", "title": "A", "workspaceRoot": "/a", "defaultModelSelection": null,
        "projectIcon": {"kind": "image", "assetId": "x"}
    }))
    .unwrap();
    assert_eq!(newer.project_icon, None);
}

const MANIFEST: &str = "# Identity manifest.\nname: App # display\nkind: code\nrepo:\n  full_name: BeFeast/app\nt3:\n  schema_version: 1\n  executor: {project_id: \"exec-1\"}\n";

#[test]
fn writing_the_icon_keeps_every_other_byte_and_field() {
    let rocket = ProjectIcon::Emoji {
        emoji: "🚀".into()
    };
    let added = write_manifest_icon(MANIFEST, Some(&rocket)).unwrap();
    assert_eq!(
        added,
        format!("{MANIFEST}icon: {{kind: emoji, emoji: \"🚀\"}}\n")
    );
    let teal = ProjectIcon::Lucide {
        name: "bot".into(),
        color: IconColor::Teal,
    };
    let replaced = write_manifest_icon(&added, Some(&teal)).unwrap();
    assert_eq!(
        replaced,
        format!("{MANIFEST}icon: {{kind: lucide, name: \"bot\", color: teal}}\n")
    );
    assert_eq!(write_manifest_icon(&replaced, None).unwrap(), MANIFEST);
    assert_eq!(write_manifest_icon(MANIFEST, None).unwrap(), MANIFEST);

    // A block-style icon in the middle, CRLF, and no final newline.
    let source = "name: App\r\nicon:\r\n  kind: monogram\r\n  text: AP\r\n  color: red\r\n\r\n# tail comment\r\nslug: app";
    let updated = write_manifest_icon(source, Some(&rocket)).unwrap();
    assert_eq!(
        updated,
        "name: App\r\nicon: {kind: emoji, emoji: \"🚀\"}\r\n\r\n# tail comment\r\nslug: app"
    );
    assert_eq!(
        write_manifest_icon(source, None).unwrap(),
        "name: App\r\n\r\n# tail comment\r\nslug: app"
    );
    let appended = write_manifest_icon("name: App", Some(&rocket)).unwrap();
    assert_eq!(appended, "name: App\nicon: {kind: emoji, emoji: \"🚀\"}\n");

    // Keys that only start with «icon» are not the icon.
    let lookalike = "icons: [a]\nicon_color: red\n";
    let written = write_manifest_icon(lookalike, Some(&rocket)).unwrap();
    assert!(written.starts_with(lookalike), "{written}");
    assert!(write_manifest_icon("- not\n- a mapping\n", Some(&rocket)).is_err());
}

#[test]
fn saving_reads_back_through_project_discovery() {
    let vault = std::env::temp_dir().join(format!("okilum-icon-{}", uuid::Uuid::new_v4()));
    let folder = vault.join("Dev/Areas/App");
    std::fs::create_dir_all(folder.join("ops")).unwrap();
    std::fs::write(folder.join("ops/project.yaml"), MANIFEST).unwrap();
    let project = super::super::Project::read(&folder).unwrap();
    assert_eq!(project.appearance.icon, None);
    assert_eq!(project.appearance.logo, None);
    assert_eq!(
        project.appearance.executor_project.as_deref(),
        Some("exec-1")
    );

    let icon = ProjectIcon::Monogram {
        text: "Ap".into(),
        color: IconColor::Rose,
    };
    save_manifest_icon(&folder, Some(&icon)).unwrap();
    std::fs::create_dir_all(folder.join("assets")).unwrap();
    std::fs::write(folder.join("assets/logo.svg"), "<svg/>").unwrap();
    let project = super::super::Project::read(&folder).unwrap();
    assert_eq!(project.appearance.icon, Some(icon));
    assert_eq!(
        project.appearance.logo,
        Some(folder.join("assets/logo.svg"))
    );
    assert_eq!(project.name, "App");
    save_manifest_icon(&folder, None).unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.join("ops/project.yaml")).unwrap(),
        MANIFEST
    );
    let leftovers = std::fs::read_dir(folder.join("ops")).unwrap().count();
    assert_eq!(leftovers, 1, "no temporary file stays behind");
    // A broken icon never hides the project.
    std::fs::write(
        folder.join("ops/project.yaml"),
        format!("{MANIFEST}icon: {{kind: hologram}}\n"),
    )
    .unwrap();
    assert_eq!(
        super::super::Project::read(&folder)
            .unwrap()
            .appearance
            .icon,
        None
    );
    std::fs::remove_dir_all(vault).unwrap();
}

#[test]
fn the_t3_counterpart_is_the_executor_project_or_the_same_vault_folder() {
    let vault = std::env::temp_dir().join(format!("okilum-icon-{}", uuid::Uuid::new_v4()));
    let folder = vault.join("HomeLab");
    std::fs::create_dir_all(folder.join("Resources/Images")).unwrap();
    std::fs::write(folder.join("Resources/Images/homelab.svg"), "<svg/>").unwrap();
    let t3 = |id: &str, root: &str, favicon: Option<&str>| crate::Project {
        id: id.into(),
        title: id.into(),
        workspace_root: root.into(),
        favicon_path: favicon.map(str::to_owned),
        ..Default::default()
    };
    let listing = vec![
        t3("brain", "/home/god/Obsidian Vault", None),
        t3(
            "homelab",
            "/home/god/Obsidian Vault/HomeLab/",
            Some("Resources/Images/homelab.svg"),
        ),
        t3(
            "okilum",
            "/home/god/src/okilum",
            Some("crates/okilum-shell/assets/brand/app-icon-light.svg"),
        ),
    ];
    let project = |executor: Option<&str>| super::super::Project {
        folder: folder.clone(),
        name: "HomeLab".into(),
        kind: super::super::Kind::Knowledge,
        environment: Some("maestro".into()),
        manager_project: Some("brain".into()),
        manager: super::super::Manager::NotStarted,
        appearance: super::super::Appearance {
            executor_project: executor.map(str::to_owned),
            ..Default::default()
        },
    };
    // The shared manager project (Brain) is never this project's icon.
    let found = t3_counterpart(&project(None), "HomeLab", &listing).unwrap();
    assert_eq!(found.id, "homelab");
    assert_eq!(
        t3_logo(&folder, "HomeLab", found),
        Some(folder.join("Resources/Images/homelab.svg"))
    );
    let found = t3_counterpart(&project(Some("okilum")), "HomeLab", &listing).unwrap();
    assert_eq!(found.id, "okilum");
    // A repository root on the server is not a file here.
    assert_eq!(t3_logo(&folder, "HomeLab", found), None);
    assert!(t3_counterpart(&project(None), "Music/Studio", &listing).is_none());
    assert!(
        !same_folder("/x/MyHomeLab", "HomeLab"),
        "whole segments only"
    );
    let escaping = t3("homelab", "/v/HomeLab", Some("../secret.svg"));
    assert_eq!(t3_logo(&folder, "HomeLab", &escaping), None);
    std::fs::remove_dir_all(vault).unwrap();
}

#[test]
fn dialog_state_opens_on_the_current_icon_and_saves_the_current_tab() {
    let draft = IconDraft::new(None, "ai-bills");
    assert_eq!(draft.tab, PickerTab::Icons);
    assert_eq!(draft.monogram, "AB");
    assert_eq!(draft.color, monogram_color("ai-bills"));
    assert_eq!(
        draft.result(),
        Some(ProjectIcon::Lucide {
            name: DEFAULT_LUCIDE.into(),
            color: monogram_color("ai-bills")
        })
    );

    let current = ProjectIcon::Monogram {
        text: "Ok".into(),
        color: IconColor::Amber,
    };
    let mut draft = IconDraft::new(Some(&current), "Okilum");
    assert_eq!(draft.tab, PickerTab::Monogram);
    assert_eq!(draft.result(), Some(current));
    draft.set_monogram("x y z");
    assert_eq!(draft.monogram, "xy");
    draft.set_monogram("");
    assert_eq!(draft.result(), None, "an empty monogram cannot be saved");
    draft.tab = PickerTab::Icons;
    draft.color = IconColor::Lime;
    draft.lucide = "bot".into();
    assert_eq!(
        draft.result(),
        Some(ProjectIcon::Lucide {
            name: "bot".into(),
            color: IconColor::Lime
        })
    );

    let mut draft = IconDraft::new(
        Some(&ProjectIcon::Emoji {
            emoji: "🧠".into()
        }),
        "Brain",
    );
    assert_eq!(draft.tab, PickerTab::Emoji);
    assert!(!draft.paste_emoji("plain text"));
    assert_eq!(draft.emoji, "🧠");
    assert!(draft.paste_emoji(" 👩🏽‍💻 and more"));
    assert_eq!(draft.emoji, "👩🏽‍💻");
    assert_eq!(
        draft.result(),
        Some(ProjectIcon::Emoji {
            emoji: "👩🏽‍💻".into()
        })
    );
}

#[test]
fn first_emoji_takes_whole_sequences_only() {
    for (input, emoji) in [
        ("🚀🚀", Some("🚀")),
        ("🛠️ tools", Some("🛠️")),
        ("🇮🇱", Some("🇮🇱")),
        ("1️⃣", Some("1️⃣")),
        ("👨‍👩‍👧", Some("👨‍👩‍👧")),
        ("1", None),
        ("abc", None),
        ("", None),
    ] {
        assert_eq!(first_emoji(input).as_deref(), emoji, "{input:?}");
    }
}

#[test]
fn icon_search_shows_popular_first_and_matches_dashed_names() {
    let available: Vec<String> = ["zap", "bot", "folder", "book-open", "file-text", "rocket"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        filter_icons(&available, ""),
        ["bot", "book-open", "file-text", "folder", "rocket", "zap"]
    );
    assert_eq!(filter_icons(&available, "Book Op"), ["book-open"]);
    assert_eq!(
        filter_icons(&available, "o"),
        ["book-open", "bot", "folder", "rocket"]
    );
    assert!(filter_icons(&available, "nothing").is_empty());
}
