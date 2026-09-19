mod config;
mod matcher;
mod model;
mod planner;

pub use config::{ConfigError, load_config, parse_config};
pub use matcher::{Candidate, CandidateDecision, CompiledMatcher, MatchError, choose_candidate};
pub use model::{
    ColumnDisplay, Config, MatchSpec, ObservedState, OutputFallback, OutputInfo, PlacementSpec,
    Recipe, ReusePolicy, RuntimeWindow, RuntimeWorkspace, Size, WindowSpec,
};
pub use planner::{Action, ReconcilePlan, build_reconcile_plan};
