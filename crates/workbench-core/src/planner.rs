use std::collections::{BTreeMap, BTreeSet};

use crate::{ColumnDisplay, ObservedState, Recipe, Size};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    MoveToWorkspace {
        name: String,
        window_id: u64,
        from: Option<u64>,
        to: u64,
    },
    SetFloating {
        name: String,
        window_id: u64,
        floating: bool,
    },
    ExpelRight {
        name: String,
        window_id: u64,
    },
    MoveColumnToIndex {
        name: String,
        window_id: u64,
        from: Option<usize>,
        to: usize,
    },
    ConsumeLeft {
        name: String,
        window_id: u64,
        target_column: usize,
    },
    MoveWindowUp {
        name: String,
        window_id: u64,
        from: usize,
        to: usize,
    },
    MoveWindowDown {
        name: String,
        window_id: u64,
        from: usize,
        to: usize,
    },
    SetColumnWidth {
        name: String,
        window_id: u64,
        column: usize,
        size: Size,
    },
    SetWindowHeight {
        name: String,
        window_id: u64,
        size: Size,
    },
    SetColumnDisplay {
        name: String,
        window_id: u64,
        column: usize,
        display: ColumnDisplay,
    },
    Focus {
        name: String,
        window_id: u64,
    },
}

impl Action {
    pub const fn display_is_unverifiable(&self) -> bool {
        matches!(self, Self::SetColumnDisplay { .. })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcilePlan {
    pub actions: Vec<Action>,
}

impl ReconcilePlan {
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn observable_actions(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(|a| !a.display_is_unverifiable())
    }
}

pub fn build_reconcile_plan(
    recipe: &Recipe,
    observed: &ObservedState,
    resolved: &BTreeMap<String, u64>,
    workspace_id: u64,
    include_display_reassertions: bool,
) -> ReconcilePlan {
    let mut plan = ReconcilePlan::default();

    for spec in &recipe.windows {
        let Some(&id) = resolved.get(&spec.name) else {
            continue;
        };
        let Some(window) = observed.windows.iter().find(|w| w.id == id) else {
            continue;
        };
        if window.workspace_id != Some(workspace_id) {
            plan.actions.push(Action::MoveToWorkspace {
                name: spec.name.clone(),
                window_id: id,
                from: window.workspace_id,
                to: workspace_id,
            });
        }
    }
    if !plan.actions.is_empty() {
        return plan;
    }

    for spec in &recipe.windows {
        let Some(&id) = resolved.get(&spec.name) else {
            continue;
        };
        let Some(window) = observed.windows.iter().find(|w| w.id == id) else {
            continue;
        };
        if window.is_floating != spec.layout.floating {
            plan.actions.push(Action::SetFloating {
                name: spec.name.clone(),
                window_id: id,
                floating: spec.layout.floating,
            });
        }
    }
    if !plan.actions.is_empty() {
        return plan;
    }

    if let Some(action) = find_wrong_group(recipe, observed, resolved, workspace_id) {
        plan.actions.push(action);
        return plan;
    }

    if let Some(action) = find_column_action(recipe, observed, resolved) {
        plan.actions.push(action);
        return plan;
    }

    for (column, specs) in desired_columns(recipe) {
        let Some(anchor) = specs.first() else {
            continue;
        };
        let Some(&id) = resolved.get(&anchor.name) else {
            continue;
        };
        let Some(window) = observed.windows.iter().find(|w| w.id == id) else {
            continue;
        };
        if let Some(size) = specs.iter().find_map(|spec| spec.layout.column_width) {
            if !size_matches_width(size, window.tile_width, observed, workspace_id) {
                plan.actions.push(Action::SetColumnWidth {
                    name: anchor.name.clone(),
                    window_id: id,
                    column,
                    size,
                });
                return plan;
            }
        }
    }

    for spec in &recipe.windows {
        let Some(size) = spec.layout.window_height else {
            continue;
        };
        let Some(&id) = resolved.get(&spec.name) else {
            continue;
        };
        let Some(window) = observed.windows.iter().find(|w| w.id == id) else {
            continue;
        };
        if !size_matches_height(size, window.tile_height, observed, workspace_id) {
            plan.actions.push(Action::SetWindowHeight {
                name: spec.name.clone(),
                window_id: id,
                size,
            });
            return plan;
        }
    }

    if include_display_reassertions {
        for (column, specs) in desired_columns(recipe) {
            let Some(anchor) = specs.first() else {
                continue;
            };
            let Some(display) = specs.iter().find_map(|spec| spec.layout.display) else {
                continue;
            };
            if let Some(&id) = resolved.get(&anchor.name) {
                plan.actions.push(Action::SetColumnDisplay {
                    name: anchor.name.clone(),
                    window_id: id,
                    column,
                    display,
                });
            }
        }
    }

    if let Some(focus) = &recipe.focus {
        if let Some(&id) = resolved.get(focus) {
            if observed.focused_window_id() != Some(id) {
                plan.actions.push(Action::Focus {
                    name: focus.clone(),
                    window_id: id,
                });
            }
        }
    }

    plan
}

fn desired_columns(recipe: &Recipe) -> BTreeMap<usize, Vec<&crate::WindowSpec>> {
    let mut columns: BTreeMap<usize, Vec<&crate::WindowSpec>> = BTreeMap::new();
    for spec in &recipe.windows {
        if !spec.layout.floating {
            columns.entry(spec.layout.column).or_default().push(spec);
        }
    }
    columns
}

fn find_wrong_group(
    recipe: &Recipe,
    observed: &ObservedState,
    resolved: &BTreeMap<String, u64>,
    workspace_id: u64,
) -> Option<Action> {
    let managed: BTreeSet<u64> = resolved.values().copied().collect();
    let desired_sets: Vec<BTreeSet<u64>> = desired_columns(recipe)
        .values()
        .map(|specs| {
            specs
                .iter()
                .filter_map(|spec| resolved.get(&spec.name).copied())
                .collect()
        })
        .collect();

    let mut current: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for window in observed
        .windows
        .iter()
        .filter(|w| w.workspace_id == Some(workspace_id) && !w.is_floating)
    {
        if let Some(column) = window.column {
            current.entry(column).or_default().push(window.id);
        }
    }

    for ids in current.values() {
        if ids.len() < 2 {
            continue;
        }
        let managed_here: BTreeSet<u64> = ids
            .iter()
            .copied()
            .filter(|id| managed.contains(id))
            .collect();
        if managed_here.is_empty() {
            continue;
        }
        let has_unmanaged = ids.iter().any(|id| !managed.contains(id));
        let already_desired = !has_unmanaged && desired_sets.iter().any(|set| set == &managed_here);
        if already_desired {
            continue;
        }

        let id = *managed_here.iter().next_back()?;
        let name = resolved
            .iter()
            .find_map(|(name, resolved_id)| (*resolved_id == id).then(|| name.clone()))
            .unwrap_or_else(|| id.to_string());
        return Some(Action::ExpelRight {
            name,
            window_id: id,
        });
    }
    None
}

fn find_column_action(
    recipe: &Recipe,
    observed: &ObservedState,
    resolved: &BTreeMap<String, u64>,
) -> Option<Action> {
    for (desired_column, specs) in desired_columns(recipe) {
        let anchor = specs.first()?;
        let &anchor_id = resolved.get(&anchor.name)?;
        let anchor_window = observed.windows.iter().find(|w| w.id == anchor_id)?;
        let anchor_column = anchor_window.column;

        let all_same = specs.iter().all(|spec| {
            resolved
                .get(&spec.name)
                .and_then(|id| observed.windows.iter().find(|w| w.id == *id))
                .and_then(|w| w.column)
                == anchor_column
        });

        if all_same {
            if anchor_column != Some(desired_column) {
                return Some(Action::MoveColumnToIndex {
                    name: anchor.name.clone(),
                    window_id: anchor_id,
                    from: anchor_column,
                    to: desired_column,
                });
            }

            for (desired_tile, spec) in specs.iter().enumerate() {
                let &id = resolved.get(&spec.name)?;
                let window = observed.windows.iter().find(|w| w.id == id)?;
                let actual = window.tile_index?;
                let desired = desired_tile + 1;
                if actual > desired {
                    return Some(Action::MoveWindowUp {
                        name: spec.name.clone(),
                        window_id: id,
                        from: actual,
                        to: desired,
                    });
                }
                if actual < desired {
                    return Some(Action::MoveWindowDown {
                        name: spec.name.clone(),
                        window_id: id,
                        from: actual,
                        to: desired,
                    });
                }
            }
            continue;
        }

        if anchor_column != Some(desired_column) {
            return Some(Action::MoveColumnToIndex {
                name: anchor.name.clone(),
                window_id: anchor_id,
                from: anchor_column,
                to: desired_column,
            });
        }

        for spec in specs.iter().skip(1) {
            let &id = resolved.get(&spec.name)?;
            let window = observed.windows.iter().find(|w| w.id == id)?;
            if window.column == anchor_column {
                continue;
            }
            let staging_column = desired_column + 1;
            if window.column != Some(staging_column) {
                return Some(Action::MoveColumnToIndex {
                    name: spec.name.clone(),
                    window_id: id,
                    from: window.column,
                    to: staging_column,
                });
            }
            return Some(Action::ConsumeLeft {
                name: spec.name.clone(),
                window_id: id,
                target_column: desired_column,
            });
        }
    }
    None
}

fn size_matches_width(
    size: Size,
    actual: f64,
    observed: &ObservedState,
    workspace_id: u64,
) -> bool {
    size_matches(size, actual, observed, workspace_id, true)
}

fn size_matches_height(
    size: Size,
    actual: f64,
    observed: &ObservedState,
    workspace_id: u64,
) -> bool {
    size_matches(size, actual, observed, workspace_id, false)
}

fn size_matches(
    size: Size,
    actual: f64,
    observed: &ObservedState,
    workspace_id: u64,
    width: bool,
) -> bool {
    let expected = match size {
        Size::Pixels(px) => f64::from(px),
        Size::Percent(ratio) => {
            let Some(output_name) = observed
                .workspace(workspace_id)
                .and_then(|w| w.output.as_deref())
            else {
                return false;
            };
            let Some(output) = observed.output(output_name) else {
                return false;
            };
            let Some(full) = (if width { output.width } else { output.height }) else {
                return false;
            };
            f64::from(full) * ratio
        }
    };
    let tolerance = match size {
        Size::Pixels(_) => 2.0,
        Size::Percent(_) => expected.mul_add(0.05, 2.0).max(8.0),
    };
    (actual - expected).abs() <= tolerance
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        ColumnDisplay, MatchSpec, ObservedState, OutputFallback, OutputInfo, PlacementSpec, Recipe,
        ReusePolicy, RuntimeWindow, RuntimeWorkspace, Size, WindowSpec,
    };

    use super::{Action, build_reconcile_plan};

    fn spec(name: &str, column: usize) -> WindowSpec {
        WindowSpec {
            name: name.into(),
            command: vec!["foot".into()],
            match_spec: MatchSpec {
                app_id: Some(format!("^{name}$")),
                ..MatchSpec::default()
            },
            reuse: ReusePolicy::Unique,
            layout: PlacementSpec {
                column,
                ..PlacementSpec::default()
            },
        }
    }

    fn state(windows: Vec<RuntimeWindow>) -> ObservedState {
        ObservedState {
            windows,
            workspaces: vec![RuntimeWorkspace {
                id: 7,
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

    fn window(id: u64, column: usize, tile: usize) -> RuntimeWindow {
        RuntimeWindow {
            id,
            title: None,
            app_id: Some(format!("w{id}")),
            pid: None,
            process_exe: None,
            workspace_id: Some(7),
            is_focused: id == 1,
            is_floating: false,
            column: Some(column),
            tile_index: Some(tile),
            tile_width: 960.0,
            tile_height: 1080.0,
        }
    }

    fn recipe(windows: Vec<WindowSpec>) -> Recipe {
        Recipe {
            name: None,
            workspace: "Dev".into(),
            output: None,
            output_fallback: OutputFallback::Focused,
            focus: Some("a".into()),
            spawn_timeout_ms: 10_000,
            windows,
        }
    }

    #[test]
    fn converged_layout_is_noop() {
        let mut a = spec("a", 1);
        a.layout.column_width = Some(Size::Percent(0.5));
        let r = recipe(vec![a, spec("b", 2)]);
        let observed = state(vec![window(1, 1, 1), window(2, 2, 1)]);
        let ids = BTreeMap::from([("a".into(), 1), ("b".into(), 2)]);
        let plan = build_reconcile_plan(&r, &observed, &ids, 7, false);
        assert!(plan.is_noop());
    }

    #[test]
    fn percentage_size_tolerates_unobservable_work_area_reduction() {
        let mut a = spec("a", 1);
        a.layout.column_width = Some(Size::Percent(0.5));
        let mut r = recipe(vec![a]);
        r.focus = None;
        let mut actual = window(1, 1, 1);
        actual.tile_width = 930.0;
        let observed = state(vec![actual]);
        let ids = BTreeMap::from([("a".into(), 1)]);

        let plan = build_reconcile_plan(&r, &observed, &ids, 7, false);

        assert!(plan.is_noop());
    }

    #[test]
    fn separated_windows_are_grouped() {
        let a = spec("a", 1);
        let mut b = spec("b", 1);
        b.layout.display = Some(ColumnDisplay::Tabbed);
        let mut r = recipe(vec![a, b]);
        r.focus = None;
        let observed = state(vec![window(1, 1, 1), window(2, 2, 1)]);
        let ids = BTreeMap::from([("a".into(), 1), ("b".into(), 2)]);
        let plan = build_reconcile_plan(&r, &observed, &ids, 7, false);
        assert!(matches!(
            plan.actions.as_slice(),
            [Action::ConsumeLeft { window_id: 2, .. }]
        ));
    }

    #[test]
    fn tabbed_display_is_reasserted_because_niri_does_not_expose_it() {
        let mut a = spec("a", 1);
        a.layout.display = Some(ColumnDisplay::Tabbed);
        let mut r = recipe(vec![a]);
        r.focus = None;
        let observed = state(vec![window(1, 1, 1)]);
        let ids = BTreeMap::from([("a".into(), 1)]);
        let plan = build_reconcile_plan(&r, &observed, &ids, 7, true);
        assert!(matches!(
            plan.actions.as_slice(),
            [Action::SetColumnDisplay { .. }]
        ));
    }

    #[test]
    fn tile_order_is_repaired_one_step_at_a_time() {
        let a = spec("a", 1);
        let b = spec("b", 1);
        let mut r = recipe(vec![a, b]);
        r.focus = None;
        let observed = state(vec![window(1, 1, 2), window(2, 1, 1)]);
        let ids = BTreeMap::from([("a".into(), 1), ("b".into(), 2)]);

        let plan = build_reconcile_plan(&r, &observed, &ids, 7, false);

        assert!(matches!(
            plan.actions.as_slice(),
            [Action::MoveWindowUp {
                window_id: 1,
                from: 2,
                to: 1,
                ..
            }]
        ));
    }
}
