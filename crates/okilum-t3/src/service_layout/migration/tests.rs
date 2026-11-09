use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    context: PathBuf,
    ids: Vec<String>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let context = temp.path().join("Areas/work-system");
        fs::create_dir_all(context.join("ops/daily")).unwrap();
        fs::create_dir(context.join(".okilum")).unwrap();
        fs::write(context.join(".okilum/vault.json"), b"existing settings").unwrap();
        fs::write(context.join("ops/project.yaml"), "# preserve this YAML and unknown fields\r\nname: Synthetic\r\nkind: knowledge\r\nfuture: {keep: true}\r\nt3:\r\n  schema_version: 1\r\n  environment: fixture\r\n  origin: https://t3.example\r\n  manager: {project_id: 'mcp:existing', thread_id: 'mcp:existing-manager', workspace_mode: local}\r\n").unwrap();
        fs::write(
            context.join("ops/board.toml"),
            "version=1\ntitle='Synthetic'\nissues=[]\n",
        )
        .unwrap();
        let ids = (1..=6)
            .map(|i| uuid::Uuid::from_u128(i).to_string())
            .collect::<Vec<_>>();
        let cards = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                format!("\n[[cards]]\nid='{id}'\ntitle='Synthetic card {i}'\nstate='todo'\n")
            })
            .collect::<String>();
        fs::write(
            context.join("ops/board-local.toml"),
            "# keep six identities\nversion=1\ncards=[]\n".replace("cards=[]\n", &cards),
        )
        .unwrap();
        fs::write(
            context.join("ops/daily/keep.md"),
            b"# ordinary knowledge; never relocate\r\n",
        )
        .unwrap();
        Self {
            _temp: temp,
            context,
            ids,
        }
    }
    fn assert_hidden(&self, plan: &Plan) {
        let layout = ServiceLayout::resolve(&self.context).unwrap();
        assert_eq!(layout.kind, Kind::Hidden);
        assert!(self.context.join(".okilum/project.yaml").is_file());
        assert!(!self.context.join(".okilum/project.toml").exists());
        for entry in &plan.files {
            same_bytes(&layout.path(&entry.name).unwrap(), entry).unwrap();
        }
        let cards = crate::board::local::read(&self.context).unwrap();
        assert_eq!(
            cards.cards.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            self.ids
        );
        assert_eq!(
            fs::read(self.context.join("ops/daily/keep.md")).unwrap(),
            b"# ordinary knowledge; never relocate\r\n"
        );
        assert_eq!(
            fs::read(self.context.join(".okilum/vault.json")).unwrap(),
            b"existing settings"
        );
    }
}

#[test]
fn reviewed_apply_keeps_yaml_six_ids_dates_and_notes_then_explicit_rollback() {
    let f = Fixture::new();
    let before = crate::area_files::read(&f.context, "project")
        .unwrap()
        .unwrap();
    let note_date = fs::metadata(f.context.join("ops/daily/keep.md"))
        .unwrap()
        .modified()
        .unwrap();
    let plan = Plan::preview(&f.context).unwrap();
    assert!(
        f.context.join("ops/project.yaml").is_file(),
        "preview cannot mutate"
    );
    assert!(
        plan.apply(false).is_err(),
        "compatibility/quiescence confirmation is explicit"
    );
    let receipt = plan.apply(true).unwrap();
    f.assert_hidden(&plan);
    for entry in &plan.files {
        let after = stamp(&f.context.join(".okilum").join(&entry.name), &entry.name).unwrap();
        assert_eq!(
            (
                after.modified_seconds,
                after.modified_nanos,
                after.unix_mode
            ),
            (
                entry.modified_seconds,
                entry.modified_nanos,
                entry.unix_mode
            )
        );
    }
    assert_eq!(
        note_date,
        fs::metadata(f.context.join("ops/daily/keep.md"))
            .unwrap()
            .modified()
            .unwrap()
    );
    assert!(!transaction(&f.context, &receipt.id)
        .unwrap()
        .starts_with(&f.context));
    let reverse = rollback_preview(&receipt).unwrap();
    reverse.apply(true).unwrap();
    assert_eq!(
        ServiceLayout::resolve(&f.context).unwrap().kind,
        Kind::Legacy
    );
    assert_eq!(
        fs::read(f.context.join("ops/project.yaml")).unwrap(),
        before.source.as_bytes()
    );
    assert!(
        before.replace("name: changed\n").is_err(),
        "even a round-trip location switch invalidates old editors"
    );
}

#[test]
fn stale_preview_new_sidecar_and_post_apply_edits_refuse_overwrite() {
    let f = Fixture::new();
    let plan = Plan::preview(&f.context).unwrap();
    fs::write(
        f.context.join("ops/board-context.toml"),
        "version=1\nnotes=[]\n",
    )
    .unwrap();
    assert!(plan.apply(true).is_err());
    let plan = Plan::preview(&f.context).unwrap();
    fs::write(f.context.join("ops/project.yaml"), "name: newer\n").unwrap();
    assert!(plan.apply(true).is_err());
    let receipt = Plan::preview(&f.context).unwrap().apply(true).unwrap();
    fs::write(
        f.context.join(".okilum/board-local.toml"),
        "version=1\ncards=[]\n",
    )
    .unwrap();
    assert!(rollback_preview(&receipt).is_err());
    assert!(
        fs::read_to_string(f.context.join(".okilum/board-local.toml"))
            .unwrap()
            .contains("cards=[]")
    );
}

#[test]
fn every_published_boundary_is_recoverable_without_mixing_layouts() {
    for stop in ["prepared", "staged", "published", "committed"] {
        let f = Fixture::new();
        let plan = Plan::preview(&f.context).unwrap();
        let mut fired = false;
        assert!(plan
            .apply_internal(true, |boundary| {
                if !fired && boundary == stop {
                    fired = true;
                    anyhow::bail!("synthetic crash at {stop}");
                }
                Ok(())
            })
            .is_err());
        assert!(fired, "control must exercise {stop}");
        assert!(
            ServiceLayout::resolve(&f.context).is_err(),
            "interrupted migration cannot look ready"
        );
        resume(&f.context, true).unwrap();
        f.assert_hidden(&plan);
    }
}

#[test]
fn interrupted_rollback_and_foreign_changes_are_preserved() {
    for stop in ["prepared", "staged", "published"] {
        let f = Fixture::new();
        let plan = Plan::preview(&f.context).unwrap();
        let mut fired = false;
        assert!(plan
            .apply_internal(true, |boundary| {
                if !fired && boundary == stop {
                    fired = true;
                    anyhow::bail!("crash");
                }
                Ok(())
            })
            .is_err());
        cancel_pending(&f.context, true).unwrap();
        assert_eq!(
            ServiceLayout::resolve(&f.context).unwrap().kind,
            Kind::Legacy
        );
        for entry in &plan.files {
            same_bytes(&f.context.join("ops").join(&entry.name), entry).unwrap();
        }
    }
    let f = Fixture::new();
    let plan = Plan::preview(&f.context).unwrap();
    assert!(plan
        .apply_internal(true, |boundary| {
            if boundary == "published" {
                anyhow::bail!("crash");
            }
            Ok(())
        })
        .is_err());
    let receipt = pending(&f.context).unwrap().unwrap();
    let moved = receipt
        .plan
        .files
        .iter()
        .find(|entry| f.context.join(".okilum").join(&entry.name).is_file())
        .unwrap();
    let changed = f.context.join(".okilum").join(&moved.name);
    fs::write(&changed, b"intervening external edit").unwrap();
    assert!(resume(&f.context, true).is_err());
    assert!(cancel_pending(&f.context, true).is_err());
    assert_eq!(fs::read(changed).unwrap(), b"intervening external edit");
    assert!(ServiceLayout::resolve(&f.context).is_err());
}

#[test]
fn relocated_first_writes_and_edits_use_selected_directory_and_keep_all_card_ids() {
    let f = Fixture::new();
    Plan::preview(&f.context).unwrap().apply(true).unwrap();
    let session = crate::board::local::Session::load(&f.context).unwrap();
    session
        .save(&crate::board::local::Change::Edit(
            f.ids[0].clone(),
            crate::board::local::Draft {
                title: "Updated synthetic card".into(),
                state: crate::board::local::State::Doing,
                ..Default::default()
            },
        ))
        .unwrap();
    let cards = crate::board::local::read(&f.context).unwrap();
    assert_eq!(
        cards.cards.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        f.ids
    );
    assert!(!f.context.join("ops/board-local.toml").exists());
    fs::remove_file(f.context.join(".okilum/board-local.toml")).unwrap();
    crate::board::local::Session::load(&f.context)
        .unwrap()
        .save(&crate::board::local::Change::Add(
            uuid::Uuid::from_u128(7).to_string(),
            crate::board::local::Draft {
                title: "First new sidecar".into(),
                ..Default::default()
            },
        ))
        .unwrap();
    assert!(f.context.join(".okilum/board-local.toml").is_file());
    assert!(!f.context.join("ops/board-local.toml").exists());
}
