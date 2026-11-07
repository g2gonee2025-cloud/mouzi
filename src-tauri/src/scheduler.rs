use crate::db::{
    claim_schedule_occurrences, finish_schedule_run, get_schedule_last_check, get_settings,
    get_watched_folders, is_folder_auto_mode, start_schedule_run, update_schedule_settings,
    AppSettings, ScheduleRunProgress, ScheduleSettings,
};
use crate::rules::{scheduled_scan_folder_if_current, WatchedRoot};
use crate::suppress::SuppressionSet;
use chrono::{DateTime, Days, Duration, Local, LocalResult, NaiveDate, NaiveTime, Offset, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration as StdDuration;
use tauri::{AppHandle, Emitter};

const CHECK_INTERVAL_SECONDS: u64 = 60;
// Missed slots trigger a scan of current files. At most one week of slots is
// needed to trigger that scan, even after a long shutdown or a bad clock jump.
const CATCH_UP_DAYS: u64 = 7;

/// The persisted watermark is an instant, so suspension and a clock/time-zone
/// change do not turn the interval into an arbitrary one-minute window.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScheduleCheckpoint {
    checked_at: DateTime<Utc>,
    configuration: Option<String>,
    #[serde(default)]
    local_offset_seconds: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScheduleOccurrence {
    key: String,
    scheduled_at: DateTime<Utc>,
}

struct SchedulePlan {
    occurrences: Vec<ScheduleOccurrence>,
    checkpoint: ScheduleCheckpoint,
}

/// Background scheduler. Occurrence claims live in SQLite rather than in the
/// thread, so restarting the application cannot repeat a claimed occurrence.
pub struct Scheduler {
    revision: Arc<AtomicU64>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self { revision: Arc::new(AtomicU64::new(0)) }
    }

    pub(crate) fn save_schedule(&self, suppression: &SuppressionSet, schedule: &ScheduleSettings) -> Result<AppSettings, String> {
        validate_schedule_times(schedule)?;
        let _operation = suppression.with_operation();
        let committed = update_schedule_settings(schedule).map_err(|error| error.to_string())?;
        self.revision.fetch_add(1, Ordering::AcqRel);
        Ok(committed)
    }

    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn start(&self, app_handle: AppHandle, suppression: Arc<SuppressionSet>) {
        let revision = self.revision.clone();
        thread::spawn(move || loop {
            // Check immediately on startup, then include the entire interval
            // since the persisted check on each wake-up.
            if let Err(e) = check_and_run(&app_handle, &suppression, &revision) {
                eprintln!("[scheduler] error: {}", e);
            }
            thread::sleep(StdDuration::from_secs(CHECK_INTERVAL_SECONDS));
        });
    }
}

pub(crate) fn schedule_from_settings(settings: AppSettings) -> ScheduleSettings {
    ScheduleSettings {
        schedule_enabled: settings.schedule_enabled,
        schedule_times_per_day: settings.schedule_times_per_day,
        schedule_time_1: settings.schedule_time_1,
        schedule_time_2: settings.schedule_time_2,
        schedule_time_3: settings.schedule_time_3,
        schedule_time_4: settings.schedule_time_4,
    }
}

/// Validate the schedule before saving it. An inactive schedule may have empty
/// slots, but any supplied value must still be a valid local wall-clock time.
pub fn validate_schedule_times(schedule: &ScheduleSettings) -> Result<(), String> {
    if !(1..=4).contains(&schedule.schedule_times_per_day) {
        return Err("Schedule times per day must be between 1 and 4".to_string());
    }
    let count = schedule.schedule_times_per_day as usize;
    let mut active_times = Vec::with_capacity(count);
    for (index, value) in schedule_values(schedule).iter().enumerate() {
        let Some(value) = value.as_deref() else {
            if schedule.schedule_enabled && index < count {
                return Err(format!("Schedule time {} is required", index + 1));
            }
            continue;
        };
        let time = parse_time(value)?;
        if index < count {
            if active_times.contains(&time) {
                return Err("Active schedule times must be distinct".to_string());
            }
            active_times.push(time);
        }
    }
    Ok(())
}

fn schedule_values(schedule: &ScheduleSettings) -> [Option<&str>; 4] {
    [
        schedule.schedule_time_1.as_deref(),
        schedule.schedule_time_2.as_deref(),
        schedule.schedule_time_3.as_deref(),
        schedule.schedule_time_4.as_deref(),
    ]
}

fn parse_time(value: &str) -> Result<NaiveTime, String> {
    let time = value.trim();
    let bytes = time.as_bytes();
    let has_shape = matches!(bytes.len(), 5 | 8)
        && bytes[2] == b':'
        && (bytes.len() == 5 || bytes[5] == b':')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 2 || (index == 5 && bytes.len() == 8) || byte.is_ascii_digit());
    if !has_shape {
        return Err(format!("Invalid schedule time '{}': use HH:MM or HH:MM:SS", value));
    }
    let hour = time[0..2].parse::<u32>().map_err(|e| e.to_string())?;
    let minute = time[3..5].parse::<u32>().map_err(|e| e.to_string())?;
    let second = if bytes.len() == 8 {
        time[6..8].parse::<u32>().map_err(|e| e.to_string())?
    } else {
        0
    };
    NaiveTime::from_hms_opt(hour, minute, second)
        .ok_or_else(|| format!("Invalid schedule time '{}': time is out of range", value))
}

fn active_times(schedule: &ScheduleSettings) -> Result<Vec<NaiveTime>, String> {
    validate_schedule_times(schedule)?;
    if !schedule.schedule_enabled {
        return Ok(Vec::new());
    }
    let mut times = schedule_values(schedule)
        .into_iter()
        .take(schedule.schedule_times_per_day as usize)
        .map(|value| parse_time(value.ok_or_else(|| "Schedule time is required".to_string())?))
        .collect::<Result<Vec<_>, _>>()?;
    times.sort_unstable();
    Ok(times)
}

fn configuration_key(times: &[NaiveTime]) -> Option<String> {
    if times.is_empty() {
        return None;
    }
    // A preference revision and slot order are not schedule identity. Using
    // normalized active times also treats 08:00 and 08:00:00 as the same time.
    Some(
        times
            .iter()
            .map(|time| time.format("%H:%M:%S").to_string())
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn scheduled_instant<Tz: TimeZone>(
    timezone: &Tz,
    date: NaiveDate,
    time: NaiveTime,
) -> Option<DateTime<Utc>> {
    let local = date.and_time(time);
    // During a repeated DST hour use its first instant, once per local date.
    // During a skipped hour run at the next valid local minute. A bad slot
    // must not prevent another valid slot from running that day.
    for minutes in 0..=180 {
        let candidate = local.checked_add_signed(Duration::minutes(minutes))?;
        match timezone.from_local_datetime(&candidate) {
            LocalResult::Single(value) => return Some(value.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, second) => {
                return Some(first.min(second).with_timezone(&Utc));
            }
            LocalResult::None => {}
        }
    }
    None
}

/// Pure planning: tests supply a clock and timezone without starting a thread,
/// observing folders, or touching the global application database.
fn plan_schedule<Tz: TimeZone>(
    schedule: &ScheduleSettings,
    previous: Option<&ScheduleCheckpoint>,
    now: DateTime<Tz>,
) -> Result<SchedulePlan, String> {
    let times = active_times(schedule)?;
    let configuration = configuration_key(&times);
    let now_utc = now.with_timezone(&Utc);
    let local_offset_seconds = now.offset().fix().local_minus_utc();
    let continuing = previous.filter(|checkpoint| {
        checkpoint.configuration == configuration && checkpoint.checked_at <= now_utc
    });
    let mut plan = SchedulePlan {
        occurrences: Vec::new(),
        checkpoint: ScheduleCheckpoint {
            checked_at: now_utc,
            configuration,
            local_offset_seconds: Some(local_offset_seconds),
        },
    };
    if times.is_empty() {
        // Disabling still advances the checkpoint, so enabling later cannot
        // replay several days that were intentionally disabled.
        return Ok(plan);
    }

    let timezone = now.timezone();
    let today = now.date_naive();
    let from = continuing.map(|checkpoint| checkpoint.checked_at);
    // First use, a configuration edit, or a future watermark revisits today's
    // due slots. An offset change also revisits today while preserving the
    // elapsed interval for older missed dates. Durable claims prevent repeats.
    let offset_changed = continuing
        .and_then(|checkpoint| checkpoint.local_offset_seconds)
        .is_some_and(|offset| offset != local_offset_seconds);
    let oldest_date = today
        .checked_sub_days(Days::new(CATCH_UP_DAYS - 1))
        .unwrap_or(NaiveDate::MIN);
    let mut date = from
        .map(|instant| instant.with_timezone(&timezone).date_naive().min(today))
        .unwrap_or(today)
        .max(oldest_date);
    while date <= today {
        for time in &times {
            let Some(scheduled_at) = scheduled_instant(&timezone, date, *time) else {
                continue;
            };
            if scheduled_at <= now_utc
                && (from.is_none_or(|instant| scheduled_at > instant)
                    || (offset_changed && date == today))
            {
                plan.occurrences.push(ScheduleOccurrence {
                    key: format!("v2|{}|{}", date.format("%Y-%m-%d"), time.format("%H:%M:%S")),
                    scheduled_at,
                });
            }
        }
        let Some(next) = date.succ_opt() else {
            break;
        };
        date = next;
    }
    plan.occurrences.sort_by_key(|occurrence| occurrence.scheduled_at);
    Ok(plan)
}

fn claim_due_occurrences<T>(
    occurrences: &[ScheduleOccurrence],
    claim: impl FnOnce(&[&str]) -> Result<T, String>,
) -> Result<T, String> {
    let keys = occurrences.iter().map(|occurrence| occurrence.key.as_str()).collect::<Vec<_>>();
    claim(&keys)
}

fn check_and_run(app_handle: &AppHandle, suppression: &SuppressionSet, revision: &AtomicU64) -> Result<(), String> {
    // Snapshot and claim under the same gate used to save the schedule. Each
    // subsequent file operation checks this revision under that gate again.
    let operation = suppression.with_operation();
    let expected_revision = revision.load(Ordering::Acquire);
    let schedule = get_settings().map(schedule_from_settings).map_err(|e| e.to_string())?;
    let saved_checkpoint = get_schedule_last_check().map_err(|e| e.to_string())?;
    let previous = saved_checkpoint.as_deref().and_then(|value| {
        serde_json::from_str::<ScheduleCheckpoint>(value)
            .map_err(|error| eprintln!("[scheduler] ignoring invalid checkpoint: {}", error))
            .ok()
    });
    let plan = plan_schedule(&schedule, previous.as_ref(), Local::now())?;
    let checkpoint = serde_json::to_string(&plan.checkpoint).map_err(|e| e.to_string())?;
    let run = claim_due_occurrences(&plan.occurrences, |keys| {
        claim_schedule_occurrences(keys, &checkpoint).map_err(|e| e.to_string())
    })?;
    let Some(run) = run else { return Ok(()); };
    start_schedule_run(run.id).map_err(|error| format!("Cannot start claimed run {}: {error}", run.id))?;
    drop(operation);

    // Commit claims and the checkpoint before moving a file so a crash cannot
    // repeat a partly completed occurrence. One scan covers all newly due slots.
    let progress = perform_scheduled_clean(suppression, run.id, &|| revision.load(Ordering::Acquire) == expected_revision);
    let finished = finish_schedule_run(run.id, &progress)
        .map_err(|error| format!("Cannot save result for scheduled run {}: {error}", run.id));
    let _ = app_handle.emit(
        "scheduled-clean-done",
        json!({ "id": run.id, "organized": progress.organized, "occurrences": run.occurrences,
                "superseded": progress.revoked, "result_saved": finished.is_ok() }),
    );
    let _ = app_handle.emit("mutation-completed", ());
    finished
}

fn perform_scheduled_clean(
    suppression: &SuppressionSet,
    run_id: i64,
    is_current: &dyn Fn() -> bool,
) -> ScheduleRunProgress {
    let mut progress = ScheduleRunProgress::default();
    let folders = match get_watched_folders() {
        Ok(folders) => folders,
        Err(error) => {
            progress.record_error(format!("Cannot read scheduled folders: {error}"));
            return progress;
        }
    };
    // Capture every root before the first move so a later folder cannot be
    // replaced and acquire authority partway through this run.
    let mut roots = Vec::new();
    for folder in folders.iter().filter(|folder| folder.enabled && is_folder_auto_mode(&folder.mode)) {
        match WatchedRoot::capture(folder) {
            Ok(root) => roots.push(root),
            Err(error) => progress.record_error(format!("{}: {error}", folder.path)),
        }
    }
    for root in roots {
        if !is_current() {
            progress.revoked = true;
            break;
        }
        // This unattended path applies grace and settling checks and rechecks
        // current root eligibility for each file. Never use Clean Now's bypass.
        scheduled_scan_folder_if_current(&root, suppression, run_id, is_current, &mut progress);
        if progress.revoked {
            break;
        }
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDateTime};
    use std::collections::HashSet;

    fn schedule(times: &[&str]) -> ScheduleSettings {
        ScheduleSettings {
            schedule_enabled: true,
            schedule_times_per_day: times.len() as i64,
            schedule_time_1: times.first().map(|value| value.to_string()),
            schedule_time_2: times.get(1).map(|value| value.to_string()),
            schedule_time_3: times.get(2).map(|value| value.to_string()),
            schedule_time_4: times.get(3).map(|value| value.to_string()),
        }
    }

    fn now(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    fn checkpoint(schedule: &ScheduleSettings, checked_at: &str) -> ScheduleCheckpoint {
        let checked_at = now(checked_at);
        ScheduleCheckpoint {
            checked_at: checked_at.with_timezone(&Utc),
            configuration: configuration_key(&active_times(schedule).unwrap()),
            local_offset_seconds: Some(checked_at.offset().local_minus_utc()),
        }
    }

    fn record_claims(claims: &mut HashSet<String>, keys: &[&str]) -> Result<usize, String> {
        let before = claims.len();
        claims.extend(keys.iter().map(|&key| key.to_owned()));
        Ok(claims.len() - before)
    }

    #[test]
    fn schedule_rejects_invalid_counts_missing_times_and_normalized_duplicates() {
        for invalid in ["24:00", "12:60", "23:59:60", "8:00", "08:3", "08:00 PM", ""] {
            assert!(validate_schedule_times(&schedule(&[invalid])).is_err(), "{invalid}");
        }
        assert!(validate_schedule_times(&schedule(&["08:00", "08:00:00"])).is_err());
        let mut missing = schedule(&["08:00"]);
        missing.schedule_times_per_day = 2;
        assert!(validate_schedule_times(&missing).is_err());
        missing.schedule_enabled = false;
        assert!(validate_schedule_times(&missing).is_ok());
        missing.schedule_times_per_day = 5;
        assert!(validate_schedule_times(&missing).is_err());
        missing.schedule_times_per_day = 0;
        assert!(validate_schedule_times(&missing).is_err());
        assert!(validate_schedule_times(&schedule(&[" 08:00 ", "14:15:30"])).is_ok());
    }

    #[test]
    fn wake_up_catches_the_whole_interval_and_includes_the_exact_due_instant() {
        let schedule = schedule(&["09:00", "12:00"]);
        let previous = checkpoint(&schedule, "2026-10-03T08:59:00+04:00");
        let plan = plan_schedule(&schedule, Some(&previous), now("2026-10-03T12:00:00+04:00")).unwrap();
        assert_eq!(plan.occurrences.len(), 2);
        assert!(plan.occurrences[0].key.ends_with("|2026-10-03|09:00:00"));
        let after = plan_schedule(&schedule, Some(&plan.checkpoint), now("2026-10-03T12:15:00+04:00")).unwrap();
        assert!(after.occurrences.is_empty());
    }

    #[test]
    fn restart_catch_up_claims_each_local_date_and_time_once() {
        let schedule = schedule(&["00:00", "23:59"]);
        let previous = checkpoint(&schedule, "2026-10-01T23:50:00+04:00");
        let time = now("2026-10-03T00:02:00+04:00");
        let plan = plan_schedule(&schedule, Some(&previous), time).unwrap();
        assert_eq!(plan.occurrences.len(), 4);
        let mut claims = HashSet::new();
        assert_eq!(claim_due_occurrences(&plan.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 4);
        // Simulate a restart before the new checkpoint was saved. Persistent
        // claims still make the same missed interval a no-op.
        let persisted = serde_json::to_string(&previous).unwrap();
        let restored = serde_json::from_str::<ScheduleCheckpoint>(&persisted).unwrap();
        let replay = plan_schedule(&schedule, Some(&restored), time).unwrap();
        assert_eq!(claim_due_occurrences(&replay.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 0);
    }

    #[test]
    fn first_use_and_schedule_edits_catch_up_today_with_stable_time_keys() {
        let original = schedule(&["08:00", "14:00"]);
        let reordered = schedule(&["14:00:00", "08:00:00"]);
        let time = now("2026-10-03T16:00:00+04:00");
        let fresh = plan_schedule(&original, None, time).unwrap();
        assert_eq!(fresh.occurrences.len(), 2);
        let reordered_plan = plan_schedule(&reordered, None, time).unwrap();
        assert_eq!(fresh.occurrences, reordered_plan.occurrences);
        let continuing = plan_schedule(&reordered, Some(&fresh.checkpoint), time).unwrap();
        assert!(continuing.occurrences.is_empty());

        let edited = schedule(&["10:00", "14:00"]);
        let edit_plan = plan_schedule(&edited, Some(&fresh.checkpoint), time).unwrap();
        assert_eq!(edit_plan.occurrences.len(), 2);
        assert_eq!(fresh.occurrences[1].key, edit_plan.occurrences[1].key);
        assert!(edit_plan.occurrences.iter().all(|occurrence| occurrence.key.contains("|2026-10-03|")));
        let mut claims = HashSet::new();
        assert_eq!(claim_due_occurrences(&fresh.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 2);
        assert_eq!(claim_due_occurrences(&edit_plan.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 1);

        let later = schedule(&["08:00", "14:00", "20:00"]);
        let add_later = plan_schedule(&later, Some(&fresh.checkpoint), time).unwrap();
        assert_eq!(claim_due_occurrences(&add_later.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 0);
    }

    #[test]
    fn disabled_schedule_advances_the_checkpoint_without_replaying_earlier_days() {
        let active = schedule(&["08:00"]);
        let old = checkpoint(&active, "2026-10-01T00:00:00+04:00");
        let mut disabled = active.clone();
        disabled.schedule_enabled = false;
        let time = now("2026-10-03T16:00:00+04:00");
        let stopped = plan_schedule(&disabled, Some(&old), time).unwrap();
        assert!(stopped.occurrences.is_empty());
        assert_eq!(stopped.checkpoint.checked_at, time.with_timezone(&Utc));
        let enabled = plan_schedule(&active, Some(&stopped.checkpoint), time).unwrap();
        assert_eq!(enabled.occurrences.len(), 1);
        assert!(enabled.occurrences[0].key.contains("|2026-10-03|"));
    }

    #[test]
    fn clock_rollback_reanchors_and_durable_claims_prevent_repeating_a_slot() {
        let schedule = schedule(&["08:00"]);
        let first = plan_schedule(&schedule, None, now("2026-10-03T10:00:00+04:00")).unwrap();
        let mut claims = HashSet::new();
        assert_eq!(claim_due_occurrences(&first.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 1);
        let corrected = now("2026-10-03T07:30:00+04:00");
        let rolled_back = plan_schedule(&schedule, Some(&first.checkpoint), corrected).unwrap();
        assert!(rolled_back.occurrences.is_empty());
        assert_eq!(rolled_back.checkpoint.checked_at, corrected.with_timezone(&Utc));
        let forward = plan_schedule(&schedule, Some(&rolled_back.checkpoint), now("2026-10-03T10:30:00+04:00")).unwrap();
        assert_eq!(claim_due_occurrences(&forward.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 0);
    }

    #[test]
    fn long_catch_up_is_bounded_and_a_forward_clock_jump_cannot_block_new_slots() {
        let schedule = schedule(&["00:00", "08:00", "16:00", "23:59:59"]);
        let original = checkpoint(&schedule, "2026-10-01T07:59:00+04:00");
        let jumped = plan_schedule(&schedule, Some(&original), now("2027-10-03T23:59:59+04:00")).unwrap();
        assert_eq!(jumped.occurrences.len(), 28);
        assert_eq!(jumped.occurrences[0].key, "v2|2027-09-27|00:00:00");
        let mut claims = HashSet::new();
        assert_eq!(claim_due_occurrences(&jumped.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 28);

        let recovered = plan_schedule(&schedule, Some(&jumped.checkpoint), now("2026-10-01T08:00:00+04:00")).unwrap();
        assert_eq!(recovered.occurrences.last().unwrap().key, "v2|2026-10-01|08:00:00");
        assert_eq!(claim_due_occurrences(&recovered.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 2);
    }

    #[test]
    fn changing_offset_can_make_a_local_slot_due_at_the_same_utc_instant() {
        let schedule = schedule(&["09:00"]);
        let old = checkpoint(&schedule, "2026-10-03T08:00:00+04:00");
        let changed = plan_schedule(&schedule, Some(&old), now("2026-10-03T10:00:00+06:00")).unwrap();
        assert_eq!(changed.occurrences.len(), 1);
        assert_eq!(changed.occurrences[0].key, "v2|2026-10-03|09:00:00");

        let legacy = serde_json::json!({ "checked_at": old.checked_at, "configuration": old.configuration });
        let legacy: ScheduleCheckpoint = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.local_offset_seconds, None);
    }

    // Fixed 2026 Eastern time transitions keep the test independent of the
    // host clock and time-zone database.
    #[derive(Clone)]
    struct Eastern2026;

    impl TimeZone for Eastern2026 {
        type Offset = FixedOffset;

        fn from_offset(_: &Self::Offset) -> Self { Self }

        fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<Self::Offset> {
            self.offset_from_local_datetime(&local.and_time(NaiveTime::MIN))
        }

        fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<Self::Offset> {
            let standard = FixedOffset::west_opt(5 * 3600).unwrap();
            let daylight = FixedOffset::west_opt(4 * 3600).unwrap();
            let valid = |offset: FixedOffset| {
                let instant = *local - Duration::seconds(i64::from(offset.local_minus_utc()));
                self.offset_from_utc_datetime(&instant) == offset
            };
            match (valid(daylight), valid(standard)) {
                (true, true) => LocalResult::Ambiguous(daylight, standard),
                (true, false) => LocalResult::Single(daylight),
                (false, true) => LocalResult::Single(standard),
                (false, false) => LocalResult::None,
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> Self::Offset {
            self.offset_from_utc_datetime(&utc.and_time(NaiveTime::MIN))
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> Self::Offset {
            let spring = Utc.with_ymd_and_hms(2026, 3, 8, 7, 0, 0).unwrap().naive_utc();
            let fall = Utc.with_ymd_and_hms(2026, 11, 1, 6, 0, 0).unwrap().naive_utc();
            let hours_west = if (spring..fall).contains(utc) { 4 } else { 5 };
            FixedOffset::west_opt(hours_west * 3600).unwrap()
        }
    }

    #[test]
    fn daylight_saving_gap_and_fold_keep_one_claim_per_local_slot() {
        let timezone = Eastern2026;
        let spring = timezone.with_ymd_and_hms(2026, 3, 8, 3, 0, 0).unwrap();
        let gap = plan_schedule(&schedule(&["02:30"]), None, spring.clone()).unwrap();
        assert_eq!(gap.occurrences.len(), 1);
        assert_eq!(gap.occurrences[0].key, "v2|2026-03-08|02:30:00");
        assert_eq!(gap.occurrences[0].scheduled_at, spring.with_timezone(&Utc));

        let fold = schedule(&["01:30"]);
        let repeated = timezone.with_ymd_and_hms(2026, 11, 1, 1, 30, 0);
        let first = plan_schedule(&fold, None, repeated.clone().earliest().unwrap()).unwrap();
        assert_eq!(first.occurrences.len(), 1);
        assert_eq!(first.occurrences[0].scheduled_at, Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap());
        let mut claims = HashSet::new();
        assert_eq!(claim_due_occurrences(&first.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 1);
        let second = plan_schedule(&fold, Some(&first.checkpoint), repeated.latest().unwrap()).unwrap();
        assert_eq!(claim_due_occurrences(&second.occurrences, |keys| record_claims(&mut claims, keys)).unwrap(), 0);
    }
}
