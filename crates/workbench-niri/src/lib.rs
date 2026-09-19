mod client;
mod executor;

pub use client::{NiriError, NiriSession, SpawnError, TargetOutput};
pub use executor::{
    ApplyError, ApplyReport, execute_action, format_action, format_plan, reconcile,
};
