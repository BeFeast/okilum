//! Pure, metadata-only G04/G05 presentation planner, independent of rendering.
//!
//! The adapter supplies canonical source RowKeys and v2 run identities, never
//! providerTurnId/attempt identities. Rows contain no authored text, attachment
//! bytes or request capabilities. Every emitted member still resolves through
//! Projection; grouping cannot change canonical data or authorize a request.
//!
//! Parse canonical timestamps before calling this module (invalid/missing =>
//! None). v2 assistant rows have startedAt/completedAt/updatedAt, not necessarily
//! createdAt. Do not substitute the local clock or a first-observed timestamp.
//! Unknown row kinds must be Barrier::Unknown, not guessed to be work.
use super::RowKey;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Source thread matters for inherited history and independently named runs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunKey {
    pub thread: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
    Unknown,
}
impl RunState {
    pub fn unsettled(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Queued | Self::Starting | Self::Running | Self::Waiting
        )
    }
    fn foldable(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Interrupted | Self::Failed | Self::Cancelled
        )
    }
}

/// Epoch milliseconds from validated source timestamps; no guessed times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunFact {
    pub key: RunKey,
    pub state: RunState,
    pub requested_at: Option<i64>,
    pub started_at: Option<i64>,
    pub work_started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub restart_continuation_of: Option<RunKey>,
}
impl RunFact {
    /// Audited v2 orchestrationV2RunWorkStartedAt precedence.
    pub fn work_start(&self) -> Option<i64> {
        self.work_started_at
            .or(self.started_at)
            .or(self.requested_at)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkStatus {
    Unspecified,
    InProgress,
    Completed,
    Failed,
    Declined,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// Normalized (itemType,label) identity; scope includes source/run already.
    StatuslessIdlessMarker(String),
    Terminal(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkFact {
    pub tool_like: bool,
    pub tool_call_id: Option<String>,
    pub status: WorkStatus,
    /// Rendered output/exit-code evidence only, never authored command input.
    pub output_failed: bool,
    pub neutral: bool,
    pub task_progress: bool,
    pub lifecycle: Option<Lifecycle>,
}
impl WorkFact {
    pub fn failed(&self) -> bool {
        self.output_failed || matches!(self.status, WorkStatus::Failed | WorkStatus::Declined)
    }
    fn active_candidate(&self) -> bool {
        self.status == WorkStatus::InProgress
            || (self.status == WorkStatus::Unspecified && (self.tool_like || self.task_progress))
    }
}

/// These rows stay visible and break ordinary work groups, even inside a fold.
/// Requests are conservatively protected regardless of displayed item status;
/// runtime request authority stays with actions::State, outside this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    Plan,
    Request,
    SevereFailure,
    AgentSpawn,
    Notice,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    User,
    Assistant,
    Work(WorkFact),
    Compaction,
    Barrier(Barrier),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub key: RowKey,
    pub run: Option<RunKey>,
    pub kind: RowKind,
    pub streaming: bool,
    /// Canonical user createdAt or explicit item start time, if available.
    pub started_at: Option<i64>,
    /// Canonical item updatedAt/completedAt chosen explicitly by the adapter.
    pub ended_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GroupIdentity {
    Tool(String),
    Entry(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub thread: String,
    pub run: Option<RunKey>,
    pub identity: GroupIdentity,
}
impl GroupKey {
    fn for_row(row: &SourceRow) -> Self {
        let identity = match &row.kind {
            RowKind::Work(work) => work
                .tool_call_id
                .as_ref()
                .map(|id| GroupIdentity::Tool(id.clone())),
            _ => None,
        }
        .unwrap_or_else(|| GroupIdentity::Entry(row.key.item.clone()));
        Self {
            thread: row.key.thread.clone(),
            run: row.run.clone(),
            identity,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Expansion {
    pub groups: HashSet<GroupKey>,
    pub runs: HashSet<RunKey>,
}

/// Ephemeral Stop transition memory. Default after reload intentionally folds
/// an already interrupted run. Call observe_latest only on lifecycle updates.
#[derive(Clone, Debug, Default)]
pub struct Session {
    previous_latest: Option<(RunKey, RunState)>,
    interrupted_open: HashSet<RunKey>,
}
impl Session {
    pub fn observe_latest(&mut self, latest: Option<&RunFact>) {
        let next = latest.map(|run| (run.key.clone(), run.state));
        if let Some((previous_key, previous_state)) = &self.previous_latest {
            if next.as_ref().map(|(key, _)| key) != Some(previous_key) {
                self.interrupted_open.remove(previous_key);
            } else if previous_state.unsettled()
                && next
                    .as_ref()
                    .is_some_and(|(_, state)| *state == RunState::Interrupted)
            {
                self.interrupted_open.insert(previous_key.clone());
            }
        }
        self.previous_latest = next;
    }
}

#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// Explicit session running identity wins over stale latest-run metadata.
    pub running: Option<RunKey>,
    pub latest: Option<RunKey>,
    pub is_working: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FoldLabel {
    Worked { elapsed_ms: Option<u64> },
    Stopped { elapsed_ms: Option<u64> },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    pub run: RunKey,
    pub anchor: RowKey,
    pub members: Arc<[RowKey]>,
    pub expanded: bool,
    pub label: FoldLabel,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkGroup {
    pub key: GroupKey,
    pub anchor: RowKey,
    pub members: Arc<[RowKey]>,
    pub expanded: bool,
    pub latest_tool_failed: bool,
    /// Live groups select the most recent active candidate, otherwise last work.
    pub live_member: Option<RowKey>,
    pub working: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlannedRow {
    Source(RowKey),
    Fold(Fold),
    WorkSummary(WorkGroup),
    /// Exactly one outer row; a renderer must virtualize these references.
    WorkDetails {
        group: GroupKey,
        members: Arc<[RowKey]>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PresentationKey {
    Source(RowKey),
    Fold(RunKey),
    WorkSummary(GroupKey),
    WorkDetails(GroupKey),
}
impl PlannedRow {
    pub fn key(&self) -> PresentationKey {
        match self {
            Self::Source(key) => PresentationKey::Source(key.clone()),
            Self::Fold(fold) => PresentationKey::Fold(fold.run.clone()),
            Self::WorkSummary(group) => PresentationKey::WorkSummary(group.key.clone()),
            Self::WorkDetails { group, .. } => PresentationKey::WorkDetails(group.clone()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub rows: Vec<PlannedRow>,
    /// Matches Copy's last-assistant-per-run / unkeyed-user-boundary rule.
    /// Availability still requires complete text and authoritative run state.
    pub terminal_assistants: HashSet<RowKey>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    DuplicateRow(RowKey),
    DuplicateRun(RunKey),
    CrossSourceRun { row: RowKey, run: RunKey },
    CrossSourceRestart(RunKey),
    DuplicatePresentation(PresentationKey),
    EmptyIdentity,
}
pub struct Input<'a> {
    pub rows: &'a [SourceRow],
    pub runs: &'a [RunFact],
    pub expansion: &'a Expansion,
    pub session: &'a Session,
    pub activity: &'a Activity,
}

/// Hooks for a retained adapter: text flushes never require history replanning.
/// Timing-only updates can refresh a fold label without rebuilding topology.
/// Scope updates invalidate that run, adjacent group boundaries and the active
/// visual response containing it (including restart siblings). Activity changes
/// need that same response scope. Order, insert/remove, source changes and reset
/// require Topology. derive() itself is an initial/full planner; this enum does
/// not claim an incremental cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalidation {
    ContentOnly,
    Timing(RunKey),
    Scope(RunKey),
    Topology,
}
pub fn row_invalidation(before: &SourceRow, after: &SourceRow) -> Invalidation {
    if before == after {
        return Invalidation::ContentOnly;
    }
    if before.key != after.key || before.run != after.run {
        return Invalidation::Topology;
    }
    let Some(run) = &after.run else {
        return Invalidation::Topology;
    };
    if before.kind == after.kind && before.streaming == after.streaming {
        Invalidation::Timing(run.clone())
    } else {
        Invalidation::Scope(run.clone())
    }
}
pub fn run_invalidation(before: &RunFact, after: &RunFact) -> Invalidation {
    if before == after {
        Invalidation::ContentOnly
    } else if before.key == after.key {
        Invalidation::Scope(after.key.clone())
    } else {
        Invalidation::Topology
    }
}

pub fn derive(input: Input<'_>) -> Result<Plan, PlanError> {
    let runs = validate(&input)?;
    let terminal_assistants = terminals(input.rows);
    let (unfolded, current) = active_runs(&input, &runs);
    let folds = folds(&input, &runs, &terminal_assistants, &unfolded);
    let mut collapsed = HashSet::new();
    for fold in folds.values().filter(|fold| !fold.expanded) {
        collapsed.extend(fold.members.iter().cloned());
    }
    let active_suffix = suffix_start(input.rows, current.as_ref());
    let mut rows = Vec::new();
    let mut index = 0;
    while index < input.rows.len() {
        let row = &input.rows[index];
        if let Some(fold) = folds.get(&row.key) {
            rows.push(PlannedRow::Fold(fold.clone()));
        }
        if collapsed.contains(&row.key) {
            index += 1;
            continue;
        }
        if !matches!(row.kind, RowKind::Work(_)) {
            rows.push(PlannedRow::Source(row.key.clone()));
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < input.rows.len() {
            let next = &input.rows[end];
            if next.key.thread != row.key.thread
                || next.run != row.run
                || !matches!(next.kind, RowKind::Work(_))
                || collapsed.contains(&next.key)
                || folds.contains_key(&next.key)
                || (active_suffix == Some(end) && active_suffix != Some(index))
            {
                break;
            }
            end += 1;
        }
        let in_current = row
            .run
            .as_ref()
            .is_some_and(|run| Some(run) == current.as_ref());
        let live_context = input.activity.is_working && in_current;
        let visible = visible_work(&input.rows[index..end], live_context);
        if !visible.is_empty() {
            let suffix = active_suffix.is_some_and(|start| index >= start);
            let live = live_context
                && (suffix
                    || visible.iter().any(|entry| match &entry.kind {
                        RowKind::Work(work) => work.status == WorkStatus::InProgress,
                        _ => false,
                    }));
            append_work(&mut rows, row, &visible, live, input.expansion);
        }
        index = end;
    }
    let mut presentation_keys = HashSet::new();
    for row in &rows {
        let key = row.key();
        if !presentation_keys.insert(key.clone()) {
            return Err(PlanError::DuplicatePresentation(key));
        }
    }
    Ok(Plan {
        rows,
        terminal_assistants,
    })
}

fn validate<'a>(input: &'a Input<'_>) -> Result<HashMap<RunKey, &'a RunFact>, PlanError> {
    let mut runs = HashMap::new();
    for run in input.runs {
        if run.key.thread.is_empty() || run.key.id.is_empty() {
            return Err(PlanError::EmptyIdentity);
        }
        if run
            .restart_continuation_of
            .as_ref()
            .is_some_and(|prior| prior.thread != run.key.thread)
        {
            return Err(PlanError::CrossSourceRestart(run.key.clone()));
        }
        if runs.insert(run.key.clone(), run).is_some() {
            return Err(PlanError::DuplicateRun(run.key.clone()));
        }
    }
    let mut seen = HashSet::new();
    for row in input.rows {
        if row.key.thread.is_empty() || row.key.item.is_empty() {
            return Err(PlanError::EmptyIdentity);
        }
        if let Some(run) = &row.run {
            if run.id.is_empty() || run.thread != row.key.thread {
                return Err(PlanError::CrossSourceRun {
                    row: row.key.clone(),
                    run: run.clone(),
                });
            }
        }
        if !seen.insert(&row.key) {
            return Err(PlanError::DuplicateRow(row.key.clone()));
        }
    }
    Ok(runs)
}
fn terminals(rows: &[SourceRow]) -> HashSet<RowKey> {
    let mut keyed = HashMap::new();
    let mut unkeyed = HashMap::new();
    let mut result = HashSet::new();
    for row in rows {
        match row.kind {
            RowKind::User => {
                if let Some(key) = unkeyed.remove(&row.key.thread) {
                    result.insert(key);
                }
            }
            RowKind::Assistant => {
                if let Some(run) = &row.run {
                    keyed.insert(run, row.key.clone());
                } else {
                    unkeyed.insert(row.key.thread.clone(), row.key.clone());
                }
            }
            _ => {}
        }
    }
    result.extend(keyed.into_values());
    result.extend(unkeyed.into_values());
    result
}
fn active_runs(
    input: &Input<'_>,
    runs: &HashMap<RunKey, &RunFact>,
) -> (HashSet<RunKey>, Option<RunKey>) {
    let mut unfolded: HashSet<_> = runs
        .values()
        .filter(|run| run.state.unsettled())
        .map(|run| run.key.clone())
        .collect();
    let current = input.activity.running.clone().or_else(|| {
        input
            .activity
            .latest
            .as_ref()
            .filter(|key| {
                runs.get(*key)
                    .is_none_or(|run| run.state.unsettled() || run.completed_at.is_none())
            })
            .cloned()
    });
    if let Some(current) = &current {
        unfolded.insert(current.clone());
        if input.activity.is_working {
            let boundary = input
                .rows
                .iter()
                .rposition(|row| row.key.thread == current.thread && row.kind == RowKind::User);
            for row in &input.rows[boundary.map_or(0, |index| index + 1)..] {
                if row.key.thread == current.thread {
                    if let Some(run) = &row.run {
                        unfolded.insert(run.clone());
                    }
                }
            }
        }
    }
    (unfolded, current)
}
fn elapsed(start: Option<i64>, end: Option<i64>) -> Option<u64> {
    Some(
        (i128::from(end?) - i128::from(start?))
            .max(0)
            .min(i128::from(u64::MAX)) as u64,
    )
}
fn folds(
    input: &Input<'_>,
    runs: &HashMap<RunKey, &RunFact>,
    terminals: &HashSet<RowKey>,
    unfolded: &HashSet<RunKey>,
) -> BTreeMap<RowKey, Fold> {
    let mut grouped: BTreeMap<RunKey, Vec<&SourceRow>> = BTreeMap::new();
    let mut starts = HashMap::new();
    let mut pending_users = HashMap::new();
    for row in input.rows {
        if row.kind == RowKind::User {
            // Retain an explicit missing boundary too; never borrow an older user.
            pending_users.insert(row.key.thread.clone(), row.started_at);
            continue;
        }
        if !matches!(
            row.kind,
            RowKind::Assistant
                | RowKind::Work(_)
                | RowKind::Compaction
                | RowKind::Barrier(Barrier::Request | Barrier::SevereFailure | Barrier::AgentSpawn)
        ) {
            continue;
        }
        let Some(run) = &row.run else {
            continue;
        };
        if !grouped.contains_key(run) {
            starts.insert(
                run.clone(),
                pending_users
                    .remove(&row.key.thread)
                    .flatten()
                    .or(row.started_at),
            );
        }
        grouped.entry(run.clone()).or_default().push(row);
    }
    let mut folds = BTreeMap::new();
    for (run_key, entries) in grouped {
        let Some(run) = runs.get(&run_key) else {
            continue;
        };
        if !run.state.foldable()
            || unfolded.contains(&run_key)
            || entries.iter().any(|row| row.streaming)
        {
            continue;
        }
        let terminal = entries.iter().position(|row| terminals.contains(&row.key));
        let terminal_index = terminal.unwrap_or(entries.len());
        let members: Vec<_> = entries
            .iter()
            .enumerate()
            .filter(|(index, row)| {
                // Protected native cards still count as trailing work and
                // duration endpoints, but must never disappear into a fold.
                if terminals.contains(&row.key) || matches!(row.kind, RowKind::Barrier(_)) {
                    return false;
                }
                if row.kind == RowKind::Compaction {
                    return true;
                }
                if *index < terminal_index {
                    return true;
                }
                entries.len() == terminal_index + 2
                    && matches!(&row.kind, RowKind::Work(work) if !work.failed())
            })
            .map(|(_, row)| row.key.clone())
            .collect();
        let member_set: HashSet<_> = members.iter().collect();
        if members.is_empty()
            || !entries
                .iter()
                .any(|row| member_set.contains(&row.key) && row.kind != RowKind::Compaction)
        {
            continue;
        }
        let end = terminal
            .and_then(|index| entries[index].ended_at)
            .into_iter()
            .chain(entries.last().and_then(|row| row.ended_at))
            .max();
        let duration = if run.work_start().is_some() && run.completed_at.is_some() {
            elapsed(run.work_start(), run.completed_at)
        } else {
            elapsed(starts.get(&run_key).copied().flatten(), end)
        };
        let stopped =
            input.activity.latest.as_ref() == Some(&run_key) && run.state == RunState::Interrupted;
        let fold = Fold {
            run: run_key.clone(),
            anchor: members[0].clone(),
            members: members.into(),
            expanded: input.expansion.runs.contains(&run_key)
                || input.session.interrupted_open.contains(&run_key),
            label: if stopped {
                FoldLabel::Stopped {
                    elapsed_ms: duration,
                }
            } else {
                FoldLabel::Worked {
                    elapsed_ms: duration,
                }
            },
        };
        folds.insert(fold.anchor.clone(), fold);
    }
    folds
}
fn suffix_start(rows: &[SourceRow], current: Option<&RunKey>) -> Option<usize> {
    let current = current?;
    let mut start = None;
    for (index, row) in rows.iter().enumerate().rev() {
        if row.run.as_ref() != Some(current) || !matches!(row.kind, RowKind::Work(_)) {
            break;
        }
        start = Some(index);
    }
    start
}
fn visible_work(rows: &[SourceRow], live: bool) -> Vec<&SourceRow> {
    let mut terminal = HashSet::new();
    let mut visible = Vec::new();
    for row in rows.iter().rev() {
        let RowKind::Work(work) = &row.kind else {
            continue;
        };
        if work.neutral && !(live && (work.status == WorkStatus::InProgress || work.task_progress))
        {
            continue;
        }
        if let Some(Lifecycle::StatuslessIdlessMarker(identity)) = &work.lifecycle {
            if terminal.contains(identity) {
                continue;
            }
        }
        if let Some(Lifecycle::Terminal(identity)) = &work.lifecycle {
            terminal.insert(identity.clone());
        }
        visible.push(row);
    }
    visible.reverse();
    visible
}
fn append_work(
    output: &mut Vec<PlannedRow>,
    anchor: &SourceRow,
    visible: &[&SourceRow],
    live: bool,
    expansion: &Expansion,
) {
    fn work(row: &SourceRow) -> &WorkFact {
        match &row.kind {
            RowKind::Work(work) => work,
            _ => unreachable!(),
        }
    }
    if visible.len() == 1 && work(visible[0]).tool_like && !live {
        output.push(PlannedRow::Source(visible[0].key.clone()));
        return;
    }
    let key = GroupKey::for_row(anchor);
    let latest = visible.last().expect("nonempty visible work");
    let latest_tool_failed = visible
        .iter()
        .rev()
        .find(|row| work(row).tool_like)
        .is_some_and(|row| work(row).failed());
    let active = visible
        .iter()
        .rev()
        .find(|row| work(row).active_candidate());
    let latest_failure = work(latest).status != WorkStatus::Declined && work(latest).failed();
    let live_member = (live && (!latest_failure || active.is_some()))
        .then(|| active.unwrap_or(latest).key.clone());
    let working = live_member.is_some()
        && (active.is_some()
            || (work(latest).tool_like
                && !work(latest).failed()
                && work(latest).status != WorkStatus::Stopped));
    let expanded = expansion.groups.contains(&key);
    let members: Arc<[RowKey]> = visible
        .iter()
        .map(|row| row.key.clone())
        .collect::<Vec<_>>()
        .into();
    output.push(PlannedRow::WorkSummary(WorkGroup {
        key: key.clone(),
        anchor: anchor.key.clone(),
        members: members.clone(),
        expanded,
        latest_tool_failed,
        live_member,
        working,
    }));
    if expanded {
        output.push(PlannedRow::WorkDetails {
            group: key,
            members,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> RowKey {
        RowKey {
            thread: "source".into(),
            item: id.into(),
        }
    }
    fn run_key(id: &str) -> RunKey {
        RunKey {
            thread: "source".into(),
            id: id.into(),
        }
    }
    fn work() -> WorkFact {
        WorkFact {
            tool_like: true,
            tool_call_id: None,
            status: WorkStatus::Completed,
            output_failed: false,
            neutral: false,
            task_progress: false,
            lifecycle: None,
        }
    }
    fn row(id: &str, kind: RowKind) -> SourceRow {
        SourceRow {
            key: key(id),
            run: Some(run_key("run")),
            kind,
            streaming: false,
            started_at: None,
            ended_at: None,
        }
    }
    fn tool(id: &str) -> SourceRow {
        row(id, RowKind::Work(work()))
    }
    fn run(id: &str, state: RunState) -> RunFact {
        RunFact {
            key: run_key(id),
            state,
            requested_at: None,
            started_at: None,
            work_started_at: None,
            completed_at: None,
            restart_continuation_of: None,
        }
    }
    fn plan(
        rows: &[SourceRow],
        runs: &[RunFact],
        expansion: &Expansion,
        session: &Session,
        activity: &Activity,
    ) -> Plan {
        derive(Input {
            rows,
            runs,
            expansion,
            session,
            activity,
        })
        .unwrap()
    }
    fn history(rows: &[SourceRow], runs: &[RunFact]) -> Plan {
        plan(
            rows,
            runs,
            &Expansion::default(),
            &Session::default(),
            &Activity::default(),
        )
    }
    fn fold(plan: &Plan) -> &Fold {
        plan.rows
            .iter()
            .find_map(|row| match row {
                PlannedRow::Fold(fold) => Some(fold),
                _ => None,
            })
            .expect("positive fold control")
    }
    fn summary(plan: &Plan) -> &WorkGroup {
        plan.rows
            .iter()
            .find_map(|row| match row {
                PlannedRow::WorkSummary(group) => Some(group),
                _ => None,
            })
            .expect("positive group control")
    }
    fn source_visible(plan: &Plan, id: &str) -> bool {
        plan.rows.contains(&PlannedRow::Source(key(id)))
    }
    fn basic_turn() -> Vec<SourceRow> {
        vec![
            row("user", RowKind::User),
            row("comment", RowKind::Assistant),
            tool("tool"),
            row("answer", RowKind::Assistant),
        ]
    }

    #[test]
    fn thirty_tools_collapse_and_expand_to_one_shared_details_row_in_order() {
        let rows: Vec<_> = (0..30).map(|n| tool(&format!("tool-{n}"))).collect();
        let collapsed = history(&rows, &[]);
        assert_eq!(collapsed.rows.len(), 1);
        let group = summary(&collapsed);
        assert_eq!(group.members.len(), 30);
        assert!(!group.expanded);
        let mut expansion = Expansion::default();
        expansion.groups.insert(group.key.clone());
        let expanded = plan(
            &rows,
            &[],
            &expansion,
            &Session::default(),
            &Activity::default(),
        );
        assert_eq!(expanded.rows.len(), 2);
        let PlannedRow::WorkDetails { members, .. } = &expanded.rows[1] else {
            panic!("one details row required")
        };
        assert_eq!(
            members.as_ref(),
            rows.iter().map(|row| row.key.clone()).collect::<Vec<_>>()
        );
        assert!(Arc::ptr_eq(members, &summary(&expanded).members));
        assert_eq!(collapsed.rows[0].key(), expanded.rows[0].key());
    }

    #[test]
    fn thousand_tools_require_two_outer_rows_and_only_source_references() {
        let rows: Vec<_> = (0..1000).map(|n| tool(&format!("tool-{n}"))).collect();
        let collapsed = history(&rows, &[]);
        let mut expansion = Expansion::default();
        expansion.groups.insert(summary(&collapsed).key.clone());
        let expanded = plan(
            &rows,
            &[],
            &expansion,
            &Session::default(),
            &Activity::default(),
        );
        assert_eq!(expanded.rows.len(), 2);
        assert_eq!(summary(&expanded).members.len(), 1000);
        // Rendering/keyboard/viewport bounds still require native integration tests.
    }

    #[test]
    fn standalone_tool_and_mixed_information_group_match_t3_shape() {
        assert_eq!(
            history(&[tool("one")], &[]).rows,
            vec![PlannedRow::Source(key("one"))]
        );
        let mut info = work();
        info.tool_like = false;
        let rows = [tool("tool"), row("information", RowKind::Work(info))];
        assert_eq!(
            summary(&history(&rows, &[])).members.as_ref(),
            &[key("tool"), key("information")]
        );
    }

    #[test]
    fn every_protected_row_breaks_groups_and_remains_visible_inside_a_fold() {
        let kinds = [
            RowKind::User,
            RowKind::Assistant,
            RowKind::Compaction,
            RowKind::Barrier(Barrier::Plan),
            RowKind::Barrier(Barrier::Request),
            RowKind::Barrier(Barrier::SevereFailure),
            RowKind::Barrier(Barrier::AgentSpawn),
            RowKind::Barrier(Barrier::Notice),
            RowKind::Barrier(Barrier::Unknown),
        ];
        for kind in kinds {
            let rows = [tool("before"), row("boundary", kind), tool("after")];
            let plan = history(&rows, &[]);
            assert_eq!(plan.rows.len(), 3, "boundary {:?}", rows[1].kind);
            assert!(source_visible(&plan, "boundary"));
        }
        for barrier in [
            Barrier::Request,
            Barrier::SevereFailure,
            Barrier::AgentSpawn,
            Barrier::Plan,
            Barrier::Unknown,
        ] {
            let mut rows = basic_turn();
            rows.insert(2, row("protected", RowKind::Barrier(barrier)));
            let plan = history(&rows, &[run("run", RunState::Completed)]);
            assert!(!fold(&plan).members.contains(&key("protected")));
            assert!(source_visible(&plan, "protected"));
        }
    }

    #[test]
    fn groups_do_not_cross_run_or_inherited_source_identity() {
        let mut other_run = tool("other-run");
        other_run.run = Some(run_key("second"));
        let mut inherited = tool("same-item");
        inherited.key.thread = "ancestor".into();
        inherited.run.as_mut().unwrap().thread = "ancestor".into();
        let rows = [tool("one"), other_run, inherited];
        assert_eq!(history(&rows, &[]).rows.len(), 3);
        let mut invalid = rows[0].clone();
        invalid.run.as_mut().unwrap().thread = "wrong-source".into();
        assert!(matches!(
            derive(Input {
                rows: &[invalid],
                runs: &[],
                expansion: &Expansion::default(),
                session: &Session::default(),
                activity: &Activity::default()
            }),
            Err(PlanError::CrossSourceRun { .. })
        ));
    }

    #[test]
    fn final_answer_survives_fold_and_trailing_rules_are_exact() {
        let completed = run("run", RunState::Completed);
        let base = basic_turn();
        let plan = history(&base, &[completed.clone()]);
        assert_eq!(fold(&plan).members.as_ref(), &[key("comment"), key("tool")]);
        assert!(source_visible(&plan, "user") && source_visible(&plan, "answer"));
        assert!(!source_visible(&plan, "comment"));
        let mut one = base.clone();
        one.push(tool("trailing-one"));
        assert!(fold(&history(&one, &[completed.clone()]))
            .members
            .contains(&key("trailing-one")));
        one.push(tool("trailing-two"));
        let two = history(&one, &[completed.clone()]);
        assert!(!fold(&two).members.contains(&key("trailing-one")));
        assert_eq!(
            summary(&two).members.as_ref(),
            &[key("trailing-one"), key("trailing-two")]
        );
        let mut failed = base;
        let mut failure = work();
        failure.status = WorkStatus::Failed;
        failed.push(row("failed-last", RowKind::Work(failure)));
        assert!(source_visible(
            &history(&failed, &[completed]),
            "failed-last"
        ));
    }

    #[test]
    fn protected_trailing_work_counts_for_tail_size_and_duration_but_never_hides() {
        for barrier in [
            Barrier::SevereFailure,
            Barrier::Request,
            Barrier::AgentSpawn,
        ] {
            let mut rows = basic_turn();
            rows[0].started_at = Some(0);
            rows[3].ended_at = Some(22_000);
            let mut protected = row("protected-tail", RowKind::Barrier(barrier));
            protected.ended_at = Some(30_000);
            rows.push(protected);
            let completed = run("run", RunState::Completed);
            let single = history(&rows, &[completed.clone()]);
            assert!(source_visible(&single, "protected-tail"));
            assert_eq!(
                fold(&single).label,
                FoldLabel::Worked {
                    elapsed_ms: Some(30_000)
                }
            );
            rows.push(tool("normal-tail"));
            let multiple = history(&rows, &[completed]);
            assert!(source_visible(&multiple, "protected-tail"));
            assert!(source_visible(&multiple, "normal-tail"));
            assert!(!fold(&multiple).members.contains(&key("normal-tail")));
        }
    }

    #[test]
    fn compaction_alone_never_folds_but_joins_meaningful_work() {
        let completed = run("run", RunState::Completed);
        let lone = [
            row("compaction", RowKind::Compaction),
            row("answer", RowKind::Assistant),
        ];
        let plan = history(&lone, &[completed.clone()]);
        assert!(source_visible(&plan, "compaction"));
        assert!(!plan
            .rows
            .iter()
            .any(|row| matches!(row, PlannedRow::Fold(_))));
        let rows = [tool("tool"), lone[1].clone(), lone[0].clone()];
        assert_eq!(
            fold(&history(&rows, &[completed])).members.as_ref(),
            &[key("tool"), key("compaction")]
        );
    }

    #[test]
    fn latest_tool_failure_can_recover_and_command_text_is_not_inspected() {
        let mut failed = work();
        failed.output_failed = true;
        let rows = [
            row("failure", RowKind::Work(failed.clone())),
            tool("recovered"),
        ];
        assert!(!summary(&history(&rows, &[])).latest_tool_failed);
        let reverse = [tool("success"), row("failure", RowKind::Work(failed))];
        assert!(summary(&history(&reverse, &[])).latest_tool_failed);
        // Only typed output_failed/status affect the result; authored input is
        // deliberately absent from SourceRow and cannot trigger a false failure.
    }

    #[test]
    fn only_superseded_statusless_idless_lifecycle_markers_disappear() {
        let mut marker = work();
        marker.status = WorkStatus::Unspecified;
        marker.lifecycle = Some(Lifecycle::StatuslessIdlessMarker("command:ls".into()));
        let mut terminal = work();
        terminal.lifecycle = Some(Lifecycle::Terminal("command:ls".into()));
        let rows = [
            row("marker", RowKind::Work(marker)),
            row("terminal", RowKind::Work(terminal)),
        ];
        assert_eq!(
            history(&rows, &[]).rows,
            vec![PlannedRow::Source(key("terminal"))]
        );
        assert_eq!(
            history(&rows[..1], &[]).rows,
            vec![PlannedRow::Source(key("marker"))]
        );
    }

    #[test]
    fn streaming_and_unknown_runs_never_fold_or_guess_settlement() {
        for state in [
            RunState::Running,
            RunState::Waiting,
            RunState::Queued,
            RunState::Unknown,
        ] {
            let plan = history(&basic_turn(), &[run("run", state)]);
            assert!(!plan
                .rows
                .iter()
                .any(|row| matches!(row, PlannedRow::Fold(_))));
            assert!(source_visible(&plan, "comment"));
        }
        let mut rows = basic_turn();
        rows[1].streaming = true;
        assert!(!history(&rows, &[run("run", RunState::Completed)])
            .rows
            .iter()
            .any(|row| matches!(row, PlannedRow::Fold(_))));
        assert!(!history(&basic_turn(), &[])
            .rows
            .iter()
            .any(|row| matches!(row, PlannedRow::Fold(_))));
    }

    #[test]
    fn authoritative_running_identity_and_promptless_restart_keep_visual_response_open() {
        let mut rows = basic_turn();
        let mut next = row("replacement-comment", RowKind::Assistant);
        next.run = Some(run_key("restart"));
        rows.push(next);
        let old = run("run", RunState::Completed);
        let mut stale = run("restart", RunState::Completed);
        stale.completed_at = Some(100);
        stale.restart_continuation_of = Some(old.key.clone());
        let activity = Activity {
            running: Some(stale.key.clone()),
            latest: Some(old.key.clone()),
            is_working: true,
        };
        let active_plan = plan(
            &rows,
            &[old.clone(), stale.clone()],
            &Expansion::default(),
            &Session::default(),
            &activity,
        );
        assert!(!active_plan
            .rows
            .iter()
            .any(|row| matches!(row, PlannedRow::Fold(_))));
        assert!(source_visible(&active_plan, "comment"));
        // A real steer has a user boundary, so the previous response can fold.
        rows.insert(4, row("steer", RowKind::User));
        let plan = plan(
            &rows,
            &[old, stale],
            &Expansion::default(),
            &Session::default(),
            &activity,
        );
        assert_eq!(fold(&plan).run, run_key("run"));
        assert!(source_visible(&plan, "replacement-comment"));
    }

    #[test]
    fn stop_is_open_only_after_observed_session_transition_and_clears_next_run() {
        let running = run("run", RunState::Running);
        let mut stopped = run("run", RunState::Interrupted);
        stopped.completed_at = Some(47_000);
        let mut session = Session::default();
        session.observe_latest(Some(&running));
        session.observe_latest(Some(&stopped));
        let activity = Activity {
            latest: Some(stopped.key.clone()),
            ..Activity::default()
        };
        let opened = plan(
            &basic_turn(),
            &[stopped.clone()],
            &Expansion::default(),
            &session,
            &activity,
        );
        assert!(fold(&opened).expanded && source_visible(&opened, "comment"));
        assert_eq!(fold(&opened).label, FoldLabel::Stopped { elapsed_ms: None });
        let reloaded = plan(
            &basic_turn(),
            &[stopped.clone()],
            &Expansion::default(),
            &Session::default(),
            &activity,
        );
        assert!(!fold(&reloaded).expanded);
        session.observe_latest(Some(&run("next", RunState::Running)));
        assert!(
            !fold(&plan(
                &basic_turn(),
                &[stopped],
                &Expansion::default(),
                &session,
                &activity
            ))
            .expanded
        );
    }

    #[test]
    fn authoritative_v2_timing_then_user_boundary_and_trailing_end_then_no_guess() {
        let mut rows = basic_turn();
        rows[0].started_at = Some(0);
        rows[1].started_at = Some(5_000);
        rows[3].ended_at = Some(22_000);
        let mut completed = run("run", RunState::Completed);
        completed.started_at = Some(1_000);
        completed.completed_at = Some(48_000);
        assert_eq!(
            fold(&history(&rows, &[completed.clone()])).label,
            FoldLabel::Worked {
                elapsed_ms: Some(47_000)
            }
        );
        completed.started_at = None;
        completed.completed_at = None;
        assert_eq!(
            fold(&history(&rows, &[completed.clone()])).label,
            FoldLabel::Worked {
                elapsed_ms: Some(22_000)
            }
        );
        let mut trailing = tool("trailing");
        trailing.ended_at = Some(25_000);
        rows.push(trailing);
        assert_eq!(
            fold(&history(&rows, &[completed.clone()])).label,
            FoldLabel::Worked {
                elapsed_ms: Some(25_000)
            }
        );
        rows[0].started_at = None;
        rows[1].started_at = None;
        assert_eq!(
            fold(&history(&rows, &[completed.clone()])).label,
            FoldLabel::Worked { elapsed_ms: None }
        );
        completed.started_at = Some(100);
        completed.completed_at = Some(99);
        assert_eq!(
            fold(&history(&rows, &[completed.clone()])).label,
            FoldLabel::Worked {
                elapsed_ms: Some(0)
            }
        );
        completed.requested_at = Some(0);
        completed.started_at = Some(5);
        completed.work_started_at = Some(10);
        assert_eq!(completed.work_start(), Some(10));
    }

    #[test]
    fn unkeyed_terminals_follow_user_boundaries_without_inventing_folds() {
        let mut rows = vec![
            row("user-one", RowKind::User),
            row("comment-one", RowKind::Assistant),
            row("answer-one", RowKind::Assistant),
            row("user-two", RowKind::User),
            row("answer-two", RowKind::Assistant),
        ];
        for row in &mut rows {
            row.run = None;
        }
        let plan = history(&rows, &[]);
        assert_eq!(
            plan.terminal_assistants,
            HashSet::from([key("answer-one"), key("answer-two")])
        );
        assert_eq!(plan.rows.len(), rows.len());
    }

    #[test]
    fn live_suffix_selects_latest_running_tool_and_neutral_progress_only_when_live() {
        let mut running = work();
        running.status = WorkStatus::InProgress;
        running.neutral = true;
        let mut info = work();
        info.tool_like = false;
        let rows = [
            row("running", RowKind::Work(running)),
            row("info", RowKind::Work(info)),
        ];
        assert_eq!(
            summary(&history(&rows, &[])).members.as_ref(),
            &[key("info")]
        );
        let activity = Activity {
            running: Some(run_key("run")),
            is_working: true,
            ..Activity::default()
        };
        let live = plan(
            &rows,
            &[run("run", RunState::Running)],
            &Expansion::default(),
            &Session::default(),
            &activity,
        );
        assert_eq!(summary(&live).live_member, Some(key("running")));
        assert!(summary(&live).working);
        assert_eq!(summary(&live).members.len(), 2);
    }

    #[test]
    fn metadata_hooks_separate_content_timing_lifecycle_and_identity() {
        let before = tool("one");
        assert_eq!(
            row_invalidation(&before, &before),
            Invalidation::ContentOnly
        );
        let mut after = before.clone();
        after.ended_at = Some(1);
        assert_eq!(
            row_invalidation(&before, &after),
            Invalidation::Timing(run_key("run"))
        );
        after.streaming = true;
        assert_eq!(
            row_invalidation(&before, &after),
            Invalidation::Scope(run_key("run"))
        );
        after.run = Some(run_key("different"));
        assert_eq!(row_invalidation(&before, &after), Invalidation::Topology);
        assert_eq!(
            run_invalidation(
                &run("run", RunState::Running),
                &run("run", RunState::Completed)
            ),
            Invalidation::Scope(run_key("run"))
        );
    }

    #[test]
    fn empty_input_and_duplicate_identity_are_explicit() {
        assert!(history(&[], &[]).rows.is_empty());
        let rows = [tool("duplicate"), tool("duplicate")];
        assert!(matches!(
            derive(Input {
                rows: &rows,
                runs: &[],
                expansion: &Expansion::default(),
                session: &Session::default(),
                activity: &Activity::default()
            }),
            Err(PlanError::DuplicateRow(_))
        ));
        let runs = [
            run("run", RunState::Completed),
            run("run", RunState::Running),
        ];
        assert!(matches!(
            derive(Input {
                rows: &[],
                runs: &runs,
                expansion: &Expansion::default(),
                session: &Session::default(),
                activity: &Activity::default()
            }),
            Err(PlanError::DuplicateRun(_))
        ));
    }
}
