use super::*;
struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("okilum-local-cards-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("ops")).unwrap();
        std::fs::write(path.join("ops/project.toml"), "name='Local'\nkind='code'\n").unwrap();
        std::fs::write(
            path.join("ops/board.toml"),
            "version=1\ntitle='Local'\nissues=[]\n",
        )
        .unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("ops/board-local.toml")
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn draft(title: &str) -> Draft {
    Draft {
        title: title.into(),
        ..Default::default()
    }
}
#[test]
fn title_only_local_cards_keep_identity_edit_remove_and_undo_without_touching_issue_manifests() {
    let area = Area::new();
    let project = std::fs::read(area.0.join("ops/project.toml")).unwrap();
    let board = std::fs::read(area.0.join("ops/board.toml")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let created = Session::load(&area.0)
        .unwrap()
        .save(&Change::Add(id.clone(), draft("Take a note")))
        .unwrap();
    let card = &read(&area.0).unwrap().cards[0];
    assert_eq!(card.id, id);
    assert!(card.details.is_empty());
    assert_eq!(card.state, State::Todo);
    let mut edit = draft("Keep my local work");
    edit.details = "The issue number #42 here is ordinary text.".into();
    edit.state = State::Done;
    let edited = created.save(&Change::Edit(id.clone(), edit)).unwrap();
    assert_eq!(edited.projection.cards[0].id, id);
    assert_eq!(edited.projection.cards[0].state.column(), Column::Ready);
    let previous = edited.document.as_ref().unwrap().source.clone();
    let removed = edited.save(&Change::Remove(id)).unwrap();
    assert!(removed.projection.cards.is_empty());
    let restored = removed.undo(&previous).unwrap();
    assert_eq!(restored.projection.cards, edited.projection.cards);
    assert_eq!(
        std::fs::read(area.0.join("ops/project.toml")).unwrap(),
        project
    );
    assert_eq!(std::fs::read(area.0.join("ops/board.toml")).unwrap(), board);
    assert!(
        super::super::read_issues(&super::super::Binding::read(&area.0).unwrap().unwrap())
            .is_empty()
    );
}
#[test]
fn external_edit_duplicate_id_and_changed_owner_refuse_stale_writes_without_losing_sources() {
    let area = Area::new();
    let id = uuid::Uuid::new_v4().to_string();
    let s = Session::load(&area.0).unwrap();
    assert!(s.save(&Change::Add(id.clone(), draft(" \n"))).is_err());
    assert!(!area.path().exists());
    let current = s.save(&Change::Add(id.clone(), draft("Original"))).unwrap();
    let old = current.document.as_ref().unwrap().source.clone();
    assert!(current
        .save(&Change::Add(id.clone(), draft("Duplicate")))
        .is_err());
    std::fs::write(area.path(), format!("{old}\n# External edit\n")).unwrap();
    let external = std::fs::read(area.path()).unwrap();
    assert!(current
        .save(&Change::Edit(id.clone(), draft("Stale")))
        .is_err());
    assert!(current.undo(&old).is_err());
    assert_eq!(std::fs::read(area.path()).unwrap(), external);
    let current = Session::load(&area.0).unwrap();
    std::fs::write(
        area.0.join("ops/board.toml"),
        "version=1\ntitle='Changed Board'\nissues=[]\n",
    )
    .unwrap();
    assert!(current
        .save(&Change::Remove(id))
        .unwrap_err()
        .to_string()
        .contains("board changed"));
    assert_eq!(std::fs::read(area.path()).unwrap(), external);
}
#[test]
fn local_sidecar_preserves_other_cards_comments_and_invalid_cards_beside_healthy_ones() {
    let area = Area::new();
    let id = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    for inline in [false, true] {
        let source = if inline {
            format!("# Local work\nversion=1\ncards=[\n {{ id='{id}', title='Edit me' }},\n # Other card stays\n {{ id='{other}', title='Do not rewrite me', details='Keep this spacing' }},\n {{ id='bad-id', title='Invalid identity' }},\n]\n")
        } else {
            format!("# Local work\nversion=1\n[[cards]]\nid='{id}'\ntitle='Edit me' # Title comment\n[[cards]] # Other card stays\nid='{other}'\ntitle='Do not rewrite me'\ndetails='Keep this spacing'\n[[cards]]\nid='bad-id'\ntitle='Invalid identity'\n")
        };
        std::fs::write(area.path(), &source).unwrap();
        let session = Session::load(&area.0).unwrap();
        assert_eq!(session.projection.cards.len(), 2);
        assert_eq!(session.projection.problems.len(), 1);
        let updated = session
            .save(&Change::Edit(id.clone(), draft("Edited")))
            .unwrap();
        let text = updated.document.as_ref().unwrap().source.as_str();
        assert!(text.contains("# Local work") && text.contains("# Other card stays"));
        assert!(
            text.contains("title='Do not rewrite me'")
                || text.contains("title='Do not rewrite me',")
        );
        assert!(text.contains("details='Keep this spacing'"));
        assert_eq!(updated.projection.cards[0].id, id);
        assert_eq!(updated.projection.cards[1].id, other);
        assert_eq!(updated.projection.problems.len(), 1);
    }
    let source =
        format!("version=1\ncards=[{{id='{id}',title='First'}},{{id='{id}',title='Second'}}]\n");
    std::fs::write(area.path(), &source).unwrap();
    let projection = read(&area.0).unwrap();
    assert!(projection.cards.is_empty());
    assert_eq!(projection.problems.len(), 2);
    assert!(Session::load(&area.0)
        .unwrap()
        .save(&Change::Remove(id))
        .is_err());
    assert_eq!(std::fs::read_to_string(area.path()).unwrap(), source);
}
#[cfg(unix)]
#[test]
fn local_sidecar_symlinks_are_refused_without_touching_the_target() {
    let area = Area::new();
    let outside = Area::new();
    std::fs::write(outside.path(), "version=1\ncards=[]\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), area.path()).unwrap();
    assert!(read(&area.0).is_err());
    assert!(Session::load(&area.0).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path()).unwrap(),
        "version=1\ncards=[]\n"
    );
}

#[test]
fn sidecar_reads_are_read_only_and_format_replacement_or_project_change_refuses_stale_writes() {
    let area = Area::new();
    assert!(read(&area.0).unwrap().cards.is_empty());
    assert!(!area.path().exists(), "readers never create the sidecar");
    let id = uuid::Uuid::new_v4().to_string();
    let session = Session::load(&area.0)
        .unwrap()
        .save(&Change::Add(id.clone(), draft("Original")))
        .unwrap();
    let before = std::fs::read(area.path()).unwrap();
    let stamp = std::fs::metadata(area.path()).unwrap().modified().unwrap();
    assert_eq!(read(&area.0).unwrap().cards.len(), 1);
    assert_eq!(std::fs::read(area.path()).unwrap(), before);
    assert_eq!(
        std::fs::metadata(area.path()).unwrap().modified().unwrap(),
        stamp
    );
    std::fs::write(area.0.join("ops/project.yaml"), "name: Inactive legacy\n").unwrap();
    let project_path = area.0.join("ops/project.toml");
    std::fs::rename(&project_path, area.0.join("ops/project-backup.toml")).unwrap();
    assert!(session
        .save(&Change::Remove(id))
        .unwrap_err()
        .to_string()
        .contains("project changed"));
    assert_eq!(std::fs::read(area.path()).unwrap(), before);
}
