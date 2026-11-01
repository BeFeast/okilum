//! Pure opt-in rotation preview. No clock, timer, transport or source writes.
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
