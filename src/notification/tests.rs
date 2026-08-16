use super::*;

fn later(now: NotificationTime, seconds: u64) -> NotificationTime {
    let elapsed = Duration::from_secs(seconds);
    NotificationTime {
        monotonic: now.monotonic + elapsed,
        wall: now.wall + elapsed,
    }
}

fn notice(severity: Severity, summary: &str) -> Notice {
    Notice::new(NotificationSource::Editor, severity, summary)
}

#[test]
fn quiet_feedback_is_retained_without_unread_attention() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    for notice in [
        notice(Severity::Success, "saved"),
        notice(Severity::Info, "copied").with_attention(AttentionPolicy::Quiet),
    ] {
        center.publish(notice, now).unwrap();
    }
    for at in [now, later(now, 10)] {
        let counts = center.counts(at.monotonic);
        assert_eq!(counts.total, 2);
        assert_eq!(counts.unread, 0);
        assert_eq!(counts.unseen, 0);
        assert_eq!(counts.needs_acknowledgment, 0);
    }
}

#[test]
fn reading_information_does_not_acknowledge_a_warning() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let info = center
        .publish(notice(Severity::Info, "finished"), now)
        .unwrap();
    let warning = center
        .publish(notice(Severity::Warning, "check this"), now)
        .unwrap();
    assert_eq!(center.counts(now.monotonic).unseen, 1);
    assert_eq!(center.counts(now.monotonic).needs_acknowledgment, 1);
    center.mark_read(info).unwrap();
    center.mark_read(warning).unwrap();
    assert_eq!(center.counts(now.monotonic).unseen, 0);
    assert_eq!(center.counts(now.monotonic).needs_acknowledgment, 1);
    center.resolve(warning).unwrap();
    assert_eq!(center.counts(now.monotonic).needs_acknowledgment, 0);
}

#[test]
fn successful_progress_is_only_attention_worthy_if_missed() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = start(&mut center, "push", ProgressPriority::UserInitiated, now);
    assert_eq!(center.counts(now.monotonic).unseen, 0);
    center
        .finish_progress(
            id,
            ProgressOutcome::Succeeded,
            MessageContent::new("pushed"),
            now,
        )
        .unwrap();
    assert_eq!(center.get(id).unwrap().attention, AttentionPolicy::IfMissed);
    assert_eq!(center.counts(later(now, 10).monotonic).unseen, 1);
    center.mark_read(id).unwrap();
    assert_eq!(center.counts(later(now, 10).monotonic).unseen, 0);
}

fn start(
    center: &mut NotificationCenter,
    key: &str,
    priority: ProgressPriority,
    now: NotificationTime,
) -> NotificationId {
    center
        .begin_progress(
            NotificationSource::Editor,
            key,
            MessageContent::new(key),
            priority,
            now,
        )
        .unwrap()
}

#[test]
fn expiry_removes_a_notice_from_active_messages_but_not_history() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = center
        .publish(notice(Severity::Info, "saved"), now)
        .unwrap();

    assert_eq!(center.primary(now.monotonic).unwrap().id, id);
    assert_eq!(center.counts(later(now, 4).monotonic).active, 0);
    assert_eq!(center.counts(later(now, 4).monotonic).unread, 1);
    assert_eq!(center.get(id).unwrap().content.summary, "saved");
}

#[test]
fn acknowledgement_hides_an_error_without_deleting_it() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = center
        .publish(notice(Severity::Error, "failed"), now)
        .unwrap();

    center.mark_read(id).unwrap();
    assert_eq!(center.counts(later(now, 600).monotonic).active, 1);
    assert_eq!(center.counts(now.monotonic).unread, 0);
    center.acknowledge(id).unwrap();
    assert_eq!(center.counts(now.monotonic).active, 0);
    assert!(center.get(id).is_some());
}

#[test]
fn progress_updates_and_completion_retain_one_identity() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = start(&mut center, "push", ProgressPriority::UserInitiated, now);
    center.mark_read(id).unwrap();

    assert!(center
        .update_progress(id, MessageContent::new("pushing"), Some(20), later(now, 1))
        .unwrap());
    assert!(!center
        .update_progress(id, MessageContent::new("pushing"), Some(20), later(now, 2))
        .unwrap());
    assert_eq!(center.get(id).unwrap().updated_at, later(now, 1).wall);
    assert_eq!(center.counts(now.monotonic).unread, 0);
    center.acknowledge(id).unwrap();
    assert_eq!(center.counts(now.monotonic).running, 1);
    assert_eq!(center.resolve(id), Err(NotificationError::StillRunning));

    center
        .finish_progress(
            id,
            ProgressOutcome::Succeeded,
            MessageContent::new("pushed"),
            later(now, 3),
        )
        .unwrap();

    assert_eq!(center.records().len(), 1);
    assert_eq!(center.get(id).unwrap().created_at, now.wall);
    assert_eq!(center.counts(later(now, 3).monotonic).running, 0);
    assert_eq!(center.counts(later(now, 3).monotonic).unread, 1);
    assert_eq!(center.counts(later(now, 7).monotonic).active, 0);
    assert_eq!(
        center.update_progress(id, MessageContent::new("late"), None, later(now, 8)),
        Err(NotificationError::NotRunning)
    );
}

#[test]
fn failed_progress_stays_active_until_acknowledged() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = start(&mut center, "push", ProgressPriority::UserInitiated, now);
    center
        .finish_progress(
            id,
            ProgressOutcome::Failed,
            MessageContent::new("push rejected").with_details("remote error\nfull output"),
            now,
        )
        .unwrap();

    assert_eq!(center.get(id).unwrap().severity, Severity::Error);
    assert_eq!(center.counts(later(now, 600).monotonic).active, 1);
    center.acknowledge(id).unwrap();
    assert_eq!(center.counts(now.monotonic).active, 0);
    assert_eq!(
        center.get(id).unwrap().content.details.as_deref(),
        Some("remote error\nfull output")
    );
}

#[test]
fn keyed_notices_coalesce_only_while_active() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = center
        .publish(
            notice(Severity::Warning, "snapshot failed").with_key("snapshot"),
            now,
        )
        .unwrap();
    center.mark_read(id).unwrap();
    let repeated = center
        .publish(
            notice(Severity::Warning, "snapshot still failing").with_key("snapshot"),
            later(now, 1),
        )
        .unwrap();

    assert_eq!(repeated, id);
    assert_eq!(center.get(id).unwrap().occurrences, 2);
    assert!(!center.get(id).unwrap().read);
    center.resolve(id).unwrap();
    assert!(!center.get(id).unwrap().read);
    let recurrence = center
        .publish(
            notice(Severity::Warning, "snapshot failed").with_key("snapshot"),
            later(now, 2),
        )
        .unwrap();
    assert_ne!(recurrence, id);
    assert_eq!(center.records().len(), 2);
}

#[test]
fn identical_unkeyed_notices_remain_distinct() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let first = center
        .publish(notice(Severity::Info, "saved"), now)
        .unwrap();
    let second = center
        .publish(notice(Severity::Info, "saved"), now)
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(center.counts(now.monotonic).active, 2);
}

#[test]
fn producer_keys_do_not_collide_across_sources_or_generations() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let sources = [
        NotificationSource::Plugin {
            name: "git".into(),
            generation: 1,
        },
        NotificationSource::Plugin {
            name: "git".into(),
            generation: 2,
        },
        NotificationSource::LanguageServer {
            name: "rust-analyzer".into(),
            workspace_root: "/one".into(),
            generation: 1,
        },
        NotificationSource::LanguageServer {
            name: "rust-analyzer".into(),
            workspace_root: "/two".into(),
            generation: 1,
        },
    ];
    for source in sources {
        center
            .begin_progress(
                source,
                "1",
                MessageContent::new("working"),
                ProgressPriority::Background,
                now,
            )
            .unwrap();
    }
    assert_eq!(center.counts(now.monotonic).running, 4);
}

#[test]
fn numeric_and_text_progress_tokens_are_distinct() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let text = start(&mut center, "1", ProgressPriority::Background, now);
    let number = center
        .begin_progress(
            NotificationSource::Editor,
            1_u64,
            MessageContent::new("numeric token"),
            ProgressPriority::Background,
            now,
        )
        .unwrap();
    assert_ne!(text, number);
    assert_eq!(center.counts(now.monotonic).running, 2);
}

#[test]
fn invalid_keys_are_rejected_instead_of_truncated_into_collisions() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    for key in [String::new(), "x".repeat(MAX_KEY_BYTES + 1)] {
        assert_eq!(
            center.publish(
                notice(Severity::Warning, "warning").with_key(key.clone()),
                now
            ),
            Err(NotificationError::InvalidKey)
        );
        assert_eq!(
            center.begin_progress(
                NotificationSource::Editor,
                key,
                MessageContent::new("working"),
                ProgressPriority::Background,
                now
            ),
            Err(NotificationError::InvalidKey)
        );
    }
    assert_eq!(center.records().len(), 0);
}

#[test]
fn a_key_cannot_replace_running_work_and_a_retry_gets_a_new_id() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let original = start(&mut center, "push", ProgressPriority::UserInitiated, now);
    assert_eq!(
        center.publish(notice(Severity::Error, "collision").with_key("push"), now),
        Err(NotificationError::KeyInUse)
    );
    assert_eq!(
        center.begin_progress(
            NotificationSource::Editor,
            "push",
            MessageContent::new("again"),
            ProgressPriority::UserInitiated,
            now
        ),
        Err(NotificationError::KeyInUse)
    );
    center
        .finish_progress(
            original,
            ProgressOutcome::Cancelled,
            MessageContent::new("cancelled"),
            now,
        )
        .unwrap();
    let retry = start(&mut center, "push", ProgressPriority::UserInitiated, now);
    assert_ne!(retry, original);
    assert_eq!(
        center.finish_progress(
            original,
            ProgressOutcome::Succeeded,
            MessageContent::new("late completion"),
            now
        ),
        Err(NotificationError::NotRunning)
    );
    assert!(center.get(retry).unwrap().is_running());
}

#[test]
fn capacity_evicts_oldest_inactive_entry_and_never_active_work() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::with_capacity(2);
    let running = start(&mut center, "index", ProgressPriority::Background, now);
    let expired = center
        .publish(notice(Severity::Info, "saved"), now)
        .unwrap();
    assert_eq!(
        center.publish(notice(Severity::Error, "cannot fit"), now),
        Err(NotificationError::CapacityReached)
    );
    let warning = center
        .publish(notice(Severity::Warning, "warning"), later(now, 4))
        .unwrap();
    assert!(center.get(expired).is_none());
    assert!(center.get(running).is_some());
    assert!(center.get(warning).is_some());
    assert_eq!(center.clear_inactive(later(now, 60).monotonic), 0);
    center.acknowledge(warning).unwrap();
    assert_eq!(center.clear_inactive(later(now, 60).monotonic), 1);
    assert!(center.get(running).is_some());
    assert_eq!(center.mark_read(expired), Err(NotificationError::UnknownId));
}

#[test]
fn zero_capacity_returns_explicit_overflow() {
    let mut center = NotificationCenter::with_capacity(0);
    assert_eq!(
        center.publish(notice(Severity::Info, "message"), NotificationTime::now()),
        Err(NotificationError::CapacityReached)
    );
}

#[test]
fn primary_prefers_severity_then_user_work_and_does_not_rotate_on_updates() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let background = start(&mut center, "index", ProgressPriority::Background, now);
    let user = start(&mut center, "push", ProgressPriority::UserInitiated, now);
    center
        .publish(notice(Severity::Success, "saved"), now)
        .unwrap();
    assert_eq!(center.primary(now.monotonic).unwrap().id, user);
    center
        .update_progress(
            background,
            MessageContent::new("indexing"),
            Some(10),
            later(now, 1),
        )
        .unwrap();
    assert_eq!(center.primary(now.monotonic).unwrap().id, user);
    let error = center
        .publish(notice(Severity::Error, "failed"), now)
        .unwrap();
    assert_eq!(center.primary(now.monotonic).unwrap().id, error);
    center.acknowledge(error).unwrap();
    assert_eq!(center.primary(now.monotonic).unwrap().id, user);
}

#[test]
fn retained_text_is_utf8_safe_bounded_and_keeps_multiline_details() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let message = "界\n".repeat(MAX_DETAILS_BYTES);
    let id = center
        .publish(
            notice(Severity::Error, &message).with_details(&message),
            now,
        )
        .unwrap();
    let content = &center.get(id).unwrap().content;
    assert!(content.truncated);
    assert!(content.summary.len() <= MAX_SUMMARY_BYTES);
    assert!(content.summary.capacity() <= MAX_SUMMARY_BYTES);
    assert!(!content.summary.contains('\n'));
    assert!(content.summary.ends_with('…'));
    assert!(content.details.as_ref().unwrap().len() <= MAX_DETAILS_BYTES);
    assert!(content.details.as_ref().unwrap().capacity() <= MAX_DETAILS_BYTES);
    assert!(content.details.as_ref().unwrap().contains('\n'));
}

#[test]
fn short_details_do_not_retain_an_oversized_producer_allocation() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let mut details = String::with_capacity(MAX_DETAILS_BYTES * 4);
    details.push_str("short details");
    let id = center
        .publish(notice(Severity::Info, "summary").with_details(details), now)
        .unwrap();
    let content = &center.get(id).unwrap().content;
    assert!(!content.truncated);
    assert_eq!(content.details.as_deref(), Some("short details"));
    assert!(content.details.as_ref().unwrap().capacity() <= MAX_DETAILS_BYTES);
}

#[test]
fn progress_percentage_is_clamped() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let id = start(&mut center, "index", ProgressPriority::Background, now);
    center
        .update_progress(id, MessageContent::new("index"), Some(255), now)
        .unwrap();
    assert_eq!(
        center.get(id).unwrap().state,
        NotificationState::Running {
            percentage: Some(100)
        }
    );
}

#[test]
fn notification_text_is_safe_to_paint_without_losing_copyable_details() {
    let now = NotificationTime::now();
    let mut center = NotificationCenter::default();
    let original = "line\n\u{1b}[31m\ttext";
    let id = center
        .publish(
            notice(Severity::Error, original).with_details(original),
            now,
        )
        .unwrap();
    let content = &center.get(id).unwrap().content;
    assert!(!content.summary.chars().any(char::is_control));
    assert_eq!(content.details.as_deref(), Some(original));
    let painted = detail_text(original);
    assert!(!painted.contains('\u{1b}'));
    assert!(!painted.contains('\t'));
    assert!(painted.contains('\n'));
}
