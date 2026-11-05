//! live_face_split trim rules + row-view assembly (Rev 2026-11-04, live rows 活行面).

use std::time::{Duration, Instant};

use phi_kernel_tools::background_shell::BackgroundTaskStatus;

use super::*;
use crate::app::{App, SubAgentState, SubAgentStatus};
use crate::task_layout::RowTint;

fn view(name: &str) -> TaskRowView {
    TaskRowView {
        glyph: "*",
        tint: RowTint::Running,
        name: name.to_string(),
        files: "1 file".to_string(),
        activity: "> tool".to_string(),
        activity_kind: ActivityKind::LiveTool,
        activity_tail_elide: false,
        corpse: false,
        remnant: false,
        selected: false,
    }
}

fn names(views: &[TaskRowView]) -> Vec<&str> {
    views.iter().map(|v| v.name.as_str()).collect()
}

fn sub_agent(name: &str, status: SubAgentStatus) -> SubAgentState {
    SubAgentState {
        name: name.to_string(),
        status,
        files: Vec::new(),
        started_at: Instant::now() - Duration::from_secs(5),
        completed_at: None,
        last_tool_at: Instant::now(),
        events: Vec::new(),
    }
}

#[test]
fn under_the_cap_everything_shows() {
    let views: Vec<TaskRowView> = ["a", "b", "c"].iter().map(|n| view(n)).collect();
    let (shown, more, done) = live_face_split(&views);
    assert_eq!(names(&shown), ["a", "b", "c"]);
    assert_eq!((more, done), (0, 0));
}

#[test]
fn corpses_are_trimmed_before_running_rows() {
    let mut views: Vec<TaskRowView> = ["a", "b", "c", "d", "e", "f", "g", "h"]
        .iter()
        .map(|n| view(n))
        .collect();
    // Two corpses at the head — trimming takes them first even though they
    // sit before the running tail.
    views[0].corpse = true;
    views[0].activity_kind = ActivityKind::Verdict;
    views[1].corpse = true;
    views[1].activity_kind = ActivityKind::Verdict;
    let (shown, more, done) = live_face_split(&views);
    assert_eq!(names(&shown), ["c", "d", "e", "f", "g", "h"]);
    assert_eq!((more, done), (0, 2));
}

#[test]
fn the_running_tail_goes_next() {
    let views: Vec<TaskRowView> = ["a", "b", "c", "d", "e", "f", "g"]
        .iter()
        .map(|n| view(n))
        .collect();
    let (shown, more, done) = live_face_split(&views);
    // One over: the tail row (least recently spawned) goes, counted as more.
    assert_eq!(names(&shown), ["a", "b", "c", "d", "e", "f"]);
    assert_eq!((more, done), (1, 0));
}

#[test]
fn the_selected_row_is_never_trimmed() {
    let mut views: Vec<TaskRowView> = ["a", "b", "c", "d", "e", "f", "g"]
        .iter()
        .map(|n| view(n))
        .collect();
    views[6].selected = true; // the tail row is pinned
    let (shown, more, done) = live_face_split(&views);
    assert_eq!(names(&shown), ["a", "b", "c", "d", "e", "g"]);
    assert_eq!((more, done), (1, 0));
}

#[test]
fn a_failure_remnant_outlives_running_rows() {
    let mut views: Vec<TaskRowView> = ["a", "b", "c", "d", "e", "f", "g"]
        .iter()
        .map(|n| view(n))
        .collect();
    views[6].remnant = true; // the tail row is an unacked failure
    let (shown, more, done) = live_face_split(&views);
    // The remnant survives the running-tail pass; a running row goes instead.
    assert_eq!(names(&shown), ["a", "b", "c", "d", "e", "g"]);
    assert_eq!((more, done), (1, 0));
}

#[test]
fn a_touched_failure_goes_only_at_the_physics_floor() {
    // 7 unacked failures (corpse + remnant both): the cap must still bite,
    // and the fall-through pass counts them by lifecycle, not by trim pass.
    let mut views: Vec<TaskRowView> = (0..7).map(|i| view(&format!("r{i}"))).collect();
    for v in &mut views {
        v.corpse = true;
        v.remnant = true;
        v.activity_kind = ActivityKind::Verdict;
    }
    views[6].selected = true; // the pinned one is always spared
    let (shown, more, done) = live_face_split(&views);
    assert_eq!(shown.len(), 6);
    assert!(shown.iter().any(|v| v.name == "r6"));
    assert_eq!((more, done), (0, 1)); // the trimmed one is a verdict row
}

#[test]
fn row_view_marks_corpses_and_remnants() {
    let mut app = App::new();
    app.sub_agents.insert(
        "root/a".to_string(),
        sub_agent(
            "a",
            SubAgentStatus::Failed {
                summary: "boom".to_string(),
            },
        ),
    );
    app.sub_agents
        .insert("root/b".to_string(), sub_agent("b", SubAgentStatus::Done));
    app.task_panel.touched.insert("root/b".to_string());
    app.task_panel.unacked_failures.insert("root/a".to_string());

    let views = app.task_row_views();
    assert_eq!(views.len(), 2);
    let a = views.iter().find(|v| v.name == "a").unwrap();
    assert!(a.remnant);
    assert!(!a.corpse); // never touched — remnant only
    assert_eq!(a.activity_kind, ActivityKind::Verdict);
    assert!(a.activity_tail_elide); // FAILED keeps its tail
    let b = views.iter().find(|v| v.name == "b").unwrap();
    assert!(b.corpse); // touched + terminal
    assert!(!b.remnant);
}

#[test]
fn a_running_background_row_is_live_not_verdict() {
    let mut app = App::new();
    app.background_tasks.insert(
        "bg1".to_string(),
        crate::app::BackgroundTaskEntry {
            id: "bg1".to_string(),
            command: "sleep 9".to_string(),
            timeout_ms: 120_000,
            status: BackgroundTaskStatus::Running,
            started_at: Instant::now(),
            finished_at: None,
            reported: false,
            output_tail: String::new(),
            consumed: false,
        },
    );
    let views = app.task_row_views();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].activity_kind, ActivityKind::LiveTool);
    assert_eq!(views[0].glyph, "*");
}
