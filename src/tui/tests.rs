use super::*;

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
fn temporary_browse_directory_paths_are_remote_child_paths() {
    assert_eq!(remote_child_path("~", "photos"), "~/photos");
    assert_eq!(
        remote_child_path("/srv/archive", "photos"),
        "/srv/archive/photos"
    );
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
            import_provider: Some(Arc::new(|_, _, _| unreachable!())),
        }),
        ..AppState::default()
    };
    let selected = FileViewRow::from_temporary_entry(
        &state.temporary_browse.as_ref().unwrap().entries[0],
        None,
        0,
    );

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
            import_provider: Some(Arc::new(|_, _, _| unreachable!())),
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
fn active_import_root_selection_accounts_for_temporary_browse_row() {
    let roots = vec![db::RootRow {
        id: "root_imported".to_string(),
        machine_id: "machine_1".to_string(),
        path: "/tmp/imported".to_string(),
        label: None,
        current_size_bytes: 0,
        latest_job_kind: None,
        latest_job_status: None,
        latest_job_phase: None,
    }];
    let mut state = AppState {
        active_import_root_id: Some("root_imported".to_string()),
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

    app::select_active_import_root(&mut state, &roots);

    assert_eq!(state.selected_root, 1);
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
fn command_hints_explain_open_root_prompt() {
    let state = AppState {
        pending_open_root: Some(OpenRootDraft::default()),
        ..AppState::default()
    };

    assert_eq!(
        active_command_hint(&state, false),
        "type local path, file:// path, or host:/path  Enter open  Esc cancel"
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
        "/ filter  u refresh  PgUp/PgDn jump  Enter open dir  Backspace parent  i import  t copy"
    );
}

#[test]
fn command_hints_include_refresh_for_db_file_view() {
    let state = AppState {
        focus: FocusPane::Files,
        ..AppState::default()
    };

    assert!(active_command_hint(&state, false).contains("u refresh db"));
}

#[test]
fn refresh_current_file_listing_explains_db_backed_views() {
    let (job_tx, _job_rx) = mpsc::unbounded_channel();
    let mut state = AppState::default();

    app::refresh_current_file_listing(&mut state, job_tx);

    assert!(state.status.contains("refresh from the database"));
    assert_eq!(state.activities.back().unwrap().level, ActivityLevel::Info);
}

#[test]
fn command_hints_explain_destination_selection() {
    let state = AppState {
        transfer_plan_draft: Some(TransferPlanDraft {
            source_root_id: "root_1".to_string(),
            source_name: "source".to_string(),
            source_path: "/tmp/source".to_string(),
            marked_count: 1,
            marked_bytes: 10,
        }),
        ..AppState::default()
    };

    assert_eq!(
        active_command_hint(&state, false),
        "choose destination root  Enter create transfer plan  Esc cancel source"
    );
}

#[test]
fn transfer_destination_modal_ignores_non_modal_keys() {
    let conn = Connection::open_in_memory().unwrap();
    let roots = vec![db::RootRow {
        id: "root_1".to_string(),
        machine_id: "machine_1".to_string(),
        path: "/tmp/root".to_string(),
        label: None,
        current_size_bytes: 0,
        latest_job_kind: None,
        latest_job_status: None,
        latest_job_phase: None,
    }];
    let mut state = AppState {
        focus: FocusPane::Roots,
        transfer_plan_draft: Some(TransferPlanDraft {
            source_root_id: "source_root".to_string(),
            source_name: "source".to_string(),
            source_path: "/tmp/source".to_string(),
            marked_count: 1,
            marked_bytes: 10,
        }),
        ..AppState::default()
    };

    app::handle_transfer_destination_modal_key(&conn, &roots, KeyCode::Tab, 40, &mut state)
        .unwrap();

    assert_eq!(state.focus, FocusPane::Roots);
    assert!(state.transfer_plan_draft.is_some());
    assert!(state.status.contains("choose destination"));
}

#[test]
fn command_hints_include_root_verify() {
    let state = AppState::default();

    assert!(active_command_hint(&state, false).contains("v verify"));
    assert!(active_command_hint(&state, false).contains("/ filter roots"));
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
fn root_filter_matches_root_metadata() {
    let roots = vec![
        db::RootRow {
            id: "root_1".to_string(),
            machine_id: "machine_local".to_string(),
            path: "/srv/photos".to_string(),
            label: Some("Photo Import".to_string()),
            current_size_bytes: 0,
            latest_job_kind: Some("scan".to_string()),
            latest_job_status: Some("completed".to_string()),
            latest_job_phase: None,
        },
        db::RootRow {
            id: "root_2".to_string(),
            machine_id: "machine_remote".to_string(),
            path: "/mnt/video".to_string(),
            label: None,
            current_size_bytes: 0,
            latest_job_kind: Some("hash".to_string()),
            latest_job_status: Some("failed".to_string()),
            latest_job_phase: None,
        },
    ];

    let label_matches = filtered_root_rows(&roots, "photo");
    assert_eq!(label_matches.len(), 1);
    assert_eq!(label_matches[0].id, "root_1");

    let status_matches = filtered_root_rows(&roots, "FAILED");
    assert_eq!(status_matches.len(), 1);
    assert_eq!(status_matches[0].id, "root_2");

    assert_eq!(filtered_root_rows(&roots, "missing").len(), 0);
}

#[test]
fn root_filter_input_edits_and_clears_filter() {
    let mut state = AppState {
        root_filter_editing: true,
        selected_root: 7,
        file_offset: 5,
        event_offset: 3,
        ..AppState::default()
    };

    assert!(handle_root_filter_input(&mut state, KeyCode::Char('v')));
    assert_eq!(state.root_filter, "v");
    assert_eq!(state.selected_root, 0);
    assert_eq!(state.file_offset, 0);
    assert_eq!(state.event_offset, 0);
    assert!(handle_root_filter_input(&mut state, KeyCode::Char('i')));
    assert_eq!(state.root_filter, "vi");
    assert!(handle_root_filter_input(&mut state, KeyCode::Backspace));
    assert_eq!(state.root_filter, "v");
    assert!(handle_root_filter_input(&mut state, KeyCode::Esc));
    assert_eq!(state.root_filter, "");
    assert!(!state.root_filter_editing);
}

#[test]
fn root_filter_title_and_hint_show_editing_state() {
    let state = AppState {
        root_filter: "video".to_string(),
        root_filter_editing: true,
        ..AppState::default()
    };

    assert_eq!(roots_title(&state), "Roots /video");
    assert_eq!(
        active_command_hint(&state, false),
        "type root filter text  Backspace edit  Enter keep  Esc clear"
    );

    let kept = AppState {
        root_filter: "video".to_string(),
        ..AppState::default()
    };
    assert_eq!(roots_title(&kept), "Roots filter:video");
}

#[test]
fn job_filter_matches_projected_job_metadata() {
    let rows = vec![
        db::JobEventRow {
            job_id: "job_scan_1".to_string(),
            job_kind: "scan".to_string(),
            status: "completed".to_string(),
            phase: Some("finalizing".to_string()),
            current_path: Some("photos/a.png".to_string()),
            files_seen: 1,
            files_done: 1,
            files_skipped: 0,
            errors: 0,
            cancel_requested: false,
            sequence: 1,
            event_kind: "job_completed".to_string(),
            payload_json: serde_json::json!({"message": "scan complete"}).to_string(),
            params_json: None,
        },
        db::JobEventRow {
            job_id: "job_transfer_1".to_string(),
            job_kind: "transfer_copy".to_string(),
            status: "running".to_string(),
            phase: Some("copying".to_string()),
            current_path: Some("video/b.mkv".to_string()),
            files_seen: 3,
            files_done: 1,
            files_skipped: 0,
            errors: 0,
            cancel_requested: false,
            sequence: 2,
            event_kind: "job_progress".to_string(),
            payload_json: serde_json::json!({"type": "job_progress"}).to_string(),
            params_json: Some(
                serde_json::json!({
                    "source_path": "/src/video",
                    "dest_path": "/dst/video"
                })
                .to_string(),
            ),
        },
    ];

    let kind_matches = filtered_job_rows(&rows, "TRANSFER");
    assert_eq!(kind_matches.len(), 1);
    assert_eq!(kind_matches[0].job_id, "job_transfer_1");

    let target_matches = filtered_job_rows(&rows, "photos");
    assert_eq!(target_matches.len(), 1);
    assert_eq!(target_matches[0].job_id, "job_scan_1");

    assert_eq!(filtered_job_rows(&rows, "missing").len(), 0);
}

#[test]
fn job_filter_input_edits_and_clears_filter() {
    let mut state = AppState {
        event_filter_editing: true,
        event_offset: 4,
        ..AppState::default()
    };

    assert!(handle_event_filter_input(&mut state, KeyCode::Char('c')));
    assert_eq!(state.event_filter, "c");
    assert_eq!(state.event_offset, 0);
    assert!(handle_event_filter_input(&mut state, KeyCode::Char('o')));
    assert_eq!(state.event_filter, "co");
    assert!(handle_event_filter_input(&mut state, KeyCode::Backspace));
    assert_eq!(state.event_filter, "c");
    assert!(handle_event_filter_input(&mut state, KeyCode::Esc));
    assert_eq!(state.event_filter, "");
    assert!(!state.event_filter_editing);
}

#[test]
fn job_filter_title_and_hint_show_editing_state() {
    let state = AppState {
        event_filter: "copy".to_string(),
        event_filter_editing: true,
        ..AppState::default()
    };

    assert_eq!(jobs_title(&state), "Jobs /copy");
    assert_eq!(
        active_command_hint(&state, false),
        "type job filter text  Backspace edit  Enter keep  Esc clear"
    );

    let kept = AppState {
        event_filter: "copy".to_string(),
        ..AppState::default()
    };
    assert_eq!(jobs_title(&kept), "Jobs filter:copy");
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
            occurrence_count: None,
            index_state: FileIndexState::Indexed,
        },
        FileViewRow {
            relative_path: "docs/readme.md".to_string(),
            size_bytes: 5,
            modified_at: None,
            content_id: None,
            status: "remote".to_string(),
            kind: FileKind::File,
            occurrence_count: None,
            index_state: FileIndexState::RemoteUnindexed,
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
fn file_row_shows_index_appearance_count() {
    let file = FileViewRow {
        relative_path: "photos/cat.png".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "present".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(3),
        index_state: FileIndexState::Available,
    };

    assert!(file_row("> ", false, &file, FileView::Basic).contains("   3"));
}

#[test]
fn file_rows_show_unicode_evidence_markers() {
    let remote = FileViewRow {
        relative_path: "remote.bin".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "remote".to_string(),
        kind: FileKind::File,
        occurrence_count: None,
        index_state: FileIndexState::RemoteUnindexed,
    };
    let fast = FileViewRow {
        relative_path: "fast.bin".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "indexed".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(1),
        index_state: FileIndexState::Indexed,
    };
    let hashed = FileViewRow {
        relative_path: "hash.bin".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: Some("content_1".to_string()),
        status: "hash".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(1),
        index_state: FileIndexState::Indexed,
    };
    let local = FileViewRow {
        relative_path: "local.bin".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "local".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(1),
        index_state: FileIndexState::Available,
    };
    let changed = FileViewRow {
        relative_path: "changed.bin".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "changed".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(1),
        index_state: FileIndexState::RemoteChanged,
    };
    let missing = FileViewRow {
        relative_path: "missing.bin".to_string(),
        size_bytes: 10,
        modified_at: None,
        content_id: None,
        status: "missing".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(1),
        index_state: FileIndexState::RemoteMissing,
    };

    assert!(file_row("  ", false, &remote, FileView::Basic).contains("◇"));
    assert!(file_row("  ", false, &fast, FileView::Basic).contains("◌"));
    assert!(file_row("  ", false, &hashed, FileView::Basic).contains("◆"));
    assert!(file_row("  ", false, &local, FileView::Basic).contains("◉"));
    assert!(file_row("  ", false, &changed, FileView::Basic).contains("!"));
    assert!(file_row("  ", false, &missing, FileView::Basic).contains("×"));
    assert_eq!(
        file_row_style(&local, false, false).bg,
        theme::available_file().bg
    );
    assert_eq!(
        file_row_style(&changed, false, false).bg,
        theme::changed_file().bg
    );
    assert_eq!(
        file_row_style(&missing, false, false).bg,
        theme::missing_file().bg
    );
    assert!(file_legend().contains("◇ remote"));
    assert!(file_legend().contains("× missing"));
}

#[test]
fn temporary_browse_rows_reconcile_live_remote_with_index() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let ssh_machine = db::ensure_machine_hint(&conn, "nas01", Some("ssh")).unwrap();
    let ssh_root = db::ensure_root(&conn, &ssh_machine, "nas01:/srv/photos").unwrap();
    for (path, size, modified) in [
        ("indexed.bin", 10, "2026-07-10T01:00:00Z"),
        ("changed.bin", 11, "2026-07-10T01:00:00Z"),
        ("missing.bin", 12, "2026-07-10T01:00:00Z"),
    ] {
        db::insert_path_observation(
            &conn,
            db::PathObservationInput {
                machine_id: &ssh_machine,
                root_id: &ssh_root,
                relative_path: path,
                basename: path,
                parent_path: ".",
                size_bytes: size,
                modified_at: Some(modified),
                content_id: None,
            },
        )
        .unwrap();
    }
    let local_machine = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let local_root = db::ensure_root(&conn, &local_machine, "/tmp/local").unwrap();
    db::insert_path_observation(
        &conn,
        db::PathObservationInput {
            machine_id: &local_machine,
            root_id: &local_root,
            relative_path: "local.bin",
            basename: "local.bin",
            parent_path: ".",
            size_bytes: 20,
            modified_at: Some("2026-07-10T02:00:00Z"),
            content_id: None,
        },
    )
    .unwrap();
    let browse = TemporaryBrowse {
        label: "nas01:/srv/photos".to_string(),
        machine_id: ssh_machine.clone(),
        root_path: "/srv/photos".to_string(),
        current_path: "/srv/photos".to_string(),
        entries: vec![
            InitialBrowseEntry {
                kind: "file".to_string(),
                name: "remote.bin".to_string(),
                size_bytes: 9,
                modified_at: Some("2026-07-10T01:00:00Z".to_string()),
            },
            InitialBrowseEntry {
                kind: "file".to_string(),
                name: "indexed.bin".to_string(),
                size_bytes: 10,
                modified_at: Some("2026-07-10T01:00:00Z".to_string()),
            },
            InitialBrowseEntry {
                kind: "file".to_string(),
                name: "changed.bin".to_string(),
                size_bytes: 12,
                modified_at: Some("2026-07-10T01:00:00Z".to_string()),
            },
            InitialBrowseEntry {
                kind: "file".to_string(),
                name: "local.bin".to_string(),
                size_bytes: 20,
                modified_at: Some("2026-07-10T02:00:00Z".to_string()),
            },
        ],
        browse_provider: None,
        import_provider: None,
    };

    let rows = app::temporary_browse_rows(&conn, &db::roots(&conn).unwrap(), &browse).unwrap();
    let state_by_path = rows
        .iter()
        .map(|row| (row.relative_path.as_str(), row.index_state))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(state_by_path["remote.bin"], FileIndexState::RemoteUnindexed);
    assert_eq!(state_by_path["indexed.bin"], FileIndexState::Indexed);
    assert_eq!(state_by_path["changed.bin"], FileIndexState::RemoteChanged);
    assert_eq!(state_by_path["local.bin"], FileIndexState::Available);
    assert_eq!(state_by_path["missing.bin"], FileIndexState::RemoteMissing);
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

    let selection = db::SelectionSummary {
        set_id: "set_1".to_string(),
        marked_count: 2,
        marked_bytes: 20,
    };

    start_transfer_plan_selection(Some(&root), Some(&selection), &mut state);

    let draft = state.transfer_plan_draft.as_ref().unwrap();
    assert_eq!(draft.source_root_id, "root_1");
    assert_eq!(draft.marked_count, 2);
    assert_eq!(draft.marked_bytes, 20);
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
fn drop_queued_transfer_confirmation_marks_plan_canceled() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
    let plan_id = db::create_transfer_plan(
        &conn,
        None,
        &source_id,
        &dest_id,
        None,
        serde_json::json!({}),
    )
    .unwrap();
    db::update_transfer_plan_status(&conn, &plan_id, "queued").unwrap();
    let plan = db::transfer_plan_by_id(&conn, &plan_id).unwrap().unwrap();
    let mut state = AppState::default();

    start_drop_queued_transfer_confirmation(Some(&plan), &mut state);
    assert_eq!(
        state.pending_drop_transfer_plan_id.as_deref(),
        Some(plan_id.as_str())
    );

    handle_drop_queued_transfer_confirmation(&conn, &mut state, KeyCode::Char('y')).unwrap();

    assert!(state.pending_drop_transfer_plan_id.is_none());
    assert_eq!(
        db::transfer_plan_by_id(&conn, &plan_id)
            .unwrap()
            .unwrap()
            .status,
        "canceled"
    );
    assert!(state.status.contains("dropped queued transfer"));
}

#[test]
fn running_resume_transfer_cancel_requests_plan_job_cancel() {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
    let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
    let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
    let job_id = db::create_job(
        &conn,
        "transfer_copy",
        Some(&machine_id),
        Some(&source_id),
        serde_json::json!({}),
    )
    .unwrap();
    db::start_job(&conn, &job_id).unwrap();
    let plan_id = db::create_transfer_plan(
        &conn,
        Some(&job_id),
        &source_id,
        &dest_id,
        None,
        serde_json::json!({}),
    )
    .unwrap();
    db::update_transfer_plan_status(&conn, &plan_id, "running").unwrap();
    let plan = db::transfer_plan_by_id(&conn, &plan_id).unwrap().unwrap();
    let mut state = AppState::default();

    assert!(request_selected_resume_transfer_cancel(&conn, Some(&plan), &mut state).unwrap());

    assert!(db::job_cancel_requested(&conn, &job_id).unwrap());
    assert!(state.status.contains("cancel requested"));
    assert!(db::recent_jobs_and_events(&conn, 10)
        .unwrap()
        .iter()
        .any(|event| event.event_kind == "job_cancel_requested"));
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
        transfer_plan_draft: Some(TransferPlanDraft {
            source_root_id: source_id.clone(),
            source_name: display_name_from_path(&source_dir.path().to_string_lossy()),
            source_path: source_dir.path().to_string_lossy().to_string(),
            marked_count: 1,
            marked_bytes: 5,
        }),
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
    assert!(state.transfer_plan_draft.is_none());
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
        occurrence_count: None,
        index_state: FileIndexState::Indexed,
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
        occurrence_count: None,
        index_state: FileIndexState::Indexed,
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
        occurrence_count: None,
        index_state: FileIndexState::RemoteUnindexed,
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
        occurrence_count: None,
        index_state: FileIndexState::RemoteUnindexed,
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
fn animated_progress_bar_spans_shift_wave_colors() {
    let first = animated_progress_bar_spans(4, 4, 4, 0);
    let second = animated_progress_bar_spans(4, 4, 4, 8);

    assert_eq!(
        first
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "▕▌▌▌▌▏"
    );
    assert_ne!(first[1].style, second[1].style);
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
fn info_bar_renders_active_transfer_progress() {
    let state = AppState::default();
    let progress = TransferProgressSnapshot {
        current_path: "incoming/photos/foo.png".to_string(),
        files_done: 2,
        files_total: 4,
        bytes_done: 512,
        bytes_total: 1024,
        file_bytes_done: 128,
        file_bytes_total: 256,
        bytes_per_second: 2.0 * 1024.0 * 1024.0,
        errors: 1,
        message: None,
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 140, 9));

    InfoBar {
        data: InfoBarData {
            root_name: Some("root".to_string()),
            file: None,
            selection: None,
            event: None,
            root_count: 1,
            transfer_progress: Some(progress),
            import_progress: None,
        },
        state: &state,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Info"));
    assert!(text.contains("Transfer file: incoming/photos/foo.png"));
    assert!(text.contains("Job"));
    assert!(text.contains("File"));
    assert!(text.contains("@ 2.0 MiB/s"));
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
fn job_rows_keep_one_latest_row_per_job() {
    let latest = db::JobEventRow {
        job_id: "job_1".to_string(),
        job_kind: "transfer_copy".to_string(),
        status: "running".to_string(),
        phase: Some("copying".to_string()),
        current_path: Some("b.bin".to_string()),
        files_seen: 2,
        files_done: 1,
        files_skipped: 0,
        errors: 0,
        cancel_requested: false,
        sequence: 2,
        event_kind: "job_progress".to_string(),
        payload_json: "{}".to_string(),
        params_json: None,
    };
    let older = db::JobEventRow {
        sequence: 1,
        current_path: Some("a.bin".to_string()),
        ..latest.clone()
    };
    let other_job = db::JobEventRow {
        job_id: "job_2".to_string(),
        sequence: 1,
        ..latest.clone()
    };

    let rows = job_rows(&[latest, older, other_job]);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].job_id, "job_1");
    assert_eq!(rows[0].current_path.as_deref(), Some("b.bin"));
    assert_eq!(rows[1].job_id, "job_2");
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
fn transfer_error_activity_includes_path_and_reason() {
    let payload = serde_json::json!({
        "type": "transfer_file",
        "relative_path": "incoming/foo.png",
        "error": "destination exists and differs"
    });

    assert_eq!(
        app::transfer_error_activity(&payload.to_string()).unwrap(),
        "transfer error incoming/foo.png: destination exists and differs"
    );
}

#[test]
fn visible_transfer_failed_events_enter_activity_log_once() {
    let mut state = AppState::default();
    let event = db::JobEventRow {
        job_id: "job_transfer_1".to_string(),
        job_kind: "transfer_copy".to_string(),
        status: "running".to_string(),
        phase: Some("copying".to_string()),
        current_path: Some("incoming/foo.png".to_string()),
        files_seen: 1,
        files_done: 0,
        files_skipped: 0,
        errors: 1,
        cancel_requested: false,
        sequence: 3,
        event_kind: "transfer_failed".to_string(),
        payload_json: serde_json::json!({
            "type": "transfer_file",
            "relative_path": "incoming/foo.png",
            "error": "destination exists and differs"
        })
        .to_string(),
        params_json: None,
    };

    app::append_visible_transfer_error_activities(std::slice::from_ref(&event), &mut state);
    app::append_visible_transfer_error_activities(&[event], &mut state);

    assert_eq!(state.activities.len(), 1);
    assert_eq!(state.activities[0].level, ActivityLevel::Error);
    assert_eq!(
        state.activities[0].message,
        "transfer error incoming/foo.png: destination exists and differs"
    );
}

#[test]
fn visible_transfer_error_count_gets_fallback_activity_without_reason() {
    let mut state = AppState::default();
    let event = db::JobEventRow {
        job_id: "job_transfer_1".to_string(),
        job_kind: "transfer_copy".to_string(),
        status: "running".to_string(),
        phase: Some("copying".to_string()),
        current_path: Some("incoming/foo.png".to_string()),
        files_seen: 1,
        files_done: 0,
        files_skipped: 0,
        errors: 1,
        cancel_requested: false,
        sequence: 4,
        event_kind: "job_progress".to_string(),
        payload_json: serde_json::json!({"type": "job_progress", "errors": 1}).to_string(),
        params_json: None,
    };

    app::append_visible_transfer_error_activities(&[event], &mut state);

    assert_eq!(state.activities.len(), 1);
    assert!(state.activities[0]
        .message
        .contains("but no failure reason is visible yet"));
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
fn app_screen_renders_plan_as_modal_when_plan_focus_is_active() {
    let selected_paths = BTreeSet::new();
    let state = AppState {
        focus: FocusPane::Plan,
        last_plan: Some(PlanSnapshot {
            plan_id: "plan_1".to_string(),
            source_root_id: "root_1".to_string(),
            status: "planned".to_string(),
            source_name: "source".to_string(),
            dest_name: "dest".to_string(),
            summary: vec![db::TransferPlanActionSummary {
                action: "copy".to_string(),
                files: 1,
                bytes: 10,
            }],
            entries: vec![db::TransferPlanEntryRow {
                relative_path: "incoming/foo.png".to_string(),
                dest_relative_path: "incoming/foo.png".to_string(),
                size_bytes: 10,
                source_content_id: None,
                dest_content_id: None,
                action: "copy".to_string(),
                reason: "destination path is not indexed".to_string(),
                metadata_json: "{}".to_string(),
            }],
        }),
        ..AppState::default()
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 140, 44));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        detail_content: None,
        file_appearances: &[],
        events: &[],
        root_count: 0,
        transfer_progress: None,
        import_progress: None,
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Plan *"));
    assert!(text.contains("incoming/foo.png"));
    assert!(text.contains("destination path is not indexed"));
}

#[test]
fn plan_modal_ignores_non_modal_keys() {
    let conn = Connection::open_in_memory().unwrap();
    let db_path = PathBuf::from("unused.db");
    let (job_tx, _job_rx) = mpsc::unbounded_channel();
    let mut state = AppState {
        focus: FocusPane::Plan,
        last_plan: Some(PlanSnapshot {
            plan_id: "plan_1".to_string(),
            source_root_id: "root_1".to_string(),
            status: "planned".to_string(),
            source_name: "source".to_string(),
            dest_name: "dest".to_string(),
            summary: Vec::new(),
            entries: Vec::new(),
        }),
        ..AppState::default()
    };

    app::handle_plan_modal_key(&conn, &db_path, job_tx, KeyCode::Tab, 40, &mut state).unwrap();

    assert_eq!(state.focus, FocusPane::Plan);
    assert!(state.status.contains("plan modal"));
}

#[test]
fn detail_pane_renders_selected_file_hashes() {
    let selected_paths = BTreeSet::new();
    let file = FileViewRow {
        relative_path: "photos/foo.png".to_string(),
        size_bytes: 10,
        modified_at: Some("2026-07-08T12:00:00Z".to_string()),
        content_id: Some("content_1234567890".to_string()),
        status: "present".to_string(),
        kind: FileKind::File,
        occurrence_count: Some(2),
        index_state: FileIndexState::Indexed,
    };
    let content = db::ContentObjectRow {
        size_bytes: 10,
        blake3: Some("blake3-hash-value".to_string()),
        sha256: Some("sha256-hash-value".to_string()),
    };
    let appearances = vec![db::FileAppearanceRow {
        root_id: "root_1".to_string(),
        root_path: "/archive".to_string(),
        root_label: Some("Archive".to_string()),
        relative_path: "photos/foo.png".to_string(),
        size_bytes: 10,
        modified_at: Some("2026-07-08T12:00:00Z".to_string()),
        content_id: Some("content_1234567890".to_string()),
    }];
    let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 20));

    DetailPane {
        data: DetailData {
            root: None,
            temporary_browse: None,
            persisted_browse_dir: None,
            summary: None,
            selection: None,
            file: Some(&file),
            content: Some(&content),
            appearances: &appearances,
            selected_paths: &selected_paths,
            plan: None,
            collection: None,
            transfer_progress: None,
            import_progress: None,
        },
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("BLAKE3: blake3-hash-value"));
    assert!(text.contains("SHA-256: sha256-hash-value"));
    assert!(text.contains("Appearances: 1"));
    assert!(text.contains("Archive:photos/foo.png"));
}

#[test]
fn detail_pane_renders_import_progress() {
    let selected_paths = BTreeSet::new();
    let progress = ImportProgress {
        root_id: "root_1".to_string(),
        root_path: "nas01:/srv/archive".to_string(),
        files_imported: 42,
        files_queued: 7,
        directories_processed: 5,
        directories_queued: 2,
        current_path: Some("photos/foo.png".to_string()),
        phase: "fast stat indexing".to_string(),
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 28));

    DetailPane {
        data: DetailData {
            root: None,
            temporary_browse: None,
            persisted_browse_dir: None,
            summary: None,
            selection: None,
            file: None,
            content: None,
            appearances: &[],
            selected_paths: &selected_paths,
            plan: None,
            collection: None,
            transfer_progress: None,
            import_progress: Some(&progress),
        },
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Import: fast stat indexing"));
    assert!(text.contains("Import files: 42 processed | 7 queued"));
    assert!(text.contains("Import dirs: 5 processed | 2 queued"));
    assert!(text.contains("Import current: photos/foo.png"));
}

#[test]
fn app_screen_renders_empty_state_widgets() {
    let state = AppState::default();
    let selected_paths = BTreeSet::new();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 42));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        detail_content: None,
        file_appearances: &[],
        events: &[],
        root_count: 0,
        transfer_progress: None,
        import_progress: None,
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Gremlin"));
    assert!(text.contains("Jobs"));
    assert!(text.contains("Activity Log"));
    assert!(text.contains("No roots yet"));
    assert!(text.contains("No indexed files"));
    assert!(!text.contains("No transfer plan yet"));
}

#[test]
fn app_screen_renders_import_progress_in_empty_file_pane() {
    let state = AppState {
        active_import_progress: Some(ImportProgress {
            root_id: "root_1".to_string(),
            root_path: "nas01:/srv/archive".to_string(),
            files_imported: 2,
            files_queued: 10,
            directories_processed: 1,
            directories_queued: 3,
            current_path: Some("dir/a.bin".to_string()),
            phase: "remote hash indexing".to_string(),
        }),
        ..AppState::default()
    };
    let selected_paths = BTreeSet::new();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 42));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        detail_content: None,
        file_appearances: &[],
        events: &[],
        root_count: 0,
        transfer_progress: None,
        import_progress: state.active_import_progress.as_ref(),
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Import in progress"));
    assert!(text.contains("remote hash indexing"));
    assert!(text.contains("2 processed"));
    assert!(!text.contains("No indexed files"));
}

#[test]
fn app_screen_renders_open_root_modal() {
    let state = AppState {
        pending_open_root: Some(OpenRootDraft {
            input: "nas01:/srv/archive".to_string(),
        }),
        ..AppState::default()
    };
    let selected_paths = BTreeSet::new();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 42));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        detail_content: None,
        file_appearances: &[],
        events: &[],
        root_count: 0,
        transfer_progress: None,
        import_progress: None,
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Open Root"));
    assert!(text.contains("Location: nas01:/srv/archive"));
    assert!(text.contains("Enter open"));
}

#[test]
fn app_screen_renders_import_decision_modal() {
    let state = AppState {
        pending_import: Some(PendingTemporaryImport {
            remote_path: "/srv/archive/photos".to_string(),
        }),
        ..AppState::default()
    };
    let selected_paths = BTreeSet::new();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 42));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        detail_content: None,
        file_appearances: &[],
        events: &[],
        root_count: 0,
        transfer_progress: None,
        import_progress: None,
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Import Remote Root"));
    assert!(text.contains("Path: /srv/archive/photos"));
    assert!(text.contains("f fast recursive stat"));
}

#[test]
fn app_screen_renders_transfer_destination_modal_with_source_context() {
    let state = AppState {
        transfer_plan_draft: Some(TransferPlanDraft {
            source_root_id: "root_1".to_string(),
            source_name: "source".to_string(),
            source_path: "/tmp/source".to_string(),
            marked_count: 3,
            marked_bytes: 4096,
        }),
        ..AppState::default()
    };
    let selected_paths = BTreeSet::new();
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 42));

    AppScreen {
        state: &state,
        roots: &[],
        files: &[],
        selected_paths: &selected_paths,
        selected_root: None,
        selected_temporary: None,
        summary: None,
        selection: None,
        detail_content: None,
        file_appearances: &[],
        events: &[],
        root_count: 0,
        transfer_progress: None,
        import_progress: None,
        detail_file_offset: 0,
    }
    .render(buffer.area, &mut buffer);

    let text = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Choose Destination"));
    assert!(text.contains("Source: source"));
    assert!(text.contains("Marked: 3 (4.00 KiB)"));
    assert!(text.contains("Move to destination root"));
}
