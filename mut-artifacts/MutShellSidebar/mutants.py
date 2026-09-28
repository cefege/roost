"""Mutant catalogue for MutShellSidebar (paths relative to the repo root)."""

TEST_BINS = ["shell_motion", "resize_drag", "dead_route_safety_net", "route_session",
             "keyboard_shortcuts", "route_surfaces", "worker_paths", "sidebar_logic"]

W = "crates/roost-web/src/"
SB = "lib::components::layout::status_bar::tests::"


def mk(id, slice, file, old, new, change, *expect):
    return {"id": id, "slice": slice, "file": file, "old": old, "new": new,
            "change": change, "expect": list(expect)}


M = {}


def add(*args):
    m = mk(*args)
    M[m["id"]] = m


# ---------------- SHELL: shell_motion ----------------
F = W + "motion/drag_threshold.rs"
add("M1", "SHELL", F, "(x - start_x).hypot(y - start_y) >= DRAG_THRESHOLD_PX",
    "(x - start_x).abs() >= DRAG_THRESHOLD_PX",
    "drag gate measures horizontal travel only", "shell_motion::a_straight_down_split_drag_arms")
add("M2", "SHELL", F, "(x - start_x).hypot(y - start_y) >= DRAG_THRESHOLD_PX",
    "(x - start_x).max(y - start_y) >= DRAG_THRESHOLD_PX",
    "drag gate uses signed max delta (ignores up/left travel)",
    "shell_motion::every_direction_past_the_threshold_arms")
add("M3", "SHELL", F, "(x - start_x).hypot(y - start_y) >= DRAG_THRESHOLD_PX",
    "(x - start_x).hypot(y - start_y) >= DRAG_THRESHOLD_PX / 2.0",
    "drag gate threshold halved", "shell_motion::travel_under_the_threshold_never_arms")
F = W + "motion/edge_swipe_drawer.rs"
add("M4", "SHELL", F, "if dx.abs() > dy.abs() * AXIS_RATIO {", "if dx.abs() > dy.abs() {",
    "horizontal lock drops the 1.5 axis ratio",
    "shell_motion::the_axis_locks_only_past_the_arm_gate_and_horizontal_needs_the_ratio")
add("M5", "SHELL", F, "-width + dx.min(width).max(0.0)", "-width + dx.min(width)",
    "open offset loses its lower clamp",
    "shell_motion::the_open_offset_follows_the_finger_within_the_drawer_width")
add("M6", "SHELL", F, "|| velocity >= FLICK_VELOCITY", "|| velocity.abs() >= FLICK_VELOCITY",
    "open commits on a flick in either direction",
    "shell_motion::an_open_commits_past_thirty_percent_or_on_a_rightward_flick_only")
add("M7", "SHELL", F, "dx.max(-width).min(0.0)", "dx.max(-width)",
    "close offset loses its upper clamp (follows rightward)",
    "shell_motion::the_close_offset_follows_the_finger_leftward_only")
add("M8", "SHELL", F, "dx <= -width * COMMIT_FRACTION", "dx < -width * COMMIT_FRACTION",
    "close commit boundary off by one (< instead of <=)",
    "shell_motion::a_close_commits_past_thirty_percent_left_or_on_a_leftward_flick_only")
F = W + "motion/drop_zones.rs"
add("M9", "SHELL", F,
    "        (DropZone::Left, x - rect.x, band_x),\n        (DropZone::Right, rect.x + rect.w - x, band_x),",
    "        (DropZone::Right, x - rect.x, band_x),\n        (DropZone::Left, rect.x + rect.w - x, band_x),",
    "left/right band labels swapped",
    "shell_motion::the_centre_merges_and_each_outer_band_splits_on_its_edge")
add("M10", "SHELL", F, "let band_x = EDGE_MIN.max(rect.w * EDGE_RATIO);",
    "let band_x = EDGE_MIN.min(rect.w * EDGE_RATIO);",
    "horizontal band uses min(80px, 25%) instead of max",
    "shell_motion::a_small_pane_still_has_an_eighty_pixel_band")
add("M11", "SHELL", F,
    "            Self::Left => 0,\n            Self::Right => 1,\n            Self::Top => 2,",
    "            Self::Left => 2,\n            Self::Right => 1,\n            Self::Top => 0,",
    "tie-break ranks vertical edges first",
    "shell_motion::a_corner_tie_favours_the_horizontal_edge_and_the_origin_is_respected")
add("M12", "SHELL", F, "DropZone::Top => (SplitDir::Col, true),",
    "DropZone::Top => (SplitDir::Col, false),", "Top split inserts the new pane second",
    "shell_motion::edges_map_to_splits_and_merge_or_reorder_do_not")
add("M13", "SHELL", F, "            x: rect.x + half_w,\n", "            x: rect.x,\n",
    "Right highlight drawn over the left half",
    "shell_motion::a_zone_highlights_the_half_the_new_pane_takes")
add("M14", "SHELL", F, "return (!home).then(|| target(DropZone::Center));",
    "return Some(target(DropZone::Center));", "origin pane's strip band also targets a merge",
    "shell_motion::the_home_pane_body_reorders_its_edges_split_and_its_strip_is_left_to_the_strip")
add("M15", "SHELL", F, "return (!home).then(|| target(DropZone::Center));",
    "return None;", "every pane's strip band targets nothing",
    "shell_motion::another_pane_merges_on_its_centre_or_strip_and_splits_on_its_edges")
add("M16", "SHELL", F,
    "            && y >= pane.rect.y\n            && y < pane.rect.y + pane.rect.h\n",
    "            && y >= pane.rect.y\n", "pane hit-test ignores the bottom edge",
    "shell_motion::a_pointer_off_every_pane_targets_nothing")
F = W + "motion/spring.rs"
add("M17", "SHELL", F, "2.0 * (stiffness * mass).sqrt()", "2.0 * (stiffness / mass).sqrt()",
    "critical damping divides by mass", "shell_motion::critical_damping_is_two_root_k_m")
add("M18", "SHELL", F, "let dt = dt_ms.max(0.0) / 1000.0;", "let dt = dt_ms / 1000.0;",
    "negative step no longer clamped to zero",
    "shell_motion::a_zero_or_negative_step_changes_nothing")
add("M19", "SHELL", F, "(-config.stiffness * displacement - config.damping * state.velocity)",
    "(config.stiffness * displacement - config.damping * state.velocity)",
    "spring force sign flipped (pushes away)",
    "shell_motion::a_step_pulls_toward_the_target_and_a_resting_spring_stays",
    "shell_motion::the_snap_spring_settles_within_two_seconds_of_frames")
add("M20", "SHELL", F, "        && state.velocity.abs() < SPRING_REST_VELOCITY",
    "        || state.velocity.abs() < SPRING_REST_VELOCITY", "rest needs close OR slow",
    "shell_motion::rest_needs_both_close_and_slow")
add("M21", "SHELL", F, "(-config.stiffness * displacement - config.damping * state.velocity)",
    "(-config.stiffness * displacement + config.damping * state.velocity)",
    "damping sign flipped (energy grows)",
    "shell_motion::the_snap_spring_settles_within_two_seconds_of_frames")

# ---------------- SHELL: resize_drag ----------------
F = W + "motion/resize_drag.rs"
add("R1", "SHELL", F, "self.live.remove(&token.owner);", "self.live.clear();",
    "release clears every owner, not its own",
    "resize_drag::overlapping_owners_suppress_until_the_final_release",
    "resize_drag::two_gestures_keep_independent_owners")
add("R2", "SHELL", F, "        self.generation += 1;\n        self.live.clear();\n",
    "        self.live.clear();\n", "reset does not bump the generation",
    "resize_drag::a_reset_retires_stale_owners_without_letting_them_release_a_new_one")
add("R3", "SHELL", F, "            self.live.clear();\n            self.generation += 1;\n",
    "            self.live.clear();\n", "cap overflow does not retire the generation",
    "resize_drag::the_owner_past_the_cap_retires_the_generation_and_prior_releases_are_stale")
add("R4", "SHELL", F, "!std::mem::replace(&mut self.frame_queued, true)",
    "{\n            self.frame_queued = true;\n            true\n        }",
    "every sample requests a frame (no coalescing)",
    "resize_drag::moves_coalesce_into_one_frame_and_other_pointers_are_ignored")
add("R5", "SHELL", F, "if !std::mem::replace(&mut self.active, false) {", "if !self.active {",
    "finish never deactivates (not idempotent)",
    "resize_drag::pointer_up_cancels_the_queued_frame_and_commits_the_latest_sample_once",
    "resize_drag::disposal_mid_drag_aborts_the_queued_geometry_and_stays_idempotent")
add("R6", "SHELL", F, "        self.frame_queued = false;\n        self.active.then_some(self.latest)",
    "        self.active.then_some(self.latest)", "a flushed frame leaves the queue flag set",
    "resize_drag::a_flushed_frame_applies_the_latest_sample_and_clears_the_queue")
add("R7", "SHELL", F, "commit: commit.then_some(self.latest),", "commit: Some(self.latest),",
    "disposal commits the latest sample",
    "resize_drag::disposal_mid_drag_aborts_the_queued_geometry_and_stays_idempotent")

# ---------------- SHELL: dead_route_safety_net ----------------
F = W + "dead_route_safety_net.rs"
add("D1", "SHELL", F, "        self.armed = None;\n        if let Some(open) = liveness.open_session {",
    "        if let Some(open) = liveness.open_session {",
    "evaluate does not void the pending timer",
    "dead_route_safety_net::a_blip_that_recovers_inside_the_grace_window_never_bounces")
add("D2", "SHELL", F, "        if recovered {\n            return None;\n        }\n", "",
    "timer ignores the recovery re-check",
    "dead_route_safety_net::a_recovery_seen_only_by_the_timers_recheck_cancels_the_bounce")
add("D3", "SHELL", F, "            last_open: self.last_open.clone(),", "            last_open: None,",
    "bounce forgets the last open session",
    "dead_route_safety_net::a_durable_miss_after_a_live_session_bounces_as_gone")
add("D4", "SHELL", F, "if !liveness.on_terminal_route || !liveness.hydrated {",
    "if !liveness.on_terminal_route {", "arms before hydration",
    "dead_route_safety_net::nothing_arms_before_hydration")
add("D5", "SHELL", F,
    "        Some(Bounce {\n            last_open: self.last_open.clone(),\n        })",
    "        self.last_open.clone().map(|session| Bounce {\n            last_open: Some(session),\n        })",
    "only a once-live session bounces (stale deep link stranded)",
    "dead_route_safety_net::a_deep_link_that_never_resolved_bounces_as_stale",
    "dead_route_safety_net::a_re_evaluation_voids_the_earlier_timer_even_while_still_missing")
add("D6", "SHELL", F, "if !liveness.on_terminal_route || !liveness.hydrated {",
    "if !liveness.hydrated {", "arms off a terminal route",
    "dead_route_safety_net::off_a_terminal_route_nothing_arms")
add("D7", "SHELL", F, "        self.next_ticket += 1;\n        self.armed = Some(self.next_ticket);",
    "        self.armed = Some(self.next_ticket);", "tickets are not fresh per arm",
    "dead_route_safety_net::a_re_evaluation_voids_the_earlier_timer_even_while_still_missing")

# ---------------- SHELL: route_session / route_surfaces ----------------
F = W + "route_session.rs"
add("S1", "SHELL", F, "Route::Session { session_id } => session_by_id(store, session_id),",
    "Route::Session { session_id } => session_by_workspace(store, session_id),",
    "/s/:id resolved with the workspace selector",
    "route_session::a_session_route_resolves_by_id",
    "route_session::a_closed_session_resolves_but_is_not_the_open_one_the_deck_renders")
add("S2", "SHELL", F,
    "let folder = decode_folder_path(worker_os(store, worker_fp), folder_path)?;",
    "let folder = folder_path.clone();", "folder splat used undecoded",
    "route_session::a_folder_route_resolves_by_machine_and_spawn_folder")
add("S4", "SHELL", F, "let channel: u32 = channel.parse().ok()?;",
    "let channel: u32 = channel.parse().unwrap_or(1);",
    "unreadable legacy channel falls back to channel 1",
    "route_session::a_legacy_channel_route_resolves_the_channel_it_names")
add("S5", "SHELL", F, "        _ => None,\n    }\n}",
    "        _ => store.sessions.sessions().values().next(),\n    }\n}",
    "non-terminal route falls back to the first session",
    "route_session::non_terminal_routes_and_an_unknown_workspace_resolve_to_nothing")
add("S6", "SHELL", F,
    "    active_session_for_route(store, paths, route)\n        .filter(|session| session.status == SessionStatus::Open)",
    "    active_session_for_route(store, paths, route)",
    "open-only filter dropped",
    "route_session::a_closed_session_resolves_but_is_not_the_open_one_the_deck_renders")
add("S11", "SHELL", F,
    "newest_open_session_in_folder(store, paths, &folder_key, Some(session.id.as_str()))",
    "newest_open_session_in_folder(store, paths, &folder_key, None)",
    "sibling search does not exclude the gone session",
    "route_session::a_gone_session_lands_on_its_newest_open_sibling_else_home")
add("S3", "SHELL", "crates/roost-client-core/src/store/selectors.rs",
    "session.created_at > current.created_at", "session.created_at < current.created_at",
    "newest_open picks the oldest",
    "route_session::a_workspace_route_resolves_to_its_newest_open_session")
add("S8", "SHELL", "crates/roost-platform/src/native_path/route.rs",
    "decoded_segments.push(decode_uri_component(segment)?);",
    "decoded_segments.push(segment.to_owned());", "POSIX route segments not percent-decoded",
    "route_session::folder_paths_round_trip_through_the_route_codec")
F = W + "terminal_href.rs"
add("S9", "SHELL", F, "let Some(platform) = worker_path_platform(worker_os, abs) else {",
    "let Some(platform) = worker_path_platform(worker_os.or(Some(\"linux\")), abs) else {",
    "unhydrated worker OS defaults to linux when encoding",
    "route_session::windows_drive_and_unc_folders_use_tagged_reversible_routes",
    "route_session::terminal_href_builds_the_folder_url_or_falls_back_to_the_session_url")
add("S10", "SHELL", F,
    "        .as_deref()\n        .filter(|folder| !folder.is_empty())\n    else {",
    "        .as_deref()\n    else {", "empty spawn folder not treated as absent",
    "route_session::terminal_href_builds_the_folder_url_or_falls_back_to_the_session_url")
F = W + "app.rs"
add("S7", "SHELL", F,
    "        | Route::File { .. }\n        | Route::Search => Surface::Served(ServedSurface::MainPane),",
    "        | Route::File { .. } => Surface::Served(ServedSurface::MainPane),\n        Route::Search => Surface::NotServed {\n            path: route.to_path(),\n        },",
    "surface_for sends /search to NotServed",
    "route_session::every_terminal_file_and_search_route_is_one_main_pane_surface",
    "route_surfaces::the_root_the_gallery_and_every_main_pane_route_are_served_and_the_rest_are_not")
add("U1", "SHELL", F,
    "Route::Settings { .. } | Route::Pair | Route::Help | Route::Browse { .. } => {",
    "Route::Settings { .. } => Surface::Served(ServedSurface::Home),\n        Route::Pair | Route::Help | Route::Browse { .. } => {",
    "surface_for serves /settings",
    "route_surfaces::the_root_the_gallery_and_every_main_pane_route_are_served_and_the_rest_are_not",
    "route_surfaces::the_not_served_panel_names_the_path_the_spec_navigated_to")

# ---------------- SHELL: keyboard_shortcuts ----------------
F = W + "keyboard_shortcuts.rs"
K = "keyboard_shortcuts::"
add("K1", "SHELL", F, "        || context.target_editable\n        || context.terminal_owns_keyboard\n",
    "        || context.target_editable\n", "cursor keys not blocked while a deck is mounted",
    K + "enter_never_activates_the_cursor_while_a_deck_is_mounted")
add("K2", "SHELL", F, "\"Enter\" if context.cursor_row_selected => A::ActivateCursor,",
    "\"Enter\" if !context.cursor_row_selected => A::ActivateCursor,",
    "Enter activation guard inverted",
    K + "enter_activates_the_cursor_in_a_pure_sidebar_view",
    K + "enter_stays_a_focused_buttons_activation_until_a_row_is_highlighted")
add("K3", "SHELL", F, "        || context.controller_map_open\n        || context.target_editable\n",
    "        || context.controller_map_open\n", "editable targets not blocked",
    K + "enter_never_activates_while_typing_in_an_input")
add("K4", "SHELL", F,
    "if platform == BrowserPlatform::Windows && matches(PlatformShortcut::CommandPalette) {",
    "if matches(PlatformShortcut::CommandPalette) {",
    "palette chord checked before the terminal-owns guard on every platform",
    K + "a_body_focused_terminal_keeps_ctrl_k")
add("K5", "SHELL", F, "\"ArrowDown\" if context.has_cursor_targets => A::MoveCursor(1),",
    "\"ArrowDown\" => A::MoveCursor(1),", "ArrowDown moves the cursor with no rows",
    K + "arrows_stay_native_scroll_when_no_cursor_rows_exist")
add("K6", "SHELL", F, "\"Enter\" if context.cursor_row_selected => A::ActivateCursor,",
    "\"Enter\" => A::ActivateCursor,", "Enter activates with no highlighted row",
    K + "enter_stays_a_focused_buttons_activation_until_a_row_is_highlighted")
add("K7", "SHELL", F, "    if context.default_prevented {\n        return A::Ignore;\n    }\n", "",
    "defaultPrevented keys rerouted", K + "a_prevented_key_is_never_rerouted")
add("K8", "SHELL", F,
    "    if context.terminal_owns_keyboard {\n        if matches(PlatformShortcut::TermFontIncrease)",
    "    {\n        if matches(PlatformShortcut::TermFontIncrease)",
    "zoom chords route without a terminal",
    K + "zoom_chords_route_only_while_a_terminal_owns_the_keyboard")
add("K9", "SHELL", F, "if context.palette_open && key.key == \"Escape\" {",
    "if context.palette_open && key.key == \"Escape\" && !context.terminal_owns_keyboard {",
    "Escape does not close the palette over a terminal",
    K + "settings_and_the_palette_route_off_the_terminal")
F = W + "platform/browser_platform.rs"
add("K10", "SHELL", F,
    "        .ua_data_platform\n        .as_deref()\n        .or(hints.platform.as_deref())",
    "        .platform\n        .as_deref()\n        .or(hints.ua_data_platform.as_deref())",
    "navigator.platform preferred over UA client hints",
    K + "detection_prefers_client_hints_then_falls_back_to_the_user_agent")
add("K11", "SHELL", F,
    "    if platform == BrowserPlatform::Windows {\n        shortcut.windows_label()",
    "    if platform != BrowserPlatform::MacOs {\n        shortcut.windows_label()",
    "Linux shown the Windows label",
    K + "mac_and_linux_labels_are_kept_and_windows_gets_its_own")
add("K12", "SHELL", F, "S::ToggleSidebar => primary && key == \"b\" && !event.shift,",
    "S::ToggleSidebar => primary && key == \"b\" && event.shift,",
    "mac/linux sidebar toggle requires shift", K + "mac_and_linux_command_chords_match")
add("K13", "SHELL", F,
    "|letter: &str| key == letter && event.ctrl && event.shift && !event.alt && !event.meta;",
    "|letter: &str| key == letter && event.ctrl && !event.alt && !event.meta;",
    "Windows ctrl_shift chords accept plain Ctrl",
    K + "windows_never_binds_a_plain_ctrl_letter")
add("K14", "SHELL", F,
    "        || event.alt_graph\n        || (platform == BrowserPlatform::Windows && event.ctrl && event.alt && !event.meta)",
    "        || event.alt_graph", "Windows Ctrl+Alt no longer read as AltGraph",
    K + "alt_graph_never_triggers_an_application_shortcut")
add("K15", "SHELL", F, "S::TerminalTab => is_digit_1_9(key) && alt_only,",
    "S::TerminalTab => is_digit_1_9(key) && ctrl_no_alt_meta,",
    "Windows tab selection bound to Ctrl",
    K + "windows_pane_focus_and_tab_selection_use_alt_not_control")

# ---------------- SHELL: status_bar inline ----------------
F = W + "components/layout/status_bar.rs"
add("B1", "SHELL", F, "if self.open_sessions == 1 {", "if self.open_sessions <= 1 {",
    "zero sessions worded singular", SB + "one_session_is_singular_and_none_is_plural")
add("B2", "SHELL", F, "self.workers_online, self.workers_total)",
    "self.workers_total, self.workers_online)", "counts read registered over online",
    SB + "the_counts_read_online_over_registered")
add("B3", "SHELL", F, "    pub fn workers_worded(&self) -> String {\n",
    "    pub fn workers_worded(&self) -> String {\n        if self.workers_total == 0 {\n            return String::new();\n        }\n",
    "empty fleet hides the counts",
    SB + "an_empty_fleet_reads_as_zero_of_zero_rather_than_as_nothing")
add("B4", "SHELL", F, "AgentDotStatus::Warn => \"warn\",", "AgentDotStatus::Warn => \"warning\",",
    "Warn spelled as a name the dot has no rule for",
    SB + "every_agent_dot_status_is_a_name_the_dot_stylesheet_knows")
add("B5", "SHELL", F, "        coordinator: None,\n        machine,\n",
    "        coordinator: Some(CoordinatorState::Syncing),\n        machine,\n",
    "coordinator item permanently Syncing",
    SB + "the_coordinator_item_is_absent_rather_than_permanently_syncing")
add("B6", "SHELL", F, "        path,\n    );\n    let context",
    "        path,\n    )\n    .or_else(|| store.sessions.sessions().values().next());\n    let context",
    "a path naming no session falls back to the first session",
    SB + "a_path_that_names_no_session_shows_no_machine_and_no_context")
add("B7", "SHELL", F, "        coordinator: None,\n        machine,\n",
    "        coordinator: None,\n        machine: machine.or_else(|| match crate::routes::Route::parse(path) {\n            crate::routes::Route::Terminal { worker_fp, .. } => Some(Reading {\n                label: worker_fp,\n                status: \"offline\".to_string(),\n            }),\n            _ => None,\n        }),\n",
    "machine guessed from the /t/ route's fingerprint",
    SB + "a_folder_terminal_route_shows_no_machine_rather_than_a_guessed_one")

# ---------------- SIDEBAR: worker_paths ----------------
WP = "worker_paths::"
F = W + "platform/worker_paths/palette.rs"
add("W1", "SIDEBAR", F, "    join_worker_path(worker_os, dir, &[name])",
    "    let _ = worker_os;\n    Some(format!(\"{dir}/{name}\"))", "child path is a naive string join",
    WP + "child_path_joins_handling_root_and_trailing_slash")
add("W5", "SIDEBAR", F,
    "    if hidden > 0 {\n        out.push(CrumbView::Ellipsis(middle[..hidden].to_vec()));\n    }",
    "    out.push(CrumbView::Ellipsis(middle[..hidden].to_vec()));",
    "an empty ellipsis is pushed when nothing is hidden",
    WP + "collapse_with_nothing_hidden_returns_every_crumb")
add("W6", "SIDEBAR", F, "out.push(CrumbView::Ellipsis(middle[..hidden].to_vec()));",
    "out.push(CrumbView::Ellipsis(middle[middle.len() - hidden..].to_vec()));",
    "ellipsis folds from the right of the middle",
    WP + "collapse_folds_from_the_left_of_the_middle_and_keeps_parent_and_current")
add("W7", "SIDEBAR", F, "let middle = &crumbs[1..crumbs.len() - 2];",
    "let middle = &crumbs[1..crumbs.len() - 1];", "the parent crumb is foldable",
    WP + "three_crumbs_never_collapse_and_hide_middle_is_clamped")
F = "crates/roost-platform/src/native_path/parts.rs"
add("W2", "SIDEBAR", F,
    "    let mut current = String::new();\n    for segment in segments(&normalized) {\n        current = format!(\"{current}/{segment}\");",
    "    let mut current = String::new();\n    for segment in segments(&normalized) {\n        current = format!(\"/{segment}\");",
    "POSIX crumbs not cumulative", WP + "path_crumbs_are_cumulative_for_absolute_and_home_roots")
add("W3", "SIDEBAR", F, "Some(index) if index > 0 => Ok(normalized[..index].to_owned()),",
    "Some(index) if index > 0 => Ok(normalized[..=index].to_owned()),",
    "dirname keeps the trailing separator",
    WP + "parent_path_drops_the_last_segment_and_roots_stay_put")
add("W4", "SIDEBAR", F, "            let root = format!(\"{drive_letter}:/\");",
    "            let root = format!(\"{drive_letter}:\");", "drive root crumb path loses its slash",
    WP + "windows_drive_and_unc_paths_keep_their_native_roots")
F = W + "platform/worker_paths.rs"
add("W8", "SIDEBAR", F, "Some(os) => HostPlatform::parse(os).ok(),",
    "Some(os) => HostPlatform::parse(os).ok().or(Some(infer_native_path_platform(path))),",
    "an unsupported worker OS is guessed instead of refused",
    WP + "a_declared_platform_wins_and_an_unknown_one_is_refused")
add("W9", "SIDEBAR", F,
    "        .strip_prefix(\"\\\\\\\\\")\n        .or_else(|| path.strip_prefix(\"//\"))\n",
    "        .strip_prefix(\"\\\\\\\\\")\n",
    "forward-slash UNC not inferred as Windows",
    WP + "an_unhydrated_worker_infers_windows_only_from_windows_spellings")
add("W10", "SIDEBAR", F, "Some(user) if user != last => format!(\"{user}/{last}\"),",
    "Some(user) => format!(\"{user}/{last}\"),", "home dir itself reads user/user",
    WP + "short_worker_path_reads_user_and_basename_under_a_home_root")
F = "crates/roost-client-core/src/store/folder_activity.rs"
add("W11", "SIDEBAR", F,
    "        if terminals > 0 {\n            activity.insert((*folder).to_owned(), FolderActivity { terminals });\n        }",
    "        activity.insert((*folder).to_owned(), FolderActivity { terminals });",
    "folders with zero terminals listed", WP + "folder_activity_is_empty_without_a_session_inside")
add("W12", "SIDEBAR", F,
    "session.kind == SessionKind::Shell && session.worker_fp.as_str() == worker_fp",
    "session.kind == SessionKind::Shell", "other machines' sessions counted",
    WP + "folder_activity_counts_a_folder_and_its_whole_subtree")
add("W13", "SIDEBAR", F,
    "        let prefix = if base.ends_with('/') {\n            base.clone()\n        } else {\n            format!(\"{base}/\")\n        };",
    "        let prefix = format!(\"{base}/\");", "root folder prefix doubled to //",
    WP + "folder_activity_handles_trailing_slash_root_and_home")
add("W14", "SIDEBAR", F, ".filter(|cwd| **cwd == base || cwd.starts_with(&prefix))",
    ".filter(|cwd| cwd.starts_with(&base))", "descendant match ignores segment boundary",
    WP + "folder_activity_folds_windows_case_at_segment_boundaries")

# ---------------- SIDEBAR: sidebar_logic ----------------
SL = "sidebar_logic::"
F = W + "session_naming.rs"
add("L1", "SIDEBAR", F, "if used + width > max {", "if used >= max {",
    "cap checked before adding the cluster",
    SL + "an_astral_emoji_straddling_the_cap_is_dropped_whole",
    SL + "a_zwj_family_at_the_cap_is_not_split_mid_cluster")
add("L2", "SIDEBAR", F, "for cluster in text.graphemes(true) {",
    "for cluster in text.split_inclusive(|_: char| true) {",
    "truncation walks chars, not grapheme clusters",
    SL + "a_zwj_family_at_the_cap_is_not_split_mid_cluster")
add("L3", "SIDEBAR", F, "(!title.is_empty()).then(|| truncate_title(title, TITLE_MAX_UTF16))",
    "(!title.is_empty()).then(|| title.to_owned())", "OSC subtitle uncapped",
    SL + "a_short_title_is_unchanged_and_the_osc_title_is_capped")
F = W + "components/machines/machine_identity.rs"
add("L4", "SIDEBAR", F, "if words.next().is_some() || !is_chip_generation(generation) {",
    "if words.next().is_some() {", "any word after Apple becomes a chip badge",
    SL + "a_verified_macbook_shows_its_apple_chip")
add("L5", "SIDEBAR", F,
    ".is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))",
    ".is_some()", "distribution matched by bare prefix",
    SL + "a_distribution_mark_needs_a_recognized_worker_identity_not_a_label")
add("L6", "SIDEBAR", F,
    ".any(|word| [\"laptop\", \"notebook\", \"book\"].contains(&word.to_ascii_lowercase().as_str()))",
    ".any(|word| [\"laptop\", \"notebook\", \"book\"].contains(&word))",
    "laptop words matched case-sensitively",
    SL + "a_worker_reported_windows_laptop_differs_from_a_desktop")
add("L7", "SIDEBAR", W + "components/sidebar/session_row.rs",
    "if path == session_path || path.starts_with(&format!(\"{session_path}/\")) {",
    "if path.starts_with(&session_path) {", "row match ignores the segment boundary",
    SL + "a_row_is_selected_only_by_its_own_route")
add("L8", "SIDEBAR", W + "components/sidebar/row_swipe.rs",
    "if self.axis == SwipeAxis::Undecided {", "if self.axis != SwipeAxis::Horizontal {",
    "a vertical gesture re-decides its axis",
    SL + "a_swipe_closes_only_past_the_threshold_and_a_vertical_drag_never_claims")
add("L9", "SIDEBAR", W + "components/sidebar/sidebar_new_terminal.rs",
    "bottom: viewport_height - trigger_top + (below.y - trigger_bottom),",
    "bottom: viewport_height - trigger_bottom + (below.y - trigger_bottom),",
    "menu anchored from the trigger's bottom, overlapping it",
    SL + "the_machine_menu_opens_upward_from_its_trigger")

PLAN = {
    "b0": [],
    "b1": ["K1", "M1", "M4", "M9", "M12", "M17", "R1", "D1", "S1", "B1", "W1", "L1"],
    "b2": ["K2", "M2", "M5", "M10", "M13", "M18", "R2", "D2", "S2", "B2", "W2", "L3"],
    "b3": ["K3", "M3", "M6", "M11", "M14", "M20", "R3", "D3", "S3", "B4", "W3", "L4"],
    "b4": ["K4", "K10", "M7", "M15", "M19", "R4", "D4", "S4", "B5", "W5", "L5"],
    "b5": ["K5", "K11", "M8", "M16", "M21", "R5", "D5", "S5", "B3", "W6", "L6"],
    "b6": ["K6", "K12", "K13", "R6", "D6", "S6", "B6", "W7", "W8", "W13", "L7"],
    "b7": ["K7", "K14", "R7", "D7", "S7", "B7", "W9", "W10", "W14", "L8"],
    "b8": ["K8", "K15", "S8", "S10", "U1", "W4", "W11", "L9"],
    "b9": ["K9", "S9", "S11", "W12", "L2"],
}
BATCHES = {name: [M[i] for i in ids] for name, ids in PLAN.items()}
_all = [i for ids in PLAN.values() for i in ids]
assert sorted(_all) == sorted(M), (set(M) - set(_all), set(_all) - set(M))
