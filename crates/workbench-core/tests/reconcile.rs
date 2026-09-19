use std::collections::BTreeMap;

use workbench_core::{
    Action, MatchSpec, ObservedState, OutputFallback, OutputInfo, PlacementSpec, Recipe,
    ReusePolicy, RuntimeWindow, RuntimeWorkspace, Size, WindowSpec, build_reconcile_plan,
};

fn window(id: u64, app_id: &str, column: usize, tile: usize) -> RuntimeWindow {
    RuntimeWindow {
        id,
        title: Some(app_id.to_owned()),
        app_id: Some(app_id.to_owned()),
        pid: Some(id as i32),
        process_exe: Some(format!("/usr/bin/{app_id}")),
        workspace_id: Some(10),
        is_focused: id == 12,
        is_floating: false,
        column: Some(column),
        tile_index: Some(tile),
        tile_width: if column == 1 { 1056.0 } else { 576.0 },
        tile_height: 1080.0,
    }
}

fn spec(name: &str, app_id: &str, column: usize) -> WindowSpec {
    WindowSpec {
        name: name.to_owned(),
        command: vec![app_id.to_owned()],
        match_spec: MatchSpec {
            app_id: Some(format!("^{app_id}$")),
            ..MatchSpec::default()
        },
        reuse: ReusePolicy::Unique,
        layout: PlacementSpec {
            column,
            ..PlacementSpec::default()
        },
    }
}

fn observed() -> ObservedState {
    ObservedState {
        windows: vec![
            window(12, "code", 1, 1),
            window(15, "google-chrome", 2, 1),
            window(18, "kitty", 3, 1),
        ],
        workspaces: vec![RuntimeWorkspace {
            id: 10,
            index: 1,
            name: Some("Dev".into()),
            output: Some("eDP-1".into()),
            is_active: true,
            is_focused: true,
        }],
        outputs: vec![OutputInfo {
            name: "eDP-1".into(),
            width: Some(1920),
            height: Some(1080),
        }],
    }
}

#[test]
fn desired_equals_actual_has_zero_mutating_actions() {
    let mut editor = spec("editor", "code", 1);
    editor.layout.column_width = Some(Size::Percent(0.55));
    let mut browser = spec("browser", "google-chrome", 2);
    browser.layout.column_width = Some(Size::Percent(0.30));
    let terminal = spec("terminal", "kitty", 3);

    let recipe = Recipe {
        workspace: "Dev".into(),
        output: None,
        output_fallback: OutputFallback::Focused,
        focus: Some("editor".into()),
        spawn_timeout_ms: 10_000,
        windows: vec![editor, browser, terminal],
    };
    let ids = BTreeMap::from([
        ("editor".into(), 12),
        ("browser".into(), 15),
        ("terminal".into(), 18),
    ]);

    let plan = build_reconcile_plan(&recipe, &observed(), &ids, 10, false);
    assert!(plan.actions.is_empty(), "{:?}", plan.actions);
}

#[test]
fn planner_repairs_wrong_column_before_sizes() {
    let editor = spec("editor", "code", 1);
    let mut browser = spec("browser", "google-chrome", 2);
    browser.layout.column_width = Some(Size::Percent(0.30));
    let mut terminal = spec("terminal", "kitty", 2);
    terminal.layout.display = Some(workbench_core::ColumnDisplay::Tabbed);

    let recipe = Recipe {
        workspace: "Dev".into(),
        output: None,
        output_fallback: OutputFallback::Focused,
        focus: Some("editor".into()),
        spawn_timeout_ms: 10_000,
        windows: vec![editor, browser, terminal],
    };
    let ids = BTreeMap::from([
        ("editor".into(), 12),
        ("browser".into(), 15),
        ("terminal".into(), 18),
    ]);

    let plan = build_reconcile_plan(&recipe, &observed(), &ids, 10, false);
    assert!(matches!(
        plan.actions.as_slice(),
        [Action::ConsumeLeft {
            window_id: 18,
            target_column: 2,
            ..
        }]
    ));
}
