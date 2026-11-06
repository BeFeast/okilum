//! Opt-in rotation preview and durable recovery. No clock, timer or transport.
//! Scheduling is a fallback (#1473); this module never swaps a model itself.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    enabled: bool,
    timezone: String,
    minutes: [u16; 2],
}

impl Policy {
    /// Exactly two distinct local wall times. Zone validation belongs to the
    /// timezone database adapter, never the machine's ambient timezone.
    pub fn new(enabled: bool, timezone: &str, at: [&str; 2]) -> Result<Self, &'static str> {
        if timezone.trim().is_empty() || timezone.chars().any(char::is_control) {
            return Err("An explicit timezone is required");
        }
        fn minute(value: &str) -> Result<u16, &'static str> {
            let b = value.as_bytes();
            if b.len() != 5
                || b[2] != b':'
                || ![b[0], b[1], b[3], b[4]].iter().all(u8::is_ascii_digit)
            {
                return Err("Use HH:MM local times");
            }
            let h = u16::from(b[0] - b'0') * 10 + u16::from(b[1] - b'0');
            let m = u16::from(b[3] - b'0') * 10 + u16::from(b[4] - b'0');
            if h > 23 || m > 59 {
                return Err("Invalid local time");
            }
            Ok(h * 60 + m)
        }
        let mut minutes = [minute(at[0])?, minute(at[1])?];
        minutes.sort();
        if minutes[0] == minutes[1] {
            return Err("Choose two distinct times");
        }
        Ok(Self {
            enabled,
            timezone: timezone.into(),
            minutes,
        })
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
    pub fn minutes(&self) -> [u16; 2] {
        self.minutes
    }

    /// The caller supplies calendar dates (days since Unix epoch), not UTC
    /// dates guessed from a fixed offset. Fold: first occurrence only. Gap:
    /// skip that slot explicitly. Unknown zones/errors fail the entire preview.
    pub fn preview(
        &self,
        dates: &[i64],
        after: i64,
        resolver: &impl ZoneResolver,
    ) -> Result<Vec<Slot>, String> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for &day in dates {
            for &minute in &self.minutes {
                let resolution = resolver.resolve(&self.timezone, day, minute)?;
                let utc = match resolution {
                    LocalTime::Unique(utc) => Some(utc),
                    LocalTime::Fold(a, b) => Some(a.min(b)),
                    LocalTime::Gap => None,
                };
                if utc.is_none_or(|utc| utc > after) {
                    result.push(Slot { day, minute, utc });
                }
            }
        }
        result.sort_by_key(|slot| (slot.day, slot.minute));
        result.dedup();
        Ok(result)
    }
}

pub enum LocalTime {
    Unique(i64),
    Fold(i64, i64),
    Gap,
}
pub trait ZoneResolver {
    fn resolve(&self, timezone: &str, day: i64, minute: u16) -> Result<LocalTime, String>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub day: i64,
    pub minute: u16,
    /// None means a nonexistent local time, never a runnable slot.
    pub utc: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub origin: String,
    pub project: String,
    pub manager: String,
    /// Changes on rebind, policy edit, disconnect or feature disable.
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocker {
    PendingQuestion,
    LiveOperation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    owner: Owner,
    slot: Slot,
    generation: u64,
}
#[derive(Default)]
pub struct Schedule {
    owner: Option<Owner>,
    active: Option<Ticket>,
    finished: Vec<Slot>,
    generation: u64,
}
impl Schedule {
    /// Cancels stale completions even when switching away and back to one owner.
    pub fn bind(&mut self, owner: Option<Owner>) {
        if self.owner != owner {
            self.owner = owner;
            self.active = None;
            self.finished.clear();
        }
    }
    pub fn begin(
        &mut self,
        owner: &Owner,
        policy: &Policy,
        slot: &Slot,
        now: i64,
        blocker: Option<Blocker>,
    ) -> Result<Ticket, &'static str> {
        if !policy.enabled {
            return Err("Rotation is disabled");
        }
        if self.owner.as_ref() != Some(owner) {
            return Err("Stale rotation owner");
        }
        if self.active.is_some() {
            return Err("Rotation already in flight");
        }
        if slot.utc.is_none_or(|utc| utc > now) || !policy.minutes.contains(&slot.minute) {
            return Err("Slot is not due");
        }
        if self.finished.contains(slot) {
            return Err("Slot already handled");
        }
        if blocker.is_some() {
            self.finished.push(slot.clone());
            return Err("Rotation deferred to the next slot");
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Rotation generation exhausted")?;
        let ticket = Ticket {
            owner: owner.clone(),
            slot: slot.clone(),
            generation: self.generation,
        };
        self.active = Some(ticket.clone());
        Ok(ticket)
    }
    pub fn finish(&mut self, ticket: &Ticket) -> bool {
        if self.active.as_ref() != Some(ticket) || self.owner.as_ref() != Some(&ticket.owner) {
            return false;
        }
        self.finished.push(ticket.slot.clone());
        self.active = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Dst;
    impl ZoneResolver for Dst {
        fn resolve(&self, zone: &str, day: i64, minute: u16) -> Result<LocalTime, String> {
            if zone != "Asia/Jerusalem" {
                return Err("Unknown zone".into());
            }
            Ok(match (day, minute) {
                (1, 90) => LocalTime::Gap,
                (2, 90) => LocalTime::Fold(200, 100),
                _ => LocalTime::Unique(day * 86400 + i64::from(minute) * 60),
            })
        }
    }
    fn owner() -> Owner {
        Owner {
            origin: "fixture".into(),
            project: "p".into(),
            manager: "m".into(),
            revision: 1,
        }
    }
    #[test]
    fn explicit_opt_in_and_dst_resolution() {
        assert!(Policy::new(true, "UTC", ["24:00", "08:30"]).is_err());
        assert!(Policy::new(true, "UTC", ["08:30", "08:30"]).is_err());
        assert!(Policy::new(false, "unknown", ["01:30", "20:30"])
            .unwrap()
            .preview(&[1], 0, &Dst)
            .unwrap()
            .is_empty());
        let p = Policy::new(true, "Asia/Jerusalem", ["01:30", "20:30"]).unwrap();
        let slots = p.preview(&[1, 2], 0, &Dst).unwrap();
        assert_eq!(slots.len(), 4);
        assert_eq!(slots[0].utc, None);
        assert_eq!(slots[2].utc, Some(100));
        assert!(Policy::new(true, "unknown", ["01:30", "20:30"])
            .unwrap()
            .preview(&[1], 0, &Dst)
            .is_err());
    }
    #[test]
    fn single_flight_and_stale_owner_cancellation() {
        let p = Policy::new(true, "UTC", ["08:30", "20:30"]).unwrap();
        let o = owner();
        let slot = Slot {
            day: 0,
            minute: 510,
            utc: Some(100),
        };
        let mut s = Schedule::default();
        s.bind(Some(o.clone()));
        assert!(s
            .begin(&o, &p, &slot, 100, Some(Blocker::LiveOperation))
            .is_err());
        assert!(s.begin(&o, &p, &slot, 100, None).is_err());
        let slot = Slot {
            day: 1,
            minute: 510,
            utc: Some(200),
        };
        let a = s.begin(&o, &p, &slot, 200, None).unwrap();
        assert!(s.begin(&o, &p, &slot, 100, None).is_err());
        s.bind(None);
        s.bind(Some(o.clone()));
        let b = s.begin(&o, &p, &slot, 200, None).unwrap();
        assert!(!s.finish(&a));
        assert!(s.finish(&b));
        assert!(s.begin(&o, &p, &slot, 100, None).is_err());
    }
}

/// IANA TZif database reader. Uses an explicit database root, never process TZ.
/// TZif transition coverage is enforced: POSIX footer rules are not guessed.
pub struct TzifResolver {
    root: std::path::PathBuf,
}
impl TzifResolver {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn zone(&self, name: &str) -> Result<Zone, String> {
        use std::path::Component;
        if name.is_empty()
            || std::path::Path::new(name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("Invalid IANA zone name".into());
        }
        let bytes = std::fs::read(self.root.join(name)).map_err(|e| e.to_string())?;
        Zone::parse(&bytes)
    }
    /// Local calendar day at a supplied UTC instant, including DST offset.
    pub fn local_day(&self, name: &str, utc: i64) -> Result<i64, String> {
        let z = self.zone(name)?;
        utc.checked_add(i64::from(z.offset(utc)?))
            .map(|s| s.div_euclid(86400))
            .ok_or("Date overflow".into())
    }
}
struct Zone {
    transitions: Vec<(i64, usize)>,
    offsets: Vec<i32>,
    has_footer: bool,
}
impl Zone {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        fn number(b: &[u8], p: usize) -> Result<usize, String> {
            let a: [u8; 4] = b.get(p..p + 4).ok_or("Truncated TZif")?.try_into().unwrap();
            Ok(u32::from_be_bytes(a) as usize)
        }
        fn header(b: &[u8], p: usize) -> Result<[usize; 6], String> {
            if b.get(p..p + 4) != Some(b"TZif") {
                return Err("Invalid TZif header".into());
            }
            let mut counts = [0; 6];
            for (i, c) in counts.iter_mut().enumerate() {
                *c = number(b, p + 20 + i * 4)?;
            }
            if counts.iter().any(|n| *n > 1_000_000) {
                return Err("Oversized TZif".into());
            }
            Ok(counts)
        }
        let first = header(bytes, 0)?;
        let (base, width, c) = match bytes.get(4) {
            Some(b'2' | b'3' | b'4') => {
                let size =
                    first[3] * 5 + first[4] * 6 + first[5] + first[2] * 8 + first[1] + first[0];
                let base = 44 + size;
                (base + 44, 8, header(bytes, base)?)
            }
            Some(0) => (44, 4, first),
            _ => return Err("Unsupported TZif version".into()),
        };
        if c[4] == 0 || c[4] > 256 || c[2] != 0 {
            return Err("Unsupported TZif types or leap clock".into());
        }
        let end = base + c[3] * (width + 1) + c[4] * 6 + c[5] + c[1] + c[0];
        if end > bytes.len() {
            return Err("Truncated TZif body".into());
        }
        let types = base + c[3] * (width + 1);
        let mut offsets = Vec::new();
        for i in 0..c[4] {
            let offset =
                i32::from_be_bytes(bytes[types + i * 6..types + i * 6 + 4].try_into().unwrap());
            if offset.abs_diff(0) > 86400 {
                return Err("Invalid timezone offset".into());
            }
            offsets.push(offset);
        }
        let mut transitions = Vec::new();
        for i in 0..c[3] {
            let p = base + i * width;
            let utc = if width == 8 {
                i64::from_be_bytes(bytes[p..p + 8].try_into().unwrap())
            } else {
                i64::from(i32::from_be_bytes(bytes[p..p + 4].try_into().unwrap()))
            };
            let index = usize::from(bytes[base + c[3] * width + i]);
            if index >= offsets.len() || transitions.last().is_some_and(|&(t, _)| t >= utc) {
                return Err("Invalid TZif transition".into());
            }
            transitions.push((utc, index));
        }
        Ok(Self {
            transitions,
            offsets,
            has_footer: bytes
                .get(end..)
                .is_some_and(|f| f.iter().any(|b| *b != b'\n')),
        })
    }
    fn offset(&self, utc: i64) -> Result<i32, String> {
        if self.has_footer && self.transitions.last().is_some_and(|&(t, _)| utc > t) {
            return Err("Timezone date exceeds TZif transition coverage".into());
        }
        let i = self.transitions.partition_point(|&(t, _)| t <= utc);
        Ok(self.offsets[if i == 0 { 0 } else { self.transitions[i - 1].1 }])
    }
}
impl ZoneResolver for TzifResolver {
    fn resolve(&self, timezone: &str, day: i64, minute: u16) -> Result<LocalTime, String> {
        if minute >= 1440 {
            return Err("Invalid wall time".into());
        }
        let local = day
            .checked_mul(86400)
            .and_then(|v| v.checked_add(i64::from(minute) * 60))
            .ok_or("Date overflow")?;
        let z = self.zone(timezone)?;
        let mut candidates = Vec::new();
        let mut outside = false;
        for &offset in &z.offsets {
            let utc = local
                .checked_sub(i64::from(offset))
                .ok_or("Date overflow")?;
            match z.offset(utc) {
                Ok(actual) if actual == offset => candidates.push(utc),
                Err(_) => outside = true,
                _ => (),
            }
        }
        candidates.sort();
        candidates.dedup();
        match candidates.as_slice() {
            [utc] => Ok(LocalTime::Unique(*utc)),
            [a, b] => Ok(LocalTime::Fold(*a, *b)),
            [] if !outside => Ok(LocalTime::Gap),
            _ => Err("Timezone date unresolved or outside transition coverage".into()),
        }
    }
}

/// Durable checkpoint for one project/owner. Stored outside any deletable index.
/// A claim advances the cursor BEFORE draft preparation: restart cannot silently
/// replay it. An interrupted claim requires explicit human reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub revision: u64,
    pub owner: Owner,
    pub policy: Policy,
    pub through: i64,
    pub pending: Option<Slot>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Disabled,
    NeedsReconciliation(Slot),
    NoMissedWindow,
    Deferred(Blocker),
    /// Only the latest missed window is offered; never a burst of old rotations.
    Offer(Slot),
}
impl Checkpoint {
    pub fn recover(
        &self,
        owner: &Owner,
        now: i64,
        resolver: &TzifResolver,
        blocker: Option<Blocker>,
    ) -> Result<Recovery, String> {
        if &self.owner != owner {
            return Err("Stale rotation owner".into());
        }
        if !self.policy.enabled {
            return Ok(Recovery::Disabled);
        }
        if let Some(slot) = &self.pending {
            return Ok(Recovery::NeedsReconciliation(slot.clone()));
        }
        if now < self.through {
            return Err("Clock moved behind recovery cursor".into());
        }
        // Latest local day plus its predecessor suffice for a twice-daily policy.
        // No iteration over downtime and no catch-up storm.
        let day = resolver.local_day(self.policy.timezone(), now)?;
        let previous = day.checked_sub(1).ok_or("Date overflow")?;
        let slots = self
            .policy
            .preview(&[previous, day], self.through, resolver)?;
        let latest = slots
            .into_iter()
            .filter(|s| s.utc.is_some_and(|t| t <= now))
            .max_by_key(|s| s.utc);
        Ok(match (latest, blocker) {
            (None, _) => Recovery::NoMissedWindow,
            (Some(_), Some(reason)) => Recovery::Deferred(reason),
            (Some(slot), None) => Recovery::Offer(slot),
        })
    }
}

/// Append-only compare-and-create revisions. A partial last record is an
/// explicit error, never an implicit reset to disabled or an older cursor.
/// Each directory must be dedicated to this project's rotation operational state.
pub struct PolicyStore {
    directory: std::path::PathBuf,
}
impl PolicyStore {
    pub fn new(directory: impl Into<std::path::PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
    pub fn load(&self) -> Result<Option<Checkpoint>, String> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut revisions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let name = name.to_str().ok_or("Invalid rotation record name")?;
            if let Some(n) = name.strip_suffix(".rotation") {
                revisions.push(n.parse::<u64>().map_err(|_| "Invalid rotation revision")?);
            }
        }
        revisions.sort();
        for (i, revision) in revisions.iter().enumerate() {
            if *revision != i as u64 + 1 {
                return Err("Rotation revision history is incomplete".into());
            }
        }
        let Some(&revision) = revisions.last() else {
            return Ok(None);
        };
        let bytes = std::fs::read(self.directory.join(format!("{revision}.rotation")))
            .map_err(|e| e.to_string())?;
        let value = decode_checkpoint(&bytes)?;
        if value.revision != revision {
            return Err("Rotation revision mismatch".into());
        }
        Ok(Some(value))
    }
    pub fn save(&self, expected: u64, mut value: Checkpoint) -> Result<Checkpoint, String> {
        use std::io::Write;
        let current = self.load()?;
        if current.as_ref().map_or(0, |c| c.revision) != expected {
            return Err("Rotation revision conflict".into());
        }
        if let Some(current) = &current {
            if current.owner.origin != value.owner.origin
                || current.owner.project != value.owner.project
            {
                return Err("Rotation project identity mismatch".into());
            }
        }
        value.revision = expected
            .checked_add(1)
            .ok_or("Rotation revision exhausted")?;
        let bytes = encode_checkpoint(&value)?;
        decode_checkpoint(&bytes)?;
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        // Publish a fully flushed immutable record atomically. hard_link is
        // create-if-absent, so racing writers cannot overwrite a winning revision.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temporary = self.directory.join(format!(
            ".{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let cleanup = Cleanup(temporary);
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        std::fs::hard_link(
            &cleanup.0,
            self.directory.join(format!("{}.rotation", value.revision)),
        )
        .map_err(|e| e.to_string())?;
        drop(cleanup);
        #[cfg(unix)]
        std::fs::File::open(&self.directory)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(value)
    }
    /// Explicit opt-in/configuration action. The enrollment instant becomes the
    /// cursor: enabling never backfills windows from before consent.
    pub fn configure(
        &self,
        expected: u64,
        owner: Owner,
        policy: Policy,
        now: i64,
        resolver: &TzifResolver,
    ) -> Result<Checkpoint, String> {
        let day = resolver.local_day(policy.timezone(), now)?;
        policy.preview(&[day], now, resolver)?;
        if let Some(previous) = self.load()? {
            if previous.pending.is_some() {
                return Err("Reconcile pending rotation before changing policy".into());
            }
            if now < previous.through {
                return Err("Clock moved behind recovery cursor".into());
            }
            if previous.policy != policy && previous.owner.revision == owner.revision {
                return Err("Policy changes require a new owner revision".into());
            }
        }
        self.save(
            expected,
            Checkpoint {
                revision: expected,
                owner,
                policy,
                through: now,
                pending: None,
            },
        )
    }
    /// Persist the safety skip so clearing a blocker cannot immediately retry an
    /// old window. Only a later slot can become an offer.
    pub fn defer(
        &self,
        expected: u64,
        owner: &Owner,
        now: i64,
        resolver: &TzifResolver,
        blocker: Blocker,
    ) -> Result<Checkpoint, String> {
        let mut value = self.load()?.ok_or("Rotation is not configured")?;
        if value.revision != expected {
            return Err("Rotation revision conflict".into());
        }
        if !matches!(
            value.recover(owner, now, resolver, Some(blocker))?,
            Recovery::Deferred(_)
        ) {
            return Err("No deferrable rotation window".into());
        }
        value.through = now;
        self.save(expected, value)
    }
    /// Revalidates the offer, persists its identity, then allows draft preparation.
    pub fn claim(
        &self,
        expected: u64,
        owner: &Owner,
        now: i64,
        resolver: &TzifResolver,
        blocker: Option<Blocker>,
    ) -> Result<Checkpoint, String> {
        let mut value = self.load()?.ok_or("Rotation is not configured")?;
        if value.revision != expected {
            return Err("Rotation revision conflict".into());
        }
        let Recovery::Offer(slot) = value.recover(owner, now, resolver, blocker)? else {
            return Err("No claimable rotation window".into());
        };
        value.through = now;
        value.pending = Some(slot);
        self.save(expected, value)
    }
    /// Explicit acknowledgement of draft preparation/reconciliation; never a
    /// claim that a remote manager rotation has actually completed.
    pub fn acknowledge(&self, expected: u64, owner: &Owner) -> Result<Checkpoint, String> {
        let mut value = self.load()?.ok_or("Rotation is not configured")?;
        if value.revision != expected || &value.owner != owner {
            return Err("Stale rotation acknowledgement".into());
        }
        if value.pending.take().is_none() {
            return Err("No pending rotation claim".into());
        }
        self.save(expected, value)
    }
}
fn encode_checkpoint(value: &Checkpoint) -> Result<Vec<u8>, String> {
    let mut fields = vec![
        "okilum-rotation-v1".to_string(),
        value.revision.to_string(),
        value.owner.origin.clone(),
        value.owner.project.clone(),
        value.owner.manager.clone(),
        value.owner.revision.to_string(),
        value.policy.enabled.to_string(),
        value.policy.timezone.clone(),
        value.policy.minutes[0].to_string(),
        value.policy.minutes[1].to_string(),
        value.through.to_string(),
    ];
    if let Some(slot) = &value.pending {
        fields.extend([
            slot.day.to_string(),
            slot.minute.to_string(),
            slot.utc
                .ok_or("Pending slot has no UTC instant")?
                .to_string(),
        ]);
    }
    if fields
        .iter()
        .any(|f| f.contains(['\n', '\r']) || f.len() > 4096)
    {
        return Err("Invalid rotation record field".into());
    }
    Ok((fields.join("\n") + "\n").into_bytes())
}
fn decode_checkpoint(bytes: &[u8]) -> Result<Checkpoint, String> {
    if bytes.len() > 65536 || !bytes.ends_with(b"\n") {
        return Err("Incomplete rotation record".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid rotation record encoding")?;
    let f: Vec<_> = text.lines().collect();
    if ![11, 14].contains(&f.len()) || f[0] != "okilum-rotation-v1" {
        return Err("Unsupported rotation record".into());
    }
    fn num<T: std::str::FromStr>(s: &str) -> Result<T, String> {
        s.parse()
            .map_err(|_| "Invalid rotation record number".into())
    }
    let minutes: [u16; 2] = [num(f[8])?, num(f[9])?];
    let times = minutes.map(|m| format!("{:02}:{:02}", m / 60, m % 60));
    let policy = Policy::new(num(f[6])?, f[7], [&times[0], &times[1]])?;
    let value = Checkpoint {
        revision: num(f[1])?,
        owner: Owner {
            origin: f[2].into(),
            project: f[3].into(),
            manager: f[4].into(),
            revision: num(f[5])?,
        },
        policy,
        through: num(f[10])?,
        pending: if f.len() == 14 {
            Some(Slot {
                day: num(f[11])?,
                minute: num(f[12])?,
                utc: Some(num(f[13])?),
            })
        } else {
            None
        },
    };
    if encode_checkpoint(&value)? != bytes {
        return Err("Noncanonical rotation record".into());
    }
    Ok(value)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    fn owner() -> Owner {
        Owner {
            origin: "fixture".into(),
            project: "p".into(),
            manager: "m".into(),
            revision: 1,
        }
    }
    fn jerusalem_resolver() -> TzifResolver {
        TzifResolver::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zoneinfo"),
        )
    }
    #[test]
    fn actual_jerusalem_dst_and_coverage_fail_closed() {
        let resolver = jerusalem_resolver();
        assert!(matches!(
            resolver.resolve("Asia/Jerusalem", 20539, 150).unwrap(),
            LocalTime::Gap
        ));
        let LocalTime::Fold(a, b) = resolver.resolve("Asia/Jerusalem", 20751, 90).unwrap() else {
            panic!("Expected Israel autumn fold")
        };
        assert_eq!(b - a, 3600);
        assert!(matches!(
            resolver.resolve("Asia/Jerusalem", 20758, 510).unwrap(),
            LocalTime::Unique(_)
        ));
        assert!(resolver.resolve("../etc/passwd", 20758, 510).is_err());
        assert!(resolver.resolve("Asia/Jerusalem", i64::MAX, 510).is_err());
        assert!(Zone::parse(b"TZif").is_err());
    }
    #[test]
    fn durable_claim_restart_skip_and_revision_conflict() {
        let path = std::env::temp_dir().join(format!("rotation-recovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let store = PolicyStore::new(&path);
        let resolver = jerusalem_resolver();
        let p = Policy::new(true, "Asia/Jerusalem", ["08:30", "20:30"]).unwrap();
        let start = 20758 * 86400;
        let configured = store.configure(0, owner(), p, start, &resolver).unwrap();
        assert_eq!(configured.revision, 1);
        let now = start + 3 * 86400 + 22 * 3600;
        let Recovery::Offer(slot) = configured.recover(&owner(), now, &resolver, None).unwrap()
        else {
            panic!("Expected latest missed slot")
        };
        assert_eq!(slot.day, 20761);
        assert_eq!(slot.minute, 1230);
        let claimed = store.claim(1, &owner(), now, &resolver, None).unwrap();
        assert!(store.claim(1, &owner(), now, &resolver, None).is_err());
        let restart = PolicyStore::new(&path).load().unwrap().unwrap();
        assert_eq!(restart, claimed);
        assert!(matches!(
            restart.recover(&owner(), now, &resolver, None).unwrap(),
            Recovery::NeedsReconciliation(_)
        ));
        let ack = store.acknowledge(2, &owner()).unwrap();
        assert_eq!(
            ack.recover(&owner(), now, &resolver, None).unwrap(),
            Recovery::NoMissedWindow
        );
        let next = now + 86400;
        let deferred = store
            .defer(3, &owner(), next, &resolver, Blocker::PendingQuestion)
            .unwrap();
        assert_eq!(
            deferred.recover(&owner(), next, &resolver, None).unwrap(),
            Recovery::NoMissedWindow
        );
        assert!(deferred.recover(&owner(), start, &resolver, None).is_err());
        let mut stale = owner();
        stale.revision += 1;
        assert!(deferred.recover(&stale, next, &resolver, None).is_err());
        std::fs::write(path.join("5.rotation"), b"interrupted").unwrap();
        assert!(store.load().is_err());
        assert!(store.save(4, deferred).is_err());
        assert_eq!(
            std::fs::read(path.join("5.rotation")).unwrap(),
            b"interrupted"
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
