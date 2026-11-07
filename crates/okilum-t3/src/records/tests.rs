use super::*;

struct Area(std::path::PathBuf);
impl Area {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("okilum-records-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn records_1892_project_scoped_read_only_discovery_keeps_conflicting_sources_and_exact_revision() {
    let area = Area::new();
    let first = "---\ntitle: Choose the release\nstatus: unresolved\n---\n# Original rationale\n\nDo not change this.\n";
    area.write("ops/decisions/2026-11-06-choice.md", first);
    area.write(
        "decisions/2026-11-07-other.md",
        "# Choose the release\nConflicting answer.\n",
    );
    area.write(
        "ops/daily/2026-11-07.md",
        "# Daily report\nSaved beta 123\n",
    );
    area.write(
        "handovers/2026-11-02-manager.md",
        "# Continuation context\nHistorical.\n",
    );
    area.write("notes/unrelated.md", "# Must not appear\n");
    area.write("nested/ops/decisions/2026-11-08.md", "# Another project\n");
    let before = std::fs::read(area.0.join("ops/decisions/2026-11-06-choice.md")).unwrap();
    let snapshot = read(&area.0, Kind::Decisions);
    assert!(snapshot.problems.is_empty(), "{:?}", snapshot.problems);
    assert_eq!(
        snapshot.records.len(),
        2,
        "same titles must not merge identities"
    );
    assert_eq!(snapshot.records[0].path, "decisions/2026-11-07-other.md");
    let record = &snapshot.records[1];
    assert_eq!(record.title, "Choose the release");
    assert_eq!(record.status.as_deref(), Some("unresolved"));
    assert_eq!(record.body, "# Original rationale\n\nDo not change this.\n");
    assert_eq!(
        record.revision,
        format!("{:x}", Sha256::digest(first.as_bytes()))
    );
    let draft = record.draft(Kind::Decisions);
    assert!(draft.contains(&record.path) && draft.contains(&record.revision));
    assert_eq!(std::fs::read(area.0.join(&record.path)).unwrap(), before);
    assert_eq!(
        read(&area.0, Kind::Reports).records[0].title,
        "Daily report"
    );
    assert_eq!(
        read(&area.0, Kind::Handovers).records[0].title,
        "Continuation context"
    );
    std::fs::remove_file(area.0.join(&record.path)).unwrap();
    assert_eq!(read(&area.0, Kind::Decisions).records.len(), 1);
}

#[test]
fn records_1892_bad_and_oversized_sources_are_visible_beside_valid_records() {
    let area = Area::new();
    area.write("ops/reports/good.md", "# Good report\n");
    area.write(
        "ops/reports/invalid.md",
        "---\ntitle: [broken\n---\n# Must not silently ignore metadata\n",
    );
    area.write("ops/reports/large.md", &"x".repeat(MAX_FILE + 1));
    let snapshot = read(&area.0, Kind::Reports);
    assert_eq!(
        snapshot.records.len(),
        1,
        "positive control: real source read"
    );
    assert_eq!(snapshot.records[0].title, "Good report");
    assert_eq!(snapshot.problems.len(), 2);
    assert!(snapshot
        .problems
        .iter()
        .any(|p| p.contains("large.md") && p.contains("not truncated")));
    assert!(snapshot.problems.iter().any(|p| p.contains("invalid.md")));
    assert!(read(&area.0, Kind::Handovers).records.is_empty());
}

#[cfg(unix)]
#[test]
fn records_1892_does_not_follow_file_directory_or_ops_symlinks() {
    use std::os::unix::fs::symlink;
    let outside = Area::new();
    outside.write("decisions/secret.md", "# Outside record\n");
    let area = Area::new();
    area.write("ops/decisions/real.md", "# Inside record\n");
    symlink(
        outside.0.join("decisions/secret.md"),
        area.0.join("ops/decisions/link.md"),
    )
    .unwrap();
    symlink(outside.0.join("decisions"), area.0.join("decisions")).unwrap();
    let snapshot = read(&area.0, Kind::Decisions);
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].title, "Inside record");
    assert_eq!(snapshot.problems.len(), 2);
    let other = Area::new();
    symlink(&outside.0, other.0.join("ops")).unwrap();
    let snapshot = read(&other.0, Kind::Decisions);
    assert!(snapshot.records.is_empty());
    assert!(snapshot.problems[0].contains("symlink"));
}
