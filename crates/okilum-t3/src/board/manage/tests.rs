use super::*;
struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("okilum-manage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("ops")).unwrap();
        std::fs::create_dir(path.join("planning")).unwrap();
        std::fs::write(path.join("planning/stage.md"), "# Stage").unwrap();
        std::fs::write(path.join("ops/project.yaml"),"name: Example\nproduct: {outcome: A useful outcome}\nrepo: {host: git.oklabs.uk, full_name: BeFeast/example}\n").unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("ops").join(name)
    }
    fn session(&self) -> Session {
        Session::load(&self.0).unwrap()
    }
    fn create(&self) -> Session {
        let s = self.session();
        s.save(&Change::Details(s.details())).unwrap()
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn card(issue: &str) -> Card {
    Card {
        issue: issue.into(),
        expected: "Exact expected outcome".into(),
        executor: "Editing".into(),
        stage: String::new(),
    }
}

#[test]
fn create_add_edit_remove_roundtrip_is_readable_toml_without_legacy_migration() {
    let a = Area::new();
    let original = std::fs::read(a.file("project.yaml")).unwrap();
    let draft = a.session();
    assert!(draft.document.is_none());
    assert_eq!(draft.details().title, "Example");
    assert_eq!(draft.details().goal, "A useful outcome");
    assert_eq!(draft.details().stage_note, "planning/stage.md");
    let created = a.create();
    assert!(created.cards().is_empty());
    assert!(!a.file("board.yaml").exists());
    assert!(Binding::read(&a.0).unwrap().unwrap().issues.is_empty());
    let added = created
        .save(&Change::Add(card(
            "https://git.oklabs.uk/BeFeast/example/issues/42",
        )))
        .unwrap();
    assert_eq!(added.cards()[0]["number"], 42);
    assert!(std::fs::read_to_string(a.file("board.toml"))
        .unwrap()
        .contains("[[issues]]"));
    let mut edit = card("#42");
    edit.expected = "Reviewed outcome".into();
    edit.executor = "UI".into();
    edit.stage = "ready".into();
    let edited = added.save(&Change::Edit(0, edit)).unwrap();
    let binding = Binding::read(&a.0).unwrap().unwrap();
    assert_eq!(binding.issues[0].executor, "UI");
    assert_eq!(binding.issues[0].stage, Some(HumanStage::Ready));
    let removed = edited.save(&Change::Remove(0)).unwrap();
    assert!(removed.cards().is_empty());
    assert!(Binding::read(&a.0).unwrap().unwrap().issues.is_empty());
    let original_doc = edited.document.unwrap();
    removed
        .document
        .as_ref()
        .unwrap()
        .replace(&original_doc.source)
        .unwrap();
    assert_eq!(a.session().cards()[0]["number"], 42);
    assert_eq!(std::fs::read(a.file("project.yaml")).unwrap(), original);
}

#[test]
fn yaml_edits_stay_two_space_json_and_both_files_edit_only_reader_selection() {
    let a = Area::new();
    let s = a.create();
    let value = s.value;
    std::fs::remove_file(a.file("board.toml")).unwrap();
    std::fs::write(
        a.file("board.yaml"),
        serde_json::to_string_pretty(&value).unwrap() + "\n",
    )
    .unwrap();
    let legacy = a.session();
    assert_eq!(legacy.format_label(), "Editing board.yaml · JSON");
    let legacy = legacy.save(&Change::Add(card("1"))).unwrap();
    let bytes = std::fs::read_to_string(a.file("board.yaml")).unwrap();
    assert_eq!(
        bytes,
        serde_json::to_string_pretty(&legacy.value).unwrap() + "\n"
    );
    assert!(bytes.contains("\n  \"issues\": ["));
    assert!(!a.file("board.toml").exists());
    std::fs::write(
        a.file("board.toml"),
        toml::to_string_pretty(&value).unwrap(),
    )
    .unwrap();
    let preferred = a.session();
    assert_eq!(
        preferred.format_label(),
        "Editing board.toml · TOML; board.yaml is inactive"
    );
    let updated = preferred.save(&Change::Add(card("2"))).unwrap();
    assert_eq!(updated.cards()[0]["number"], 2);
    assert_eq!(
        std::fs::read_to_string(a.file("board.yaml")).unwrap(),
        bytes
    );
}

#[test]
fn changed_revision_or_format_or_project_refuses_writes_and_undo() {
    let a = Area::new();
    let created = a.create();
    let added = created.save(&Change::Add(card("1"))).unwrap();
    assert!(created.save(&Change::Add(card("2"))).is_err());
    assert!(created
        .document
        .as_ref()
        .unwrap()
        .replace("stale undo")
        .is_err());
    assert_eq!(a.session().cards().len(), 1);
    std::fs::write(a.file("project.yaml"), "name: Changed\n").unwrap();
    assert!(added.save(&Change::Remove(0)).is_err());
    assert_eq!(
        std::fs::read_to_string(a.file("board.toml")).unwrap(),
        added.document.unwrap().source
    );
    let b = Area::new();
    let draft = b.session();
    std::fs::write(b.file("board.yaml"), "human file").unwrap();
    assert!(draft.save(&Change::Details(draft.details())).is_err());
    assert!(!b.file("board.toml").exists());
}

#[test]
fn invalid_inputs_report_each_field_and_do_not_write_or_resolve_another_repository() {
    let a = Area::new();
    let s = a.create();
    let before = std::fs::read(a.file("board.toml")).unwrap();
    let bad = Card {
        issue: "https://git.oklabs.uk/Other/repo/issues/1".into(),
        expected: " ".into(),
        executor: "".into(),
        stage: "working".into(),
    };
    let error = s.save(&Change::Add(bad)).unwrap_err();
    let fields = &error.downcast_ref::<Problems>().unwrap().0;
    assert_eq!(
        fields.keys().cloned().collect::<Vec<_>>(),
        ["executor", "expected", "issue", "stage"]
    );
    assert_eq!(std::fs::read(a.file("board.toml")).unwrap(), before);
    let s = s.save(&Change::Add(card("1"))).unwrap();
    assert!(s.save(&Change::Add(card("1"))).is_err());
    for text in [
        "0",
        "-1",
        "https://user:pass@git.oklabs.uk/BeFeast/example/issues/1",
        "https://git.oklabs.uk/BeFeast/example/issues/1?x=2",
        "https://git.oklabs.uk/BeFeast/example/issues/1/extra",
    ] {
        assert!(
            issue_number(text, "https://git.oklabs.uk/BeFeast/example").is_err(),
            "{text}"
        );
    }
    let mut details = s.details();
    details.stage_note = "../outside.md".into();
    details.goal = "".into();
    let error = s.save(&Change::Details(details)).unwrap_err();
    let fields = &error.downcast_ref::<Problems>().unwrap().0;
    assert!(fields.contains_key("goal") && fields.contains_key("stage_note"));
}

#[test]
fn multiple_plans_require_a_visible_human_choice_and_toml_comments_survive_edits() {
    let a = Area::new();
    std::fs::write(a.0.join("planning/other.md"), "# Other").unwrap();
    let draft = a.session();
    assert_eq!(draft.stage_notes.len(), 2);
    assert!(draft.details().stage_note.is_empty());
    assert!(draft.save(&Change::Details(draft.details())).is_err());
    let mut details = draft.details();
    details.stage_note = "planning/stage.md".into();
    let s = draft.save(&Change::Details(details)).unwrap();
    let s = s.save(&Change::Add(card("1"))).unwrap();
    let s = s.save(&Change::Add(card("2"))).unwrap();
    let source = s
        .document
        .unwrap()
        .source
        .replace("number = 2", "# Keep this other card's comment\nnumber = 2")
        .replace(
            "title = \"Example\"",
            "# Human title comment\ntitle = \"Example\"",
        );
    std::fs::write(a.file("board.toml"), source).unwrap();
    let s = a.session();
    let mut c = s.card(0);
    c.expected = "Changed expected result".into();
    let s = s.save(&Change::Edit(0, c)).unwrap();
    let mut details = s.details();
    details.title = "Updated".into();
    let s = s.save(&Change::Details(details)).unwrap();
    let source = s.document.unwrap().source;
    assert!(source.contains("# Keep this other card's comment"));
    assert!(source.contains("# Human title comment"));
    assert_eq!(a.session().cards()[1]["number"], 2);
}

#[cfg(unix)]
#[test]
fn symlinked_stage_or_manifest_cannot_redirect_management_writes() {
    let a = Area::new();
    let outside = Area::new();
    std::os::unix::fs::symlink(
        outside.0.join("planning/stage.md"),
        a.0.join("planning/escape.md"),
    )
    .unwrap();
    let s = a.session();
    assert!(!s.stage_notes.contains(&"planning/escape.md".into()));
    let mut d = s.details();
    d.stage_note = "planning/escape.md".into();
    assert!(s.save(&Change::Details(d)).is_err());
    std::os::unix::fs::symlink(outside.file("project.yaml"), a.file("board.toml")).unwrap();
    assert!(Session::load(&a.0).is_err());
}
