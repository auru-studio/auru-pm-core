use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

const QUIET_PERIOD: Duration = Duration::from_secs(5 * 60);

/// The filesystem facts the automatic-backup policy needs for one project.
pub struct ProjectObservation {
    pub project_id: String,
    pub modified_at: Option<SystemTime>,
    pub backed_up_at: Option<SystemTime>,
    pub backup_destination_ready: bool,
    pub backup_blocked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticBackupReason {
    SavedAfterLastBackup,
    SavedDuringPreviousBackup,
}

#[derive(Debug, Eq, PartialEq)]
pub struct AutomaticBackupCandidate {
    pub project_id: String,
    pub reason: AutomaticBackupReason,
    /// The revision that completed the quiet period for this poll.
    pub qualified_revision: SystemTime,
}

/// Selects enrolled projects whose latest save has been quiet long enough.
#[derive(Default)]
pub struct AutomaticBackupScheduler {
    attempted_revisions: BTreeMap<String, SystemTime>,
}

impl AutomaticBackupScheduler {
    pub fn from_attempted_revisions(
        revisions: impl IntoIterator<Item = (String, SystemTime)>,
    ) -> Self {
        Self {
            attempted_revisions: revisions.into_iter().collect(),
        }
    }

    /// Remember the exact file revision handed to the coordinator.
    pub fn record_backup_attempt(
        &mut self,
        project_id: impl Into<String>,
        modified_at: Option<SystemTime>,
    ) {
        if let Some(modified_at) = modified_at {
            self.attempted_revisions
                .insert(project_id.into(), modified_at);
        }
    }

    pub fn poll(
        &self,
        now: SystemTime,
        observations: impl IntoIterator<Item = ProjectObservation>,
    ) -> Vec<AutomaticBackupCandidate> {
        let mut ready = Vec::new();
        for project in observations {
            if !project.backup_destination_ready || project.backup_blocked {
                continue;
            }
            let (Some(modified_at), Some(backed_up_at)) =
                (project.modified_at, project.backed_up_at)
            else {
                continue;
            };
            let attempted_revision = self.attempted_revisions.get(&project.project_id);
            let already_attempted = attempted_revision == Some(&modified_at);
            let changed_during_attempt =
                attempted_revision.is_some_and(|attempted| *attempted != modified_at);
            if already_attempted || (modified_at <= backed_up_at && !changed_during_attempt) {
                continue;
            }
            let quiet_long_enough = now
                .duration_since(modified_at)
                .is_ok_and(|elapsed| elapsed >= QUIET_PERIOD);
            if quiet_long_enough {
                let reason = if modified_at <= backed_up_at && changed_during_attempt {
                    AutomaticBackupReason::SavedDuringPreviousBackup
                } else {
                    AutomaticBackupReason::SavedAfterLastBackup
                };
                ready.push(AutomaticBackupCandidate {
                    project_id: project.project_id,
                    reason,
                    qualified_revision: modified_at,
                });
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{
        AutomaticBackupCandidate, AutomaticBackupReason, AutomaticBackupScheduler,
        ProjectObservation,
    };

    const SAVED_AT: SystemTime = SystemTime::UNIX_EPOCH;

    fn linked_project() -> ProjectObservation {
        ProjectObservation {
            project_id: "project:Song.dawproject".to_owned(),
            modified_at: Some(SAVED_AT),
            backed_up_at: Some(SAVED_AT - Duration::from_secs(60)),
            backup_destination_ready: true,
            backup_blocked: false,
        }
    }

    fn project_ids(candidates: Vec<AutomaticBackupCandidate>) -> Vec<String> {
        candidates
            .into_iter()
            .map(|candidate| candidate.project_id)
            .collect()
    }

    #[test]
    fn a_linked_project_should_wait_for_five_quiet_minutes() {
        let scheduler = AutomaticBackupScheduler::default();

        let early = scheduler.poll(SAVED_AT + Duration::from_secs(299), [linked_project()]);
        let ready = scheduler.poll(SAVED_AT + Duration::from_secs(300), [linked_project()]);

        assert!(early.is_empty());
        assert_eq!(ready[0].qualified_revision, SAVED_AT);
        assert_eq!(project_ids(ready), ["project:Song.dawproject"]);
    }

    #[test]
    fn one_saved_revision_should_only_be_selected_once() {
        let mut scheduler = AutomaticBackupScheduler::default();
        let ready_at = SAVED_AT + Duration::from_secs(300);

        let first = scheduler.poll(ready_at, [linked_project()]);
        scheduler.record_backup_attempt("project:Song.dawproject", Some(SAVED_AT));
        let repeated = scheduler.poll(ready_at + Duration::from_secs(10), [linked_project()]);

        assert_eq!(project_ids(first), ["project:Song.dawproject"]);
        assert!(repeated.is_empty());
    }

    #[test]
    fn a_later_save_should_begin_a_new_quiet_window() {
        let mut scheduler = AutomaticBackupScheduler::default();
        let first_ready_at = SAVED_AT + Duration::from_secs(300);
        assert_eq!(
            project_ids(scheduler.poll(first_ready_at, [linked_project()])),
            ["project:Song.dawproject"]
        );
        scheduler.record_backup_attempt("project:Song.dawproject", Some(SAVED_AT));
        let saved_again_at = first_ready_at + Duration::from_secs(1);
        let changed = ProjectObservation {
            modified_at: Some(saved_again_at),
            ..linked_project()
        };

        let early = scheduler.poll(
            saved_again_at + Duration::from_secs(299),
            [ProjectObservation {
                modified_at: Some(saved_again_at),
                ..linked_project()
            }],
        );
        let ready = scheduler.poll(saved_again_at + Duration::from_secs(300), [changed]);

        assert!(early.is_empty());
        assert_eq!(project_ids(ready), ["project:Song.dawproject"]);
    }

    #[test]
    fn a_project_should_need_an_existing_backup_before_it_is_automatic() {
        let scheduler = AutomaticBackupScheduler::default();
        let never_backed_up = ProjectObservation {
            backed_up_at: None,
            backup_destination_ready: false,
            ..linked_project()
        };

        let ready = scheduler.poll(SAVED_AT + Duration::from_secs(600), [never_backed_up]);

        assert!(ready.is_empty());
    }

    #[test]
    fn a_save_during_an_in_flight_backup_should_start_another_quiet_window() {
        let mut scheduler = AutomaticBackupScheduler::default();
        scheduler.record_backup_attempt("project:Song.dawproject", Some(SAVED_AT));
        let saved_during_upload = SAVED_AT + Duration::from_secs(60);
        let completion_time = SAVED_AT + Duration::from_secs(120);
        let changed = ProjectObservation {
            modified_at: Some(saved_during_upload),
            backed_up_at: Some(completion_time),
            ..linked_project()
        };

        let ready = scheduler.poll(saved_during_upload + Duration::from_secs(300), [changed]);

        assert_eq!(
            ready,
            [AutomaticBackupCandidate {
                project_id: "project:Song.dawproject".to_owned(),
                reason: AutomaticBackupReason::SavedDuringPreviousBackup,
                qualified_revision: saved_during_upload,
            }]
        );
    }

    #[test]
    fn attempted_revisions_should_restore_after_a_restart() {
        let project_id = "project:Song.dawproject";
        let scheduler =
            AutomaticBackupScheduler::from_attempted_revisions([(project_id.to_owned(), SAVED_AT)]);
        let saved_during_upload = SAVED_AT + Duration::from_secs(60);
        let changed = ProjectObservation {
            modified_at: Some(saved_during_upload),
            backed_up_at: Some(SAVED_AT + Duration::from_secs(120)),
            ..linked_project()
        };

        let ready = scheduler.poll(saved_during_upload + Duration::from_secs(300), [changed]);

        assert_eq!(
            ready,
            [AutomaticBackupCandidate {
                project_id: project_id.to_owned(),
                reason: AutomaticBackupReason::SavedDuringPreviousBackup,
                qualified_revision: saved_during_upload,
            }]
        );
    }
}
