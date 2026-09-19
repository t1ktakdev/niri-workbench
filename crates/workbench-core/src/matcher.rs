use std::collections::HashSet;

use regex::Regex;
use thiserror::Error;

use crate::{MatchSpec, RuntimeWindow, WindowSpec};

#[derive(Debug)]
pub struct CompiledMatcher {
    window_id: Option<u64>,
    app_id: Option<Regex>,
    title: Option<Regex>,
    process: Option<Regex>,
    pid: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: u64,
    pub score: i32,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub pid: Option<i32>,
    pub process_exe: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateDecision {
    None,
    One(Candidate),
    Ambiguous(Vec<Candidate>),
}

#[derive(Debug, Error)]
pub enum MatchError {
    #[error("invalid {field} regex {pattern:?}: {source}")]
    Regex {
        field: &'static str,
        pattern: String,
        #[source]
        source: regex::Error,
    },
}

impl CompiledMatcher {
    pub fn new(spec: &MatchSpec) -> Result<Self, MatchError> {
        Ok(Self {
            window_id: spec.window_id,
            app_id: compile("app_id", spec.app_id.as_deref())?,
            title: compile("title", spec.title.as_deref())?,
            process: compile("process", spec.process.as_deref())?,
            pid: spec.pid,
        })
    }

    pub fn score(&self, window: &RuntimeWindow) -> Option<i32> {
        let mut score = 0;
        if let Some(window_id) = self.window_id {
            if window.id != window_id {
                return None;
            }
            score += 120;
        }
        if let Some(regex) = &self.app_id {
            let value = window.app_id.as_deref()?;
            if !regex.is_match(value) {
                return None;
            }
            score += 50;
        }
        if let Some(regex) = &self.title {
            let value = window.title.as_deref()?;
            if !regex.is_match(value) {
                return None;
            }
            score += 45;
        }
        if let Some(regex) = &self.process {
            let value = window.process_exe.as_deref()?;
            if !regex.is_match(value) {
                return None;
            }
            score += 25;
        }
        if let Some(pid) = self.pid {
            if window.pid != Some(pid) {
                return None;
            }
            score += 80;
        }
        Some(score)
    }
}

pub fn choose_candidate(
    spec: &WindowSpec,
    windows: &[RuntimeWindow],
    used: &HashSet<u64>,
    baseline: Option<&HashSet<u64>>,
) -> Result<CandidateDecision, MatchError> {
    let matcher = CompiledMatcher::new(&spec.match_spec)?;
    let mut candidates: Vec<Candidate> = windows
        .iter()
        .filter(|window| !used.contains(&window.id))
        .filter(|window| baseline.is_none_or(|ids| !ids.contains(&window.id)))
        .filter_map(|window| {
            matcher.score(window).map(|mut score| {
                if baseline.is_some() {
                    score += 100;
                }
                Candidate {
                    id: window.id,
                    score,
                    app_id: window.app_id.clone(),
                    title: window.title.clone(),
                    pid: window.pid,
                    process_exe: window.process_exe.clone(),
                }
            })
        })
        .collect();

    candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));

    match candidates.len() {
        0 => Ok(CandidateDecision::None),
        1 => Ok(CandidateDecision::One(candidates.remove(0))),
        _ => {
            if baseline.is_some() && candidates[0].score > candidates[1].score {
                Ok(CandidateDecision::One(candidates.remove(0)))
            } else {
                Ok(CandidateDecision::Ambiguous(candidates))
            }
        }
    }
}

fn compile(field: &'static str, pattern: Option<&str>) -> Result<Option<Regex>, MatchError> {
    pattern
        .map(|pattern| {
            Regex::new(pattern).map_err(|source| MatchError::Regex {
                field,
                pattern: pattern.to_owned(),
                source,
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{MatchSpec, PlacementSpec, ReusePolicy, RuntimeWindow, WindowSpec};

    use super::{CandidateDecision, choose_candidate};

    fn window(id: u64, app_id: &str, title: &str) -> RuntimeWindow {
        RuntimeWindow {
            id,
            title: Some(title.into()),
            app_id: Some(app_id.into()),
            pid: Some(id as i32),
            process_exe: None,
            workspace_id: Some(1),
            is_focused: false,
            is_floating: false,
            column: Some(id as usize),
            tile_index: Some(1),
            tile_width: 800.0,
            tile_height: 900.0,
        }
    }

    fn spec(title: Option<&str>) -> WindowSpec {
        WindowSpec {
            name: "browser".into(),
            command: vec!["google-chrome-stable".into()],
            match_spec: MatchSpec {
                window_id: None,
                app_id: Some("^google-chrome$".into()),
                title: title.map(str::to_owned),
                process: None,
                pid: None,
            },
            reuse: ReusePolicy::Unique,
            layout: PlacementSpec::default(),
        }
    }

    #[test]
    fn ambiguous_generic_matcher_is_not_guessed() {
        let windows = vec![
            window(1, "google-chrome", "YouTube"),
            window(2, "google-chrome", "docs.rs"),
        ];
        let decision = choose_candidate(&spec(None), &windows, &HashSet::new(), None).unwrap();
        assert!(matches!(decision, CandidateDecision::Ambiguous(_)));
    }

    #[test]
    fn title_disambiguates() {
        let windows = vec![
            window(1, "google-chrome", "YouTube"),
            window(2, "google-chrome", "docs.rs"),
        ];
        let decision =
            choose_candidate(&spec(Some("docs\\.rs")), &windows, &HashSet::new(), None).unwrap();
        assert!(matches!(decision, CandidateDecision::One(candidate) if candidate.id == 2));
    }

    #[test]
    fn spawn_baseline_ignores_existing_windows() {
        let windows = vec![
            window(1, "google-chrome", "YouTube"),
            window(2, "google-chrome", "docs.rs"),
        ];
        let baseline = HashSet::from([1]);
        let decision =
            choose_candidate(&spec(None), &windows, &HashSet::new(), Some(&baseline)).unwrap();
        assert!(matches!(decision, CandidateDecision::One(candidate) if candidate.id == 2));
    }

    #[test]
    fn multiple_new_windows_after_spawn_remain_ambiguous() {
        let windows = vec![
            window(1, "google-chrome", "existing"),
            window(2, "google-chrome", "first new window"),
            window(3, "google-chrome", "second new window"),
        ];
        let baseline = HashSet::from([1]);

        let decision =
            choose_candidate(&spec(None), &windows, &HashSet::new(), Some(&baseline)).unwrap();

        assert!(matches!(decision, CandidateDecision::Ambiguous(c) if c.len() == 2));
    }

    #[test]
    fn missing_app_id_does_not_match_an_app_id_regex() {
        let mut candidate = window(7, "google-chrome", "docs.rs");
        candidate.app_id = None;

        let decision = choose_candidate(&spec(None), &[candidate], &HashSet::new(), None).unwrap();

        assert_eq!(decision, CandidateDecision::None);
    }
}
