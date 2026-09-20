use std::collections::BTreeMap;
use std::time::Duration;

use niri_ipc::{
    Action as NiriAction, ColumnDisplay as NiriColumnDisplay, SizeChange, WorkspaceReferenceArg,
};
use thiserror::Error;
use tracing::debug;
use workbench_core::{
    Action, ColumnDisplay, ObservedState, Recipe, ReconcilePlan, Size, build_reconcile_plan,
};

use crate::{NiriError, NiriSession};

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error(transparent)]
    Niri(#[from] NiriError),
    #[error("window {name:?} (id {id}) disappeared while applying the recipe")]
    WindowDisappeared { name: String, id: u64 },
    #[error("reconciliation did not converge after {steps} mutations; last action: {last_action}")]
    DidNotConverge { steps: usize, last_action: String },
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub executed: Vec<Action>,
    pub warnings: Vec<String>,
    pub remaining: Vec<Action>,
    pub display_reassertions: usize,
}

pub async fn reconcile(
    session: &NiriSession,
    recipe: &Recipe,
    resolved: &BTreeMap<String, u64>,
    workspace_id: u64,
) -> Result<ApplyReport, ApplyError> {
    let mut report = ApplyReport::default();
    let mut previous = None::<String>;
    let mut repeats = 0usize;

    // Re-plan after every mutation. Niri processes IPC requests separately, and moving/grouping
    // windows changes column indices. A stale multi-action plan is much more fragile than this
    // bounded observe -> mutate -> observe loop.
    for step in 0..64 {
        let snapshot = session.snapshot().await;
        let plan = build_reconcile_plan(recipe, &snapshot, resolved, workspace_id, false);
        let Some(action) = plan.actions.first().cloned() else {
            break;
        };

        // Focus is intentionally last. Column display is not observable in Niri 26.04 and must
        // be reasserted before we restore the user's requested final focus.
        if matches!(action, Action::Focus { .. }) {
            break;
        }

        let fingerprint = format!("{action:?}");
        if previous.as_deref() == Some(&fingerprint) {
            repeats += 1;
        } else {
            previous = Some(fingerprint.clone());
            repeats = 0;
        }

        // Percentage sizes are expressed against Niri's working area, while IPC exposes
        // output logical size rather than the layer-shell-exclusive work area. If our verification
        // approximation disagrees after one successful mutation, do not loop forever.
        if repeats >= 1 && is_unverifiable_percentage_size(&action) {
            report.warnings.push(format!(
                "could not verify percentage size after applying {fingerprint}; Niri IPC does not expose \
                 the effective working-area dimensions, so the requested size was applied once"
            ));
            break;
        }
        if repeats >= 2 {
            return Err(ApplyError::DidNotConverge {
                steps: step + 1,
                last_action: fingerprint,
            });
        }

        execute_action(session, &action).await?;
        report.executed.push(action);
    }

    let snapshot = session.snapshot().await;
    let verification = build_reconcile_plan(recipe, &snapshot, resolved, workspace_id, false);
    report.remaining = verification
        .actions
        .iter()
        .filter(|action| !matches!(action, Action::Focus { .. }))
        .cloned()
        .collect();

    // Niri does not expose current column display mode in WindowLayout. Reassert every
    // explicitly declared column mode exactly once per apply, independently of observable drift.
    // This must not depend on the planner reaching its final stage: a slow size/layout event can
    // otherwise mask the display action.
    for action in declared_display_actions(recipe, resolved) {
        execute_action(session, &action).await?;
        report.executed.push(action);
        report.display_reassertions += 1;
    }

    if let Some(focus_name) = &recipe.focus {
        if let Some(&window_id) = resolved.get(focus_name) {
            let focus = Action::Focus {
                name: focus_name.clone(),
                window_id,
            };
            let snapshot = session.snapshot().await;
            if snapshot.focused_window_id() != Some(window_id) {
                execute_action(session, &focus).await?;
                report.executed.push(focus);
            }
        } else {
            report.warnings.push(format!(
                "final focus target {focus_name:?} was not resolved"
            ));
        }
    }

    Ok(report)
}

pub async fn execute_action(session: &NiriSession, action: &Action) -> Result<(), ApplyError> {
    debug!(?action, "executing reconcile action");
    match action {
        Action::MoveToWorkspace {
            name,
            window_id,
            to,
            ..
        } => {
            ensure_window(session, name, *window_id).await?;
            session
                .send_action(NiriAction::MoveWindowToWorkspace {
                    window_id: Some(*window_id),
                    reference: WorkspaceReferenceArg::Id(*to),
                    focus: false,
                })
                .await?;
            wait_for_window(session, *window_id, |window| {
                window.workspace_id == Some(*to)
            })
            .await?;
        }
        Action::SetFloating {
            name,
            window_id,
            floating,
        } => {
            ensure_window(session, name, *window_id).await?;
            let niri_action = if *floating {
                NiriAction::MoveWindowToFloating {
                    id: Some(*window_id),
                }
            } else {
                NiriAction::MoveWindowToTiling {
                    id: Some(*window_id),
                }
            };
            session.send_action(niri_action).await?;
            wait_for_window(session, *window_id, |window| {
                window.is_floating == *floating
            })
            .await?;
        }
        Action::ExpelRight { name, window_id } => {
            ensure_window(session, name, *window_id).await?;
            let old = window_position(&session.snapshot().await, *window_id);
            session
                .send_action(NiriAction::ConsumeOrExpelWindowRight {
                    id: Some(*window_id),
                })
                .await?;
            wait_for_position_change(session, *window_id, old).await?;
        }
        Action::ConsumeLeft {
            name,
            window_id,
            target_column,
        } => {
            ensure_window(session, name, *window_id).await?;
            session
                .send_action(NiriAction::ConsumeOrExpelWindowLeft {
                    id: Some(*window_id),
                })
                .await?;
            wait_for_window(session, *window_id, |window| {
                window.column == Some(*target_column)
            })
            .await?;
        }
        Action::MoveColumnToIndex {
            name,
            window_id,
            to,
            ..
        } => {
            ensure_window(session, name, *window_id).await?;
            focus_window(session, *window_id).await?;
            session
                .send_action(NiriAction::MoveColumnToIndex { index: *to })
                .await?;
            wait_for_window(session, *window_id, |window| window.column == Some(*to)).await?;
        }
        Action::MoveWindowUp {
            name,
            window_id,
            from,
            ..
        } => {
            ensure_window(session, name, *window_id).await?;
            focus_window(session, *window_id).await?;
            session.send_action(NiriAction::MoveWindowUp {}).await?;
            wait_for_window(session, *window_id, |window| {
                window.tile_index.is_some_and(|actual| actual < *from)
            })
            .await?;
        }
        Action::MoveWindowDown {
            name,
            window_id,
            from,
            ..
        } => {
            ensure_window(session, name, *window_id).await?;
            focus_window(session, *window_id).await?;
            session.send_action(NiriAction::MoveWindowDown {}).await?;
            wait_for_window(session, *window_id, |window| {
                window.tile_index.is_some_and(|actual| actual > *from)
            })
            .await?;
        }
        Action::SetColumnWidth {
            name,
            window_id,
            size,
            ..
        } => {
            ensure_window(session, name, *window_id).await?;
            focus_window(session, *window_id).await?;
            let old = session
                .snapshot()
                .await
                .windows
                .iter()
                .find(|window| window.id == *window_id)
                .map(|window| window.tile_width);
            session
                .send_action(NiriAction::SetColumnWidth {
                    change: size_change(*size),
                })
                .await?;
            wait_for_size_change(session, *window_id, *size, old, true).await?;
        }
        Action::SetWindowHeight {
            name,
            window_id,
            size,
        } => {
            ensure_window(session, name, *window_id).await?;
            let old = session
                .snapshot()
                .await
                .windows
                .iter()
                .find(|window| window.id == *window_id)
                .map(|window| window.tile_height);
            session
                .send_action(NiriAction::SetWindowHeight {
                    id: Some(*window_id),
                    change: size_change(*size),
                })
                .await?;
            wait_for_size_change(session, *window_id, *size, old, false).await?;
        }
        Action::SetColumnDisplay {
            name,
            window_id,
            display,
            ..
        } => {
            ensure_window(session, name, *window_id).await?;
            focus_window(session, *window_id).await?;
            let before = session.layout_version();
            session
                .send_action(NiriAction::SetColumnDisplay {
                    display: match display {
                        ColumnDisplay::Normal => NiriColumnDisplay::Normal,
                        ColumnDisplay::Tabbed => NiriColumnDisplay::Tabbed,
                    },
                })
                .await?;
            // The display mode itself is not observable in 26.04, but changing it normally
            // produces layout events. Give EventStream a bounded chance to catch up so apply
            // does not return while the visible tab transition is still pending.
            let _ = session
                .wait_for_layout_change_after(before, Duration::from_millis(150))
                .await;
        }
        Action::Focus { name, window_id } => {
            ensure_window(session, name, *window_id).await?;
            focus_window(session, *window_id).await?;
        }
    }
    Ok(())
}

fn size_change(size: Size) -> SizeChange {
    match size {
        Size::Pixels(px) => SizeChange::SetFixed(px),
        Size::Percent(ratio) => SizeChange::SetProportion(ratio * 100.0),
    }
}

fn is_unverifiable_percentage_size(action: &Action) -> bool {
    matches!(
        action,
        Action::SetColumnWidth {
            size: Size::Percent(_),
            ..
        } | Action::SetWindowHeight {
            size: Size::Percent(_),
            ..
        }
    )
}

fn declared_display_actions(recipe: &Recipe, resolved: &BTreeMap<String, u64>) -> Vec<Action> {
    let mut displays = BTreeMap::new();
    for spec in &recipe.windows {
        if !spec.layout.floating {
            if let Some(display) = spec.layout.display {
                displays.entry(spec.layout.column).or_insert(display);
            }
        }
    }

    displays
        .into_iter()
        .filter_map(|(column, display)| {
            let anchor = recipe
                .windows
                .iter()
                .find(|spec| !spec.layout.floating && spec.layout.column == column)?;
            let &window_id = resolved.get(&anchor.name)?;
            Some(Action::SetColumnDisplay {
                name: anchor.name.clone(),
                window_id,
                column,
                display,
            })
        })
        .collect()
}

async fn wait_for_size_change(
    session: &NiriSession,
    id: u64,
    size: Size,
    old: Option<f64>,
    width: bool,
) -> Result<(), NiriError> {
    let result = session
        .wait_until(Duration::from_secs(2), |state| {
            let Some(window) = state.windows.iter().find(|window| window.id == id) else {
                return false;
            };
            let actual = if width {
                window.tile_width
            } else {
                window.tile_height
            };
            match size {
                Size::Pixels(px) => (actual - f64::from(px)).abs() <= 2.0,
                Size::Percent(_) => old.is_none_or(|old| (actual - old).abs() > 1.0),
            }
        })
        .await;

    if matches!(size, Size::Percent(_)) && result.is_err() {
        // Niri may accept a proportional size that rounds to the current tile size.
        // The caller's bounded re-plan handles verification without turning this into a hard error.
        Ok(())
    } else {
        result
    }
}

async fn ensure_window(session: &NiriSession, name: &str, id: u64) -> Result<(), ApplyError> {
    if session
        .snapshot()
        .await
        .windows
        .iter()
        .any(|window| window.id == id)
    {
        Ok(())
    } else {
        Err(ApplyError::WindowDisappeared {
            name: name.to_owned(),
            id,
        })
    }
}

async fn focus_window(session: &NiriSession, id: u64) -> Result<(), NiriError> {
    if session.snapshot().await.focused_window_id() == Some(id) {
        return Ok(());
    }
    session.send_action(NiriAction::FocusWindow { id }).await?;
    wait_for_window(session, id, |window| window.is_focused).await
}

async fn wait_for_window<F>(session: &NiriSession, id: u64, predicate: F) -> Result<(), NiriError>
where
    F: Fn(&workbench_core::RuntimeWindow) -> bool,
{
    session
        .wait_until(Duration::from_secs(2), |state| {
            state
                .windows
                .iter()
                .find(|window| window.id == id)
                .is_some_and(&predicate)
        })
        .await
}

async fn wait_for_position_change(
    session: &NiriSession,
    id: u64,
    old: Option<(Option<usize>, Option<usize>)>,
) -> Result<(), NiriError> {
    session
        .wait_until(Duration::from_secs(2), |state| {
            window_position(state, id) != old
        })
        .await
}

fn window_position(state: &ObservedState, id: u64) -> Option<(Option<usize>, Option<usize>)> {
    state
        .windows
        .iter()
        .find(|window| window.id == id)
        .map(|window| (window.column, window.tile_index))
}

pub fn format_plan(plan: &ReconcilePlan) -> Vec<String> {
    plan.actions.iter().map(format_action).collect()
}

pub fn format_action(action: &Action) -> String {
    match action {
        Action::MoveToWorkspace { name, from, to, .. } => {
            format!("MOVE    {name:<12} workspace {from:?} -> {to}")
        }
        Action::SetFloating { name, floating, .. } => {
            format!(
                "FLOAT   {name:<12} {}",
                if *floating { "floating" } else { "tiling" }
            )
        }
        Action::ExpelRight { name, .. } => format!("EXPEL   {name:<12} from current column"),
        Action::MoveColumnToIndex { name, from, to, .. } => {
            format!("COLUMN  {name:<12} {from:?} -> {to}")
        }
        Action::ConsumeLeft {
            name,
            target_column,
            ..
        } => format!("GROUP   {name:<12} into column {target_column}"),
        Action::MoveWindowUp { name, from, to, .. } => {
            format!("ORDER   {name:<12} tile {from} -> {to} (up)")
        }
        Action::MoveWindowDown { name, from, to, .. } => {
            format!("ORDER   {name:<12} tile {from} -> {to} (down)")
        }
        Action::SetColumnWidth { name, size, .. } => {
            format!("RESIZE  {name:<12} column width -> {size}")
        }
        Action::SetWindowHeight { name, size, .. } => {
            format!("RESIZE  {name:<12} window height -> {size}")
        }
        Action::SetColumnDisplay {
            name,
            column,
            display,
            ..
        } => format!("DISPLAY {name:<12} column {column} -> {display:?}"),
        Action::Focus { name, .. } => format!("FOCUS   {name}"),
    }
}

#[cfg(test)]
mod tests {
    use niri_ipc::SizeChange;
    use workbench_core::Size;

    use super::size_change;

    #[test]
    fn percentage_size_uses_niri_percent_units() {
        assert_eq!(
            size_change(Size::Percent(0.45)),
            SizeChange::SetProportion(45.0)
        );
    }
}
