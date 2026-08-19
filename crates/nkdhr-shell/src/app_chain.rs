//! Style-neutral product model for the top-left application chain and the
//! Alt+Tab hold preview. Rendering consumes these snapshots but cannot change
//! grouping, MRU, split conservation, or workspace isolation.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

const DEFAULT_HOLD_DELAY: Duration = Duration::from_millis(260);
const DEFAULT_VISIBLE_NODES: usize = 8;
const MAX_APP_KEY_CHARS: usize = 256;
const MAX_TITLE_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpatialRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SpatialRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        (x.is_finite()
            && y.is_finite()
            && width.is_finite()
            && height.is_finite()
            && width >= 0.0
            && height >= 0.0)
            .then_some(Self {
                x,
                y,
                width,
                height,
            })
    }

    fn center(self) -> (f64, f64) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowSnapshot {
    pub id: u64,
    pub app_key: String,
    pub title: String,
    pub rect: SpatialRect,
    /// Back-to-front index. Greater values are visually above lower values.
    pub stacking_index: usize,
    pub focused: bool,
}

impl WindowSnapshot {
    pub fn new(
        id: u64,
        app_key: impl Into<String>,
        title: impl Into<String>,
        rect: SpatialRect,
        stacking_index: usize,
        focused: bool,
    ) -> Self {
        Self {
            id,
            app_key: canonical_app_key(&app_key.into(), id),
            title: bounded_text(&title.into(), MAX_TITLE_CHARS),
            rect,
            stacking_index,
            focused,
        }
    }
}

/// Canonical grouping identity shared by Wayland app IDs and X11 classes.
/// Unknown IDs stay window-local so unrelated legacy clients never collapse
/// into one accidental application group.
pub fn canonical_app_key(candidate: &str, window_id: u64) -> String {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return format!("window-{window_id}");
    }
    let lowered = trimmed.to_lowercase();
    let without_desktop = lowered.strip_suffix(".desktop").unwrap_or(&lowered);
    let bounded = bounded_text(without_desktop, MAX_APP_KEY_CHARS);
    if bounded.is_empty() {
        format!("window-{window_id}")
    } else {
        bounded
    }
}

fn bounded_text(value: &str, maximum_chars: usize) -> String {
    value.chars().take(maximum_chars).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPageEncoding {
    Single,
    Dots(u8),
    Star,
}

impl AppPageEncoding {
    pub fn for_count(count: usize) -> Self {
        match count {
            0 | 1 => Self::Single,
            2..=5 => Self::Dots(count as u8),
            _ => Self::Star,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppGroup {
    pub app_key: String,
    /// MRU-first window order. The first item is the mother icon.
    pub windows: Vec<u64>,
    pub mother: u64,
    pub page_encoding: AppPageEncoding,
    /// Continuously animated water target. Five represented secondary pages
    /// saturate the mother icon, but the exact count remains in `windows`.
    pub water_fill: f32,
}

impl AppGroup {
    fn from_windows(app_key: String, windows: Vec<u64>) -> Self {
        let count = windows.len();
        Self {
            app_key,
            mother: windows[0],
            page_encoding: AppPageEncoding::for_count(count),
            water_fill: (count.saturating_sub(1).min(5) as f32) / 5.0,
            windows,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChainNode {
    Application(AppGroup),
    /// Dedicated stable node. Expanding it yields the complete remaining app
    /// groups and never discards windows merely because the rail is narrow.
    Overflow {
        applications: Vec<AppGroup>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
struct AppSplit {
    mother: u64,
    unsplit: Vec<u64>,
    left: Vec<u64>,
    right: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSplitSnapshot {
    pub mother: u64,
    pub represented_by_mother: usize,
    pub water_fill: f32,
    pub page_encoding: AppPageEncoding,
    pub left: Vec<u64>,
    pub right: Vec<u64>,
}

impl AppSplit {
    fn new(group: &AppGroup) -> Self {
        Self {
            mother: group.mother,
            unsplit: group.windows.iter().copied().skip(1).collect(),
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    fn snapshot(&self) -> AppSplitSnapshot {
        let represented = self.unsplit.len() + 1;
        AppSplitSnapshot {
            mother: self.mother,
            represented_by_mother: represented,
            water_fill: (represented.saturating_sub(1).min(5) as f32) / 5.0,
            page_encoding: AppPageEncoding::for_count(represented),
            left: self.left.clone(),
            right: self.right.clone(),
        }
    }

    fn total(&self) -> usize {
        1 + self.unsplit.len() + self.left.len() + self.right.len()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SwitcherPhase {
    #[default]
    Dormant,
    Armed,
    Expanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitcherRelease {
    pub selected: u64,
    pub was_expanded: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct SwitcherSession {
    workspace: u16,
    order: Vec<u64>,
    selected_index: usize,
    armed_at: Duration,
    expanded: bool,
    splits: BTreeMap<String, AppSplit>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct WorkspaceState {
    windows: BTreeMap<u64, WindowSnapshot>,
    mru: Vec<u64>,
}

impl WorkspaceState {
    fn sync(&mut self, windows: Vec<WindowSnapshot>) {
        let live = windows
            .iter()
            .map(|window| window.id)
            .collect::<BTreeSet<_>>();
        self.mru.retain(|id| live.contains(id));
        self.windows = windows
            .into_iter()
            .map(|window| (window.id, window))
            .collect();

        let mut unseen = self
            .windows
            .values()
            .filter(|window| !self.mru.contains(&window.id))
            .map(|window| (window.stacking_index, window.id))
            .collect::<Vec<_>>();
        unseen.sort_by_key(|(stacking, _)| std::cmp::Reverse(*stacking));
        self.mru.extend(unseen.into_iter().map(|(_, id)| id));
        if let Some(focused) = self
            .windows
            .values()
            .find(|window| window.focused)
            .map(|window| window.id)
        {
            self.touch(focused);
        }
    }

    fn touch(&mut self, id: u64) {
        if !self.windows.contains_key(&id) {
            return;
        }
        self.mru.retain(|candidate| *candidate != id);
        self.mru.insert(0, id);
    }

    fn groups(&self) -> Vec<AppGroup> {
        let mut grouped = BTreeMap::<String, Vec<u64>>::new();
        for id in &self.mru {
            if let Some(window) = self.windows.get(id) {
                grouped.entry(window.app_key.clone()).or_default().push(*id);
            }
        }
        let rank = self
            .mru
            .iter()
            .enumerate()
            .map(|(rank, id)| (*id, rank))
            .collect::<BTreeMap<_, _>>();
        let mut groups = grouped
            .into_iter()
            .map(|(app_key, windows)| AppGroup::from_windows(app_key, windows))
            .collect::<Vec<_>>();
        groups.sort_by_key(|group| rank[&group.mother]);
        groups
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewNode {
    pub window: u64,
    pub app_key: String,
    pub title: String,
    pub center_x: f32,
    pub center_y: f32,
    pub chain_index: usize,
    pub stacking_index: usize,
    pub selected: bool,
}

/// One model is owned by one physical output. Workspace maps remain local to
/// that output so independent multi-monitor activity cannot steal selection.
#[derive(Debug, Clone)]
pub struct AppChainModel {
    workspaces: BTreeMap<u16, WorkspaceState>,
    active_workspace: u16,
    switcher: Option<SwitcherSession>,
    hold_delay: Duration,
    visible_nodes: usize,
}

impl Default for AppChainModel {
    fn default() -> Self {
        Self {
            workspaces: BTreeMap::new(),
            active_workspace: 1,
            switcher: None,
            hold_delay: DEFAULT_HOLD_DELAY,
            visible_nodes: DEFAULT_VISIBLE_NODES,
        }
    }
}

impl AppChainModel {
    pub fn with_policy(hold_delay: Duration, visible_nodes: usize) -> Self {
        Self {
            hold_delay,
            visible_nodes: visible_nodes.max(1),
            ..Self::default()
        }
    }

    pub fn sync_workspace(&mut self, workspace: u16, windows: Vec<WindowSnapshot>) {
        if self.active_workspace != workspace {
            self.switcher = None;
            self.active_workspace = workspace;
        }
        self.workspaces.entry(workspace).or_default().sync(windows);
        if self
            .switcher
            .as_ref()
            .is_some_and(|session| session.order.iter().any(|id| !self.window_exists(*id)))
        {
            self.switcher = None;
        }
    }

    pub fn active_workspace(&self) -> u16 {
        self.active_workspace
    }

    pub fn chain_nodes(&self) -> Vec<ChainNode> {
        let groups = self
            .workspaces
            .get(&self.active_workspace)
            .map(WorkspaceState::groups)
            .unwrap_or_default();
        if groups.len() <= self.visible_nodes {
            return groups.into_iter().map(ChainNode::Application).collect();
        }
        let visible_applications = self.visible_nodes.saturating_sub(1);
        let mut nodes = groups[..visible_applications]
            .iter()
            .cloned()
            .map(ChainNode::Application)
            .collect::<Vec<_>>();
        nodes.push(ChainNode::Overflow {
            applications: groups[visible_applications..].to_vec(),
        });
        nodes
    }

    /// Immediate focus target for every Tab press. The first chord also arms
    /// the advanced preview, but ordinary short Alt+Tab remains instantaneous.
    pub fn cycle(&mut self, now: Duration) -> Option<u64> {
        if let Some(session) = &mut self.switcher {
            if session.order.is_empty() {
                return None;
            }
            session.selected_index = (session.selected_index + 1) % session.order.len();
            return Some(session.order[session.selected_index]);
        }
        let state = self.workspaces.get(&self.active_workspace)?;
        if state.mru.is_empty() {
            return None;
        }
        let selected_index = usize::from(state.mru.len() > 1);
        let selected = state.mru[selected_index];
        self.switcher = Some(SwitcherSession {
            workspace: self.active_workspace,
            order: state.mru.clone(),
            selected_index,
            armed_at: now,
            expanded: false,
            splits: BTreeMap::new(),
        });
        Some(selected)
    }

    pub fn advance(&mut self, now: Duration, alt_held: bool) -> SwitcherPhase {
        let Some(session) = &mut self.switcher else {
            return SwitcherPhase::Dormant;
        };
        if !alt_held {
            return SwitcherPhase::Armed;
        }
        if now.saturating_sub(session.armed_at) >= self.hold_delay {
            session.expanded = true;
            SwitcherPhase::Expanded
        } else {
            SwitcherPhase::Armed
        }
    }

    pub fn release_alt(&mut self) -> Option<SwitcherRelease> {
        let session = self.switcher.take()?;
        let selected = session.order[session.selected_index];
        if let Some(workspace) = self.workspaces.get_mut(&session.workspace) {
            workspace.touch(selected);
        }
        Some(SwitcherRelease {
            selected,
            was_expanded: session.expanded,
        })
    }

    pub fn switcher_phase(&self) -> SwitcherPhase {
        match self.switcher.as_ref() {
            None => SwitcherPhase::Dormant,
            Some(session) if session.expanded => SwitcherPhase::Expanded,
            Some(_) => SwitcherPhase::Armed,
        }
    }

    pub fn selected_window(&self) -> Option<u64> {
        let session = self.switcher.as_ref()?;
        session.order.get(session.selected_index).copied()
    }

    pub fn split_next(&mut self, app_key: &str, direction: SplitDirection) -> Option<u64> {
        let canonical = canonical_app_key(app_key, 0);
        let group = self
            .workspaces
            .get(&self.active_workspace)?
            .groups()
            .into_iter()
            .find(|group| group.app_key == canonical)?;
        let session = self.switcher.as_mut()?;
        if !session.expanded {
            return None;
        }
        let split = session
            .splits
            .entry(group.app_key.clone())
            .or_insert_with(|| AppSplit::new(&group));
        let window = split.unsplit.pop()?;
        match direction {
            SplitDirection::Left => split.left.push(window),
            SplitDirection::Right => split.right.push(window),
        }
        debug_assert_eq!(split.total(), group.windows.len());
        Some(window)
    }

    pub fn collapse_one(&mut self, app_key: &str, direction: SplitDirection) -> Option<u64> {
        let canonical = canonical_app_key(app_key, 0);
        let session = self.switcher.as_mut()?;
        let split = session.splits.get_mut(&canonical)?;
        let window = match direction {
            SplitDirection::Left => split.left.pop(),
            SplitDirection::Right => split.right.pop(),
        }?;
        split.unsplit.push(window);
        Some(window)
    }

    pub fn split_snapshot(&self, app_key: &str) -> Option<AppSplitSnapshot> {
        let canonical = canonical_app_key(app_key, 0);
        self.switcher
            .as_ref()?
            .splits
            .get(&canonical)
            .map(AppSplit::snapshot)
    }

    pub fn select_window(&mut self, id: u64) -> bool {
        let Some(workspace) = self.workspaces.get_mut(&self.active_workspace) else {
            return false;
        };
        if !workspace.windows.contains_key(&id) {
            return false;
        }
        workspace.touch(id);
        if let Some(session) = &mut self.switcher
            && let Some(index) = session.order.iter().position(|candidate| *candidate == id)
        {
            session.selected_index = index;
        }
        true
    }

    /// Normalized spatial preview. Window centers preserve their exact
    /// relative order and overlap; no collision-avoidance reflow is applied.
    pub fn preview_nodes(&self) -> Vec<PreviewNode> {
        let Some(workspace) = self.workspaces.get(&self.active_workspace) else {
            return Vec::new();
        };
        let selected = self.selected_window();
        let centers = workspace
            .mru
            .iter()
            .enumerate()
            .filter_map(|(chain_index, id)| {
                workspace
                    .windows
                    .get(id)
                    .map(|window| (chain_index, window))
            })
            .map(|(chain_index, window)| {
                (
                    window.id,
                    window.app_key.clone(),
                    window.title.clone(),
                    window.rect.center(),
                    chain_index,
                    window.stacking_index,
                )
            })
            .collect::<Vec<_>>();
        let Some((min_x, max_x, min_y, max_y)) = centers.iter().fold(
            None::<(f64, f64, f64, f64)>,
            |bounds, (_, _, _, p, _, _)| {
                Some(match bounds {
                    None => (p.0, p.0, p.1, p.1),
                    Some((min_x, max_x, min_y, max_y)) => (
                        min_x.min(p.0),
                        max_x.max(p.0),
                        min_y.min(p.1),
                        max_y.max(p.1),
                    ),
                })
            },
        ) else {
            return Vec::new();
        };
        let width = (max_x - min_x).max(f64::EPSILON);
        let height = (max_y - min_y).max(f64::EPSILON);
        let normalize = |value: f64, minimum: f64, extent: f64| {
            if extent <= f64::EPSILON {
                0.5
            } else {
                ((value - minimum) / extent) as f32
            }
        };
        let mut nodes = centers
            .into_iter()
            .map(
                |(window, app_key, title, (x, y), chain_index, stacking_index)| PreviewNode {
                    window,
                    app_key,
                    title,
                    center_x: normalize(x, min_x, width),
                    center_y: normalize(y, min_y, height),
                    chain_index,
                    stacking_index,
                    selected: selected == Some(window),
                },
            )
            .collect::<Vec<_>>();
        nodes.sort_by_key(|node| node.stacking_index);
        nodes
    }

    fn window_exists(&self, id: u64) -> bool {
        self.workspaces
            .get(&self.active_workspace)
            .is_some_and(|workspace| workspace.windows.contains_key(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        id: u64,
        app: &str,
        x: f64,
        y: f64,
        stacking: usize,
        focused: bool,
    ) -> WindowSnapshot {
        WindowSnapshot::new(
            id,
            app,
            format!("window {id}"),
            SpatialRect::new(x, y, 100.0, 80.0).unwrap(),
            stacking,
            focused,
        )
    }

    #[test]
    fn app_groups_use_exact_water_and_badge_count_contract() {
        for count in 1..=7 {
            let mut model = AppChainModel::default();
            model.sync_workspace(
                1,
                (1..=count)
                    .map(|id| {
                        window(
                            id as u64,
                            "Browser.desktop",
                            id as f64,
                            0.0,
                            id,
                            id == count,
                        )
                    })
                    .collect(),
            );
            let nodes = model.chain_nodes();
            let [ChainNode::Application(group)] = nodes.as_slice() else {
                panic!("one app must produce one aggregate node");
            };
            assert_eq!(group.windows.len(), count);
            assert_eq!(group.mother, count as u64);
            assert_eq!(
                group.water_fill,
                (count.saturating_sub(1).min(5) as f32) / 5.0
            );
            assert_eq!(group.page_encoding, AppPageEncoding::for_count(count));
        }
    }

    #[test]
    fn workspace_mru_and_mother_icons_remain_independent() {
        let mut model = AppChainModel::default();
        model.sync_workspace(
            1,
            vec![
                window(1, "app", 0.0, 0.0, 0, true),
                window(2, "app", 10.0, 0.0, 1, false),
            ],
        );
        model.sync_workspace(2, vec![window(3, "app", 0.0, 0.0, 0, true)]);
        assert_eq!(model.active_workspace(), 2);
        let nodes = model.chain_nodes();
        let [ChainNode::Application(group)] = nodes.as_slice() else {
            panic!()
        };
        assert_eq!(group.mother, 3);

        model.sync_workspace(
            1,
            vec![
                window(1, "app", 0.0, 0.0, 0, true),
                window(2, "app", 10.0, 0.0, 1, false),
            ],
        );
        let nodes = model.chain_nodes();
        let [ChainNode::Application(group)] = nodes.as_slice() else {
            panic!()
        };
        assert_eq!(group.mother, 1);
    }

    #[test]
    fn quick_cycle_is_immediate_but_hold_expands_only_after_threshold() {
        let mut model = AppChainModel::with_policy(Duration::from_millis(200), 8);
        model.sync_workspace(
            1,
            vec![
                window(1, "one", 0.0, 0.0, 0, true),
                window(2, "two", 10.0, 0.0, 1, false),
                window(3, "three", 20.0, 0.0, 2, false),
            ],
        );
        assert_eq!(model.cycle(Duration::from_millis(10)), Some(3));
        assert_eq!(
            model.advance(Duration::from_millis(209), true),
            SwitcherPhase::Armed
        );
        assert_eq!(
            model.advance(Duration::from_millis(210), true),
            SwitcherPhase::Expanded
        );
        assert_eq!(model.cycle(Duration::from_millis(220)), Some(2));
        assert_eq!(
            model.release_alt(),
            Some(SwitcherRelease {
                selected: 2,
                was_expanded: true,
            })
        );
        assert_eq!(model.switcher_phase(), SwitcherPhase::Dormant);
    }

    #[test]
    fn left_and_right_split_conserve_every_page_and_selected_page_becomes_mru() {
        let mut model = AppChainModel::with_policy(Duration::ZERO, 8);
        model.sync_workspace(
            1,
            (1..=8)
                .map(|id| window(id, "browser", id as f64, 0.0, id as usize, id == 8))
                .collect(),
        );
        model.cycle(Duration::ZERO);
        assert_eq!(model.advance(Duration::ZERO, true), SwitcherPhase::Expanded);
        assert!(model.split_next("browser", SplitDirection::Left).is_some());
        assert!(model.split_next("browser", SplitDirection::Right).is_some());
        assert!(model.split_next("browser", SplitDirection::Left).is_some());
        assert!(model.split_next("browser", SplitDirection::Right).is_some());
        let split = model.split_snapshot("browser").unwrap();
        assert_eq!(split.represented_by_mother, 4);
        assert_eq!(split.water_fill, 3.0 / 5.0);
        assert_eq!(split.page_encoding, AppPageEncoding::Dots(4));
        assert_eq!(split.left.len() + split.right.len(), 4);

        let selected = split.left[0];
        assert!(model.select_window(selected));
        model.release_alt();
        let nodes = model.chain_nodes();
        let [ChainNode::Application(group)] = nodes.as_slice() else {
            panic!()
        };
        assert_eq!(group.mother, selected);
    }

    #[test]
    fn preview_preserves_relative_position_overlap_and_stacking() {
        let mut model = AppChainModel::with_policy(Duration::ZERO, 8);
        model.sync_workspace(
            1,
            vec![
                window(1, "one", -100.0, -50.0, 0, true),
                window(2, "two", 0.0, 50.0, 2, false),
                window(3, "three", 0.0, 50.0, 1, false),
            ],
        );
        model.cycle(Duration::ZERO);
        model.advance(Duration::ZERO, true);
        let nodes = model.preview_nodes();
        assert_eq!(
            nodes.iter().map(|node| node.window).collect::<Vec<_>>(),
            [1, 3, 2]
        );
        assert_eq!(nodes[1].center_x, nodes[2].center_x);
        assert_eq!(nodes[1].center_y, nodes[2].center_y);
        assert!(nodes[0].center_x < nodes[1].center_x);
        assert!(nodes[0].center_y < nodes[1].center_y);
    }

    #[test]
    fn overflow_is_one_dedicated_node_without_losing_applications() {
        let mut model = AppChainModel::with_policy(DEFAULT_HOLD_DELAY, 3);
        model.sync_workspace(
            1,
            (1..=6)
                .map(|id| {
                    window(
                        id,
                        &format!("app-{id}"),
                        id as f64,
                        0.0,
                        id as usize,
                        id == 6,
                    )
                })
                .collect(),
        );
        let nodes = model.chain_nodes();
        assert_eq!(nodes.len(), 3);
        let ChainNode::Overflow { applications } = &nodes[2] else {
            panic!("last node must be dedicated overflow");
        };
        assert_eq!(applications.len(), 4);
        assert_eq!(
            nodes
                .iter()
                .map(|node| match node {
                    ChainNode::Application(_) => 1,
                    ChainNode::Overflow { applications } => applications.len(),
                })
                .sum::<usize>(),
            6
        );
    }

    #[test]
    fn app_keys_are_bounded_case_folded_and_desktop_suffix_free() {
        assert_eq!(
            canonical_app_key("  Org.Example.App.desktop ", 7),
            "org.example.app"
        );
        assert_eq!(canonical_app_key("", 7), "window-7");
        assert_eq!(canonical_app_key(&"A".repeat(400), 7).chars().count(), 256);
    }
}
