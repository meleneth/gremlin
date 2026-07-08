use super::*;
use std::sync::Mutex;

#[test]
fn truncates_long_values() {
    assert_eq!(truncate("abcdef", 4), "abc~");
    assert_eq!(truncate("abc", 4), "abc");
}

#[test]
fn root_display_name_uses_basename_when_label_is_path() {
    let root = db::RootRow {
        id: "root_1".to_string(),
        machine_id: "machine_1".to_string(),
        path: "/tmp/archive/photos".to_string(),
        label: Some("/tmp/archive/photos".to_string()),
        current_size_bytes: 0,
        latest_job_kind: None,
        latest_job_status: None,
        latest_job_phase: None,
    };
    assert_eq!(root_display_name(&root), "photos");
}

#[test]
fn temporary_browse_enter_directory_loads_child_entries() {
    let requested_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let provider_paths = requested_paths.clone();
    let provider: BrowseProvider = Arc::new(move |path| {
        provider_paths.lock().unwrap().push(path.to_string());
        Ok(vec![InitialBrowseEntry {
            kind: "file".to_string(),
            name: "inside.txt".to_string(),
            size_bytes: 5,
            modified_at: None,
        }])
    });
    let mut state = AppState {
        temporary_browse: Some(TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~".to_string(),
            entries: vec![InitialBrowseEntry {
                kind: "dir".to_string(),
                name: "photos".to_string(),
                size_bytes: 0,
                modified_at: None,
            }],
            browse_provider: Some(provider),
            import_provider: None,
        }),
        ..AppState::default()
    };
    let selected =
        FileViewRow::from_temporary_entry(&state.temporary_browse.as_ref().unwrap().entries[0]);

    open_temporary_file_entry(&mut state, Some(&selected));

    let browse = state.temporary_browse.as_ref().unwrap();
    assert_eq!(browse.current_path, "~/photos");
    assert_eq!(browse.entries.len(), 1);
    assert_eq!(browse.entries[0].name, "inside.txt");
    assert_eq!(requested_paths.lock().unwrap().as_slice(), ["~/photos"]);
}

#[test]
fn temporary_import_prompt_targets_selected_file() {
    let mut state = AppState {
        focus: FocusPane::Files,
        temporary_browse: Some(TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~/photos".to_string(),
            entries: vec![InitialBrowseEntry {
                kind: "file".to_string(),
                name: "image.png".to_string(),
                size_bytes: 10,
                modified_at: None,
            }],
            browse_provider: None,
            import_provider: Some(Arc::new(|_, _| unreachable!())),
        }),
        ..AppState::default()
    };
    let selected =
        FileViewRow::from_temporary_entry(&state.temporary_browse.as_ref().unwrap().entries[0]);

    start_temporary_import_prompt(&mut state, Some(&selected));

    assert_eq!(
        state.pending_import.as_ref().unwrap().remote_path,
        "~/photos/image.png"
    );
    assert!(state.status.contains("remote file ~/photos/image.png"));
}

#[test]
fn temporary_import_prompt_defaults_to_current_directory() {
    let mut state = AppState {
        focus: FocusPane::Roots,
        temporary_browse: Some(TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~/photos".to_string(),
            entries: Vec::new(),
            browse_provider: None,
            import_provider: Some(Arc::new(|_, _| unreachable!())),
        }),
        ..AppState::default()
    };

    start_temporary_import_prompt(&mut state, None);

    assert_eq!(
        state.pending_import.as_ref().unwrap().remote_path,
        "~/photos"
    );
    assert!(state.status.contains("remote directory ~/photos"));
}

#[test]
fn command_hints_prioritize_modal_prompts() {
    let state = AppState {
        pending_import: Some(PendingTemporaryImport {
            remote_path: "~/photos".to_string(),
        }),
        ..AppState::default()
    };

    assert_eq!(
        active_command_hint(&state, true),
        "n root only  f fast stat import  h SHA-256 hash import  Esc cancel"
    );
}

#[test]
fn ctrl_c_and_ctrl_d_are_interrupt_keys() {
    assert!(is_interrupt_key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
    assert!(is_interrupt_key(KeyEvent::new(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_interrupt_key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::NONE
    )));
}

#[test]
fn command_hints_explain_temporary_file_browse_actions() {
    let state = AppState {
        focus: FocusPane::Files,
        selected_root: 0,
        temporary_browse: Some(TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~/photos".to_string(),
            entries: Vec::new(),
            browse_provider: None,
            import_provider: None,
        }),
        ..AppState::default()
    };

    assert_eq!(
        active_command_hint(&state, true),
        "/ filter  PgUp/PgDn jump  Enter open dir  Backspace parent  i import  t copy"
    );
}

#[test]
fn command_hints_explain_destination_selection() {
    let state = AppState {
        transfer_source_root_id: Some("root_1".to_string()),
        ..AppState::default()
    };

    assert_eq!(
        active_command_hint(&state, false),
        "choose destination root  Enter create plan  Esc cancel source"
    );
}

#[test]
fn command_hints_include_root_verify() {
    let state = AppState::default();

    assert!(active_command_hint(&state, false).contains("v verify"));
}

#[test]
fn command_hints_explain_scoped_job_choice() {
    let state = AppState {
        pending_scoped_job: Some(PendingScopedJob {
            kind: "verify".to_string(),
            root_id: "root_1".to_string(),
        }),
        ..AppState::default()
    };

    assert_eq!(
        active_command_hint(&state, false),
        "a all files in root  m marked paths only  Esc cancel"
    );
}

#[test]
fn file_filter_matches_paths_and_status() {
    let files = vec![
        FileViewRow {
            relative_path: "photos/cat.png".to_string(),
            size_bytes: 10,
            modified_at: None,
            content_id: None,
            status: "present".to_string(),
            kind: FileKind::File,
        },
        FileViewRow {
            relative_path: "docs/readme.md".to_string(),
            size_bytes: 5,
            modified_at: None,
            content_id: None,
            status: "remote".to_string(),
            kind: FileKind::File,
        },
    ];

    let path_matches = filtered_file_rows(&files, "CAT");
    assert_eq!(path_matches.len(), 1);
    assert_eq!(path_matches[0].relative_path, "photos/cat.png");

    let status_matches = filtered_file_rows(&files, "remote");
    assert_eq!(status_matches.len(), 1);
    assert_eq!(status_matches[0].relative_path, "docs/readme.md");
}

#[test]
fn file_filter_input_edits_and_clears_filter() {
    let mut state = AppState {
        file_filter_editing: true,
        file_offset: 7,
        ..AppState::default()
    };

    assert!(handle_file_filter_input(&mut state, KeyCode::Char('p')));
    assert_eq!(state.file_filter, "p");
    assert_eq!(state.file_offset, 0);
    assert!(handle_file_filter_input(&mut state, KeyCode::Char('n')));
    assert_eq!(state.file_filter, "pn");
    assert!(handle_file_filter_input(&mut state, KeyCode::Backspace));
    assert_eq!(state.file_filter, "p");
    assert!(handle_file_filter_input(&mut state, KeyCode::Esc));
    assert_eq!(state.file_filter, "");
    assert!(!state.file_filter_editing);
}

#[test]
fn page_navigation_jumps_within_active_pane() {
    let mut state = AppState {
        focus: FocusPane::Files,
        file_offset: 2,
        ..AppState::default()
    };

    move_page_down(&mut state, 0, 25, 0, 0, 10);
    assert_eq!(state.file_offset, 12);
    move_page_down(&mut state, 0, 25, 0, 0, 20);
    assert_eq!(state.file_offset, 24);
    move_page_up(&mut state, 10);
    assert_eq!(state.file_offset, 14);
}

#[test]
fn verifies_latest_checksum_collection_for_selected_root() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("gremlin.db");
    let conn = db::open_or_create(&db_path).unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let root_id = db::ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
    let content_id = db::ensure_content_object(
        &conn,
        4,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "ssssssssssssssssssssssssssssssssssssssssssssssssssssssssssssssss",
    )
    .unwrap();
    db::insert_path_observation(
        &conn,
        db::PathObservationInput {
            machine_id: &machine_id,
            root_id: &root_id,
            relative_path: "ok.txt",
            basename: "ok.txt",
            parent_path: ".",
            size_bytes: 4,
            modified_at: None,
            content_id: Some(&content_id),
        },
    )
    .unwrap();
    let collection_id =
        db::create_checksum_collection(&conn, "test collection", "jsonl_import", None).unwrap();
    db::insert_checksum_entry(
        &conn,
        db::ChecksumEntryInput {
            collection_id: &collection_id,
            relative_path: "ok.txt",
            basename: "ok.txt",
            size_bytes: 4,
            modified_at: None,
            blake3: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            sha256: Some("ssssssssssssssssssssssssssssssssssssssssssssssssssssssssssssssss"),
            metadata_json: serde_json::json!({}),
        },
    )
    .unwrap();
    db::insert_checksum_entry(
        &conn,
        db::ChecksumEntryInput {
            collection_id: &collection_id,
            relative_path: "missing.txt",
            basename: "missing.txt",
            size_bytes: 7,
            modified_at: None,
            blake3: None,
            sha256: None,
            metadata_json: serde_json::json!({}),
        },
    )
    .unwrap();
    let root = db::root_by_id(&conn, &root_id).unwrap().unwrap();
    let mut state = AppState::default();

    verify_latest_collection_for_root(&conn, Some(&root), &mut state).unwrap();

    let result = state.collection_result.as_ref().unwrap();
    assert_eq!(state.focus, FocusPane::Plan);
    assert_eq!(result.collection_id, collection_id);
    assert_eq!(result.ok, 1);
    assert_eq!(result.missing, 1);
    assert!(result.rows.iter().any(|row| row.kind == "missing"));
}

#[test]
fn plan_pane_renders_collection_result_rows() {
    let collection = CollectionSnapshot {
        collection_id: "collection_1".to_string(),
        collection_name: "imported".to_string(),
        root_id: "root_1".to_string(),
        root_path: "/tmp/root".to_string(),
        entries: 1,
        ok: 0,
        size_only: 0,
        missing: 1,
        size_mismatch: 0,
        hash_mismatch: 0,
        unverified: 0,
        extras: 0,
        rows: vec![CollectionResultRow {
            kind: "missing".to_string(),
            relative_path: "lost.bin".to_string(),
            expected_size_bytes: 12,
            actual_size_bytes: None,
        }],
    };
    let state = AppState {
        collection_result: Some(collection),
        focus: FocusPane::Plan,
        ..AppState::default()
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 8));

    PlanReviewPane {
        plan: None,
        collection: state.collection_result.as_ref(),
        state: &state,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Collection"));
    assert!(text.contains("missing"));
    assert!(text.contains("lost.bin"));
}

#[test]
fn detail_selection_waits_for_stable_file_offset() {
    let start = Instant::now();
    let mut state = AppState {
        file_offset: 1,
        ..AppState::default()
    };

    state.sync_detail_selection("root:one:.".to_string(), 10, start);
    assert_eq!(state.detail_file_offset, 1);

    state.file_offset = 4;
    state.sync_detail_selection(
        "root:one:.".to_string(),
        10,
        start + Duration::from_millis(100),
    );
    assert_eq!(state.detail_file_offset, 1);

    state.sync_detail_selection(
        "root:one:.".to_string(),
        10,
        start + Duration::from_millis(349),
    );
    assert_eq!(state.detail_file_offset, 1);

    state.sync_detail_selection(
        "root:one:.".to_string(),
        10,
        start + Duration::from_millis(350),
    );
    assert_eq!(state.detail_file_offset, 4);
}

#[test]
fn detail_selection_resets_immediately_for_new_list() {
    let start = Instant::now();
    let mut state = AppState {
        file_offset: 3,
        ..AppState::default()
    };
    state.sync_detail_selection("root:one:.".to_string(), 10, start);
    assert_eq!(state.detail_file_offset, 3);

    state.file_offset = 0;
    state.sync_detail_selection(
        "root:two:.".to_string(),
        2,
        start + Duration::from_millis(10),
    );
    assert_eq!(state.detail_file_offset, 0);
    assert_eq!(state.detail_pending_file_offset, 0);
}

#[test]
fn transfer_plan_selection_moves_focus_to_roots() {
    let root = db::RootRow {
        id: "root_1".to_string(),
        machine_id: "machine_1".to_string(),
        path: "/tmp/source".to_string(),
        label: None,
        current_size_bytes: 0,
        latest_job_kind: None,
        latest_job_status: None,
        latest_job_phase: None,
    };
    let mut state = AppState {
        focus: FocusPane::Files,
        ..AppState::default()
    };

    start_transfer_plan_selection(Some(&root), &mut state);

    assert_eq!(state.transfer_source_root_id.as_deref(), Some("root_1"));
    assert_eq!(state.focus, FocusPane::Roots);
    assert!(state.status.contains("choose destination root"));
}

#[test]
fn root_selection_can_target_resume_transfer_plan_rows() {
    let plan = db::TransferPlanRow {
        id: "plan_resume".to_string(),
        job_id: Some("job_1".to_string()),
        source_root_id: "source".to_string(),
        source_path: "/tmp/source".to_string(),
        dest_root_id: "dest".to_string(),
        dest_path: "/tmp/dest".to_string(),
        selection_set_id: None,
        status: "canceled".to_string(),
        created_at: "2026-07-08T00:00:00Z".to_string(),
        params_json: None,
        entry_count: 3,
        total_bytes: 1024,
    };
    let state = AppState {
        selected_root: 2,
        resumable_transfer_plans: vec![plan.clone()],
        ..AppState::default()
    };

    assert_eq!(visible_root_count(&state, 2), 3);
    assert!(selected_persisted_root(&[], &state).is_none());
    assert_eq!(
        selected_resume_plan(&state, 2).map(|plan| plan.id.as_str()),
        Some("plan_resume")
    );
    assert!(resume_plan_row("> ", &plan).contains("canceled"));
}

#[test]
fn transfer_plan_destination_uses_visible_persisted_root_with_temporary_browse() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    let source_id =
        db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
    db::insert_path_observation(
        &conn,
        db::PathObservationInput {
            machine_id: &machine_id,
            root_id: &source_id,
            relative_path: "a.txt",
            basename: "a.txt",
            parent_path: ".",
            size_bytes: 5,
            modified_at: None,
            content_id: None,
        },
    )
    .unwrap();
    db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
    let roots = db::roots(&conn).unwrap();
    let dest_index = roots
        .iter()
        .position(|root| root.id == dest_id)
        .expect("destination root should be visible");
    let mut state = AppState {
        selected_root: visible_index_for_persisted(
            &AppState {
                temporary_browse: Some(TemporaryBrowse {
                    label: "nas01:".to_string(),
                    machine_id: "machine_remote".to_string(),
                    root_path: "~".to_string(),
                    current_path: "~".to_string(),
                    entries: Vec::new(),
                    browse_provider: None,
                    import_provider: None,
                }),
                ..AppState::default()
            },
            dest_index,
        ),
        transfer_source_root_id: Some(source_id.clone()),
        temporary_browse: Some(TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~".to_string(),
            entries: Vec::new(),
            browse_provider: None,
            import_provider: None,
        }),
        ..AppState::default()
    };

    create_transfer_plan_from_selection(&conn, &roots, &mut state).unwrap();

    let plan = state.last_plan.as_ref().unwrap();
    assert_eq!(
        plan.dest_name,
        display_name_from_path(&dest_dir.path().to_string_lossy())
    );
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].action, "copy");
    assert_eq!(state.transfer_source_root_id, None);
    assert_eq!(dest_id, roots[dest_index].id);
}

#[test]
fn app_state_tracks_activity_and_background_jobs() {
    let mut state = AppState::default();

    state.background_started("started scan job_1");
    assert_eq!(state.active_background_jobs, 1);
    assert_eq!(state.status, "started scan job_1");
    assert_eq!(state.activities.back().unwrap().level, ActivityLevel::Info);

    state.background_finished(ActivityLevel::Success, "completed scan job_1");
    assert_eq!(state.active_background_jobs, 0);
    assert_eq!(
        state.activities.back().unwrap().level,
        ActivityLevel::Success
    );
}

#[test]
fn persisted_root_enter_and_backspace_navigate_directories() {
    let mut state = AppState::default();
    let dir = FileViewRow {
        relative_path: "photos/2026".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "dir:1".to_string(),
        kind: FileKind::Directory,
    };

    open_persisted_file_entry(&mut state, Some("root_1"), Some(&dir));
    assert_eq!(current_persisted_root_dir(&state, "root_1"), "photos/2026");
    assert_eq!(state.file_offset, 0);

    open_persisted_parent(&mut state, Some("root_1"));
    assert_eq!(current_persisted_root_dir(&state, "root_1"), "photos");

    open_persisted_parent(&mut state, Some("root_1"));
    assert_eq!(current_persisted_root_dir(&state, "root_1"), ".");
}

#[test]
fn directory_rows_mark_descendant_files() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let root_id = db::ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
    for path in ["photos/a.png", "photos/nested/b.png"] {
        db::insert_path_observation(
            &conn,
            db::PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: path,
                basename: path.rsplit('/').next().unwrap(),
                parent_path: ".",
                size_bytes: 1,
                modified_at: None,
                content_id: None,
            },
        )
        .unwrap();
    }
    let root = db::root_by_id(&conn, &root_id).unwrap().unwrap();
    let dir = FileViewRow {
        relative_path: "photos".to_string(),
        size_bytes: 2,
        modified_at: None,
        content_id: None,
        status: "dir:2".to_string(),
        kind: FileKind::Directory,
    };
    let mut state = AppState::default();

    toggle_selected_file_mark(&conn, Some(&root), Some(&dir), &mut state).unwrap();

    assert_eq!(
        db::selected_paths_for_root(&conn, &root_id).unwrap(),
        BTreeSet::from([
            "photos/a.png".to_string(),
            "photos/nested/b.png".to_string()
        ])
    );
    assert!(state.status.contains("marked 2 files under photos"));
    assert!(file_row_selected(
        &dir,
        &db::selected_paths_for_root(&conn, &root_id).unwrap()
    ));
}

#[test]
fn temporary_transfer_source_targets_selected_file() {
    let browse = TemporaryBrowse {
        label: "nas01:".to_string(),
        machine_id: "machine_remote".to_string(),
        root_path: "~".to_string(),
        current_path: "~/photos".to_string(),
        entries: Vec::new(),
        browse_provider: None,
        import_provider: None,
    };
    let selected = FileViewRow {
        relative_path: "image.png".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "remote".to_string(),
        kind: FileKind::File,
    };

    let target = temporary_transfer_import_target(FocusPane::Files, &browse, Some(&selected));

    assert_eq!(target.remote_path, "~/photos/image.png");
    assert_eq!(target.selected_relative_path.as_deref(), Some("image.png"));
    assert!(!target.mark_all);
}

#[test]
fn temporary_transfer_source_marks_all_for_directory_target() {
    let browse = TemporaryBrowse {
        label: "nas01:".to_string(),
        machine_id: "machine_remote".to_string(),
        root_path: "~".to_string(),
        current_path: "~/photos".to_string(),
        entries: Vec::new(),
        browse_provider: None,
        import_provider: None,
    };
    let selected = FileViewRow {
        relative_path: "albums".to_string(),
        size_bytes: 0,
        modified_at: None,
        content_id: None,
        status: "dir".to_string(),
        kind: FileKind::Directory,
    };

    let target = temporary_transfer_import_target(FocusPane::Files, &browse, Some(&selected));

    assert_eq!(target.remote_path, "~/photos/albums");
    assert_eq!(target.selected_relative_path, None);
    assert!(target.mark_all);
}

#[test]
fn mark_imported_transfer_source_marks_selected_or_all_paths() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_machine_hint(&conn, "nas01", Some("ssh")).unwrap();
    let root_id = db::ensure_root(&conn, &machine_id, "/srv/photos").unwrap();
    for path in ["a.png", "b.png"] {
        db::insert_path_observation(
            &conn,
            db::PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: path,
                basename: path,
                parent_path: ".",
                size_bytes: 1,
                modified_at: None,
                content_id: None,
            },
        )
        .unwrap();
    }

    mark_imported_transfer_source(&conn, &root_id, Some("a.png"), false).unwrap();
    assert_eq!(
        db::selected_paths_for_root(&conn, &root_id).unwrap(),
        BTreeSet::from(["a.png".to_string()])
    );

    mark_imported_transfer_source(&conn, &root_id, None, true).unwrap();
    assert_eq!(
        db::selected_paths_for_root(&conn, &root_id).unwrap(),
        BTreeSet::from(["a.png".to_string(), "b.png".to_string()])
    );
}

#[test]
fn formats_plan_summary_line() {
    let summary = vec![db::TransferPlanActionSummary {
        action: "copy".to_string(),
        files: 2,
        bytes: 2048,
    }];
    assert_eq!(plan_summary_line(&summary), "copy 2 2.00 KiB");
}

#[test]
fn formats_byte_progress_summary() {
    let payload = serde_json::json!({
        "type": "job_progress",
        "bytes_done": 512,
        "bytes_total": 1024,
        "bytes_per_second": 1048576.0
    });
    assert_eq!(
        byte_progress_summary(&payload.to_string()).unwrap(),
        "▕███████░░░░░░░▏  50% 1.0 MiB/s"
    );
}

#[test]
fn progress_bar_uses_partial_blocks_at_static_width() {
    assert_eq!(progress_bar(1, 4, 4), "▕█░░░▏");
    assert_eq!(progress_bar(1, 8, 4), "▕▌░░░▏");
    assert_eq!(progress_bar(4, 4, 4), "▕████▏");
}

#[test]
fn formats_transfer_progress_detail() {
    let payload = serde_json::json!({
        "type": "job_progress",
        "current_path": "incoming/photos/foo.png",
        "files_done": 2,
        "files_total": 4,
        "bytes_done": 512,
        "bytes_total": 1024,
        "file_bytes_done": 128,
        "file_bytes_total": 256,
        "bytes_per_second": 2.0 * 1024.0 * 1024.0,
        "errors": 1,
        "message": "2/8 reused local checkpoint after MD5 verify offset=67108864 size=67108864"
    });
    let progress = transfer_progress_snapshot(&payload.to_string()).unwrap();
    let lines = transfer_progress_lines(&progress);

    assert!(lines.contains("Job  ▕██████████████░░░░░░░░░░░░░░▏  50%"));
    assert!(lines.contains("@ 2.0 MiB/s"));
    assert!(lines.contains("File ▕██████████████░░░░░░░░░░░░░░▏  50%"));
    assert!(lines.contains("(3/4)"));
    assert!(lines.contains("Path incoming/photos/foo.png | errors 1"));
    assert!(lines.contains("Chunk 2/8 reused local checkpoint after MD5 verify"));
}

#[test]
fn finds_latest_transfer_progress_event() {
    let complete = db::JobEventRow {
        job_id: "job_1".to_string(),
        job_kind: "transfer_copy".to_string(),
        status: "completed".to_string(),
        phase: Some("copying".to_string()),
        current_path: None,
        files_seen: 1,
        files_done: 1,
        files_skipped: 0,
        errors: 0,
        cancel_requested: false,
        sequence: 2,
        event_kind: "job_completed".to_string(),
        payload_json: serde_json::json!({"type": "job", "message": "completed"}).to_string(),
        params_json: None,
    };
    let progress = db::JobEventRow {
        sequence: 1,
        event_kind: "job_progress".to_string(),
        payload_json: serde_json::json!({
            "type": "job_progress",
            "current_path": "a.bin",
            "files_done": 0,
            "files_total": 1,
            "bytes_done": 5,
            "bytes_total": 10,
            "file_bytes_done": 5,
            "file_bytes_total": 10,
            "bytes_per_second": 512.0,
            "errors": 0
        })
        .to_string(),
        ..complete.clone()
    };

    let found = latest_transfer_progress(&[complete, progress]).unwrap();
    assert_eq!(found.current_path, "a.bin");
    assert_eq!(found.bytes_done, 5);
}

#[test]
fn activity_rows_show_transfer_direction() {
    let row = db::JobEventRow {
        job_id: "job_transfer_1".to_string(),
        job_kind: "transfer_copy".to_string(),
        status: "running".to_string(),
        phase: Some("copying".to_string()),
        current_path: Some("a.bin".to_string()),
        files_seen: 0,
        files_done: 0,
        files_skipped: 0,
        errors: 0,
        cancel_requested: false,
        sequence: 1,
        event_kind: "job_progress".to_string(),
        payload_json: serde_json::json!({"type": "job_progress"}).to_string(),
        params_json: Some(
            serde_json::json!({
                "source_path": "/mnt/source",
                "dest_path": "/mnt/dest"
            })
            .to_string(),
        ),
    };

    assert!(event_row("> ", &row).contains("source -> dest"));
}

#[test]
fn formats_plan_review_hint_and_count() {
    let review = db::TransferPlanEntryRow {
        relative_path: "incoming/foo.png".to_string(),
        dest_relative_path: "incoming/foo.png".to_string(),
        size_bytes: 10,
        source_content_id: Some("content_src".to_string()),
        dest_content_id: Some("content_dest".to_string()),
        action: "review".to_string(),
        reason: "collision".to_string(),
        metadata_json: serde_json::json!({
            "hash_collisions": [{"relative_path": "existing/foo.png"}],
            "filename_size_date_collisions": [{"relative_path": "other/foo.png"}]
        })
        .to_string(),
    };
    let copy = db::TransferPlanEntryRow {
        action: "copy".to_string(),
        reason: "destination path is not indexed".to_string(),
        ..review.clone()
    };
    let plan = PlanSnapshot {
        plan_id: "plan_1".to_string(),
        source_root_id: "source_root".to_string(),
        status: "planned".to_string(),
        source_name: "source".to_string(),
        dest_name: "dest".to_string(),
        summary: Vec::new(),
        entries: vec![review.clone(), copy.clone()],
    };

    assert_eq!(plan_entry_hint(&review), "review hash=1 name=1");
    assert!(plan_entry_row("> ", &copy, 120).contains("destination path is not indexed"));
    assert_eq!(plan_review_count(&plan), 1);
    assert_eq!(plan_copy_count(&plan), 1);
}

#[test]
fn app_screen_renders_empty_state_widgets() {
    let state = AppState::default();
    let selected_paths = BTreeSet::new();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 32));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        events: &[],
        root_count: 0,
        transfer_progress: None,
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Gremlin"));
    assert!(text.contains("No roots yet"));
    assert!(text.contains("No indexed files"));
    assert!(text.contains("No transfer plan yet"));
}
