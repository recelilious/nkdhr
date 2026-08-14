use std::{
    any::Any,
    collections::{BTreeSet, HashSet},
    fmt,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use nkdhr_render::{CornerRadii, Point, Rect, Shadow};

use crate::text::{TextLayout, TextWrap};
use crate::theme::{mix, with_alpha};
use crate::{
    ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier,
    MeasureCtx, Modifiers, MotionFamily, MotionFeature, MotionPropertyDomain, MotionRuntimeProfile,
    MotionScopeData, MotionSemanticFamilyData, PaintCtx, PointerButton, Reactive,
    ResolvedSemanticFluid, ScalarMotion, SelectionMassMotion, SelectionMassSample, SemanticRole,
    Semantics, SemanticsCtx, Size, Theme, ThemeReadSet, ThemeSnapshot, UiError, UiEvent, UpdateCtx,
    Widget,
};

use super::surface::{
    SurfaceState, paint_surface, resolve_fluid_material_tones, surface_theme_reads,
};

const DEFAULT_TYPEAHEAD_TIMEOUT: Duration = Duration::from_millis(700);
const TREE_INDENT: f32 = 16.0;
const DISCLOSURE_SIZE: f32 = 20.0;
const REORDER_HANDLE_WIDTH: f32 = 36.0;

/// Stable multi-selection state. Selected identities survive filtering,
/// sorting and virtualization because they are never represented by indices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListMultiSelection {
    cursor: Option<u64>,
    anchor: Option<u64>,
    selected: BTreeSet<u64>,
}

impl ListMultiSelection {
    pub fn new(selected: impl IntoIterator<Item = u64>) -> Self {
        let selected = selected.into_iter().collect::<BTreeSet<_>>();
        let cursor = selected.iter().next().copied();
        Self {
            cursor,
            anchor: cursor,
            selected,
        }
    }

    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    pub fn with_cursor(mut self, cursor: Option<u64>) -> Self {
        self.cursor = cursor;
        self.anchor = cursor;
        self
    }

    pub fn anchor(&self) -> Option<u64> {
        self.anchor
    }

    pub fn contains(&self, identity: u64) -> bool {
        self.selected.contains(&identity)
    }

    pub fn selected(&self) -> impl ExactSizeIterator<Item = u64> + '_ {
        self.selected.iter().copied()
    }

    pub fn clear(&mut self) {
        self.selected.clear();
    }

    pub fn set_cursor(&mut self, cursor: Option<u64>) {
        self.cursor = cursor;
        self.anchor = cursor;
    }

    pub fn set_selected(&mut self, identity: u64, selected: bool) {
        if selected {
            self.selected.insert(identity);
        } else {
            self.selected.remove(&identity);
        }
    }
}

/// Selection binding shared by a List and its ListItems.
#[derive(Debug, Clone)]
pub enum ListSelection {
    Single(Reactive<Option<u64>>),
    Multiple(Reactive<ListMultiSelection>),
}

impl From<Reactive<Option<u64>>> for ListSelection {
    fn from(selection: Reactive<Option<u64>>) -> Self {
        Self::Single(selection)
    }
}

impl From<Reactive<ListMultiSelection>> for ListSelection {
    fn from(selection: Reactive<ListMultiSelection>) -> Self {
        Self::Multiple(selection)
    }
}

impl ListSelection {
    pub fn cursor(&self) -> Option<u64> {
        match self {
            Self::Single(selection) => selection.get(),
            Self::Multiple(selection) => selection.get().cursor,
        }
    }

    pub fn contains(&self, identity: u64) -> bool {
        match self {
            Self::Single(selection) => selection.get() == Some(identity),
            Self::Multiple(selection) => selection.get().contains(identity),
        }
    }

    pub fn is_multiple(&self) -> bool {
        matches!(self, Self::Multiple(_))
    }

    fn select_only(&self, identity: u64) {
        match self {
            Self::Single(selection) => selection.set(Some(identity)),
            Self::Multiple(selection) => selection.update(|selection| {
                selection.selected.clear();
                selection.selected.insert(identity);
                selection.cursor = Some(identity);
                selection.anchor = Some(identity);
            }),
        }
    }

    fn initialize_cursor(&self, identity: u64) {
        match self {
            Self::Single(selection) if selection.get().is_none() => selection.set(Some(identity)),
            Self::Multiple(selection) if selection.get().cursor.is_none() => {
                selection.update(|selection| {
                    selection.cursor = Some(identity);
                    selection.anchor = Some(identity);
                });
            }
            _ => {}
        }
    }

    fn toggle(&self, identity: u64) {
        match self {
            Self::Single(selection) => selection.set(Some(identity)),
            Self::Multiple(selection) => selection.update(|selection| {
                if !selection.selected.remove(&identity) {
                    selection.selected.insert(identity);
                }
                selection.cursor = Some(identity);
                selection.anchor = Some(identity);
            }),
        }
    }

    fn select_without_range(&self, identity: u64, modifiers: Modifiers) {
        match self {
            Self::Single(selection) => selection.set(Some(identity)),
            Self::Multiple(_) if modifiers.shift => {}
            Self::Multiple(_) if modifiers.control => self.toggle(identity),
            Self::Multiple(_) => self.select_only(identity),
        }
    }

    fn select_with_order(&self, identity: u64, modifiers: Modifiers, entries: &[ListEntry]) {
        let Self::Multiple(selection) = self else {
            self.select_only(identity);
            return;
        };
        if modifiers.shift {
            selection.update(|selection| {
                let anchor = selection.anchor.or(selection.cursor).unwrap_or(identity);
                let anchor = entries
                    .iter()
                    .position(|entry| entry.identity == anchor)
                    .unwrap_or_else(|| {
                        entries
                            .iter()
                            .position(|entry| entry.identity == identity)
                            .unwrap_or(0)
                    });
                let target = entries
                    .iter()
                    .position(|entry| entry.identity == identity)
                    .unwrap_or(anchor);
                if !modifiers.control {
                    selection.selected.clear();
                }
                let (start, end) = if anchor <= target {
                    (anchor, target)
                } else {
                    (target, anchor)
                };
                for entry in &entries[start..=end] {
                    if entry.interactive() {
                        selection.selected.insert(entry.identity);
                    }
                }
                selection.cursor = Some(identity);
            });
        } else if modifiers.control {
            self.toggle(identity);
        } else {
            self.select_only(identity);
        }
    }

    fn navigate(&self, identity: u64, modifiers: Modifiers, entries: &[ListEntry]) {
        let Self::Multiple(selection) = self else {
            self.select_only(identity);
            return;
        };
        if modifiers.shift {
            self.select_with_order(identity, modifiers, entries);
        } else if modifiers.control {
            selection.update(|selection| selection.cursor = Some(identity));
        } else {
            self.select_only(identity);
        }
    }

    fn watch_paint(&self, ctx: &mut PaintCtx<'_>) -> SelectionSnapshot {
        match self {
            Self::Single(selection) => SelectionSnapshot::Single(
                ctx.watch(selection, Invalidation::PAINT | Invalidation::SEMANTICS),
            ),
            Self::Multiple(selection) => SelectionSnapshot::Multiple(
                ctx.watch(selection, Invalidation::PAINT | Invalidation::SEMANTICS),
            ),
        }
    }

    fn watch_semantics(&self, ctx: &mut SemanticsCtx<'_>) -> SelectionSnapshot {
        match self {
            Self::Single(selection) => {
                SelectionSnapshot::Single(ctx.watch(selection, Invalidation::SEMANTICS))
            }
            Self::Multiple(selection) => {
                SelectionSnapshot::Multiple(ctx.watch(selection, Invalidation::SEMANTICS))
            }
        }
    }
}

#[derive(Debug, Clone)]
enum SelectionSnapshot {
    Single(Option<u64>),
    Multiple(ListMultiSelection),
}

impl SelectionSnapshot {
    fn cursor(&self) -> Option<u64> {
        match self {
            Self::Single(selection) => *selection,
            Self::Multiple(selection) => selection.cursor,
        }
    }

    fn contains(&self, identity: u64) -> bool {
        match self {
            Self::Single(selection) => *selection == Some(identity),
            Self::Multiple(selection) => selection.contains(identity),
        }
    }
}

/// Declarative metadata for one visible row. A virtualized List contains only
/// the visible entries, while selection may retain identities outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    identity: u64,
    label: String,
    enabled: bool,
    depth: u16,
    expandable: bool,
    expanded: bool,
    loading: bool,
}

impl ListEntry {
    pub fn new(identity: u64, label: impl Into<String>) -> Self {
        Self {
            identity,
            label: label.into(),
            enabled: true,
            depth: 0,
            expandable: false,
            expanded: false,
            loading: false,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn tree(mut self, depth: u16, expanded: bool) -> Self {
        self.depth = depth;
        self.expandable = true;
        self.expanded = expanded;
        self
    }

    pub fn depth(mut self, depth: u16) -> Self {
        self.depth = depth;
        self
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn identity(&self) -> u64 {
        self.identity
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn interactive(&self) -> bool {
        self.enabled && !self.loading
    }
}

/// Stable leading/trailing extent around the materialized rows of a virtual
/// List. The data adapter owns these estimates and keeps them stable while a
/// loading placeholder is replaced by its real row.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ListVirtualWindow {
    before_extent: f32,
    after_extent: f32,
}

impl ListVirtualWindow {
    pub fn new(before_extent: f32, after_extent: f32) -> Result<Self, ListError> {
        if !before_extent.is_finite()
            || before_extent < 0.0
            || !after_extent.is_finite()
            || after_extent < 0.0
        {
            return Err(ListError::InvalidVirtualExtent);
        }
        Ok(Self {
            before_extent,
            after_extent,
        })
    }

    pub fn before_extent(self) -> f32 {
        self.before_extent
    }

    pub fn after_extent(self) -> f32 {
        self.after_extent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListReorder {
    pub identity: u64,
    /// Insert before this stable identity, or at the end when absent.
    pub before: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListTreeToggle {
    pub identity: u64,
    pub expanded: bool,
}

/// One shared-panel vertical list. Entry order, selection and tree/reorder
/// transactions all use stable identities rather than child indices.
pub struct List {
    label: String,
    selection: ListSelection,
    entries: Vec<ListEntry>,
    theme: Arc<Theme>,
    material_tier: MaterialTier,
    paint_panel_surface: bool,
    square_selection_node: bool,
    conserved_fluid_selection: bool,
    fluid_selection_scope: MotionScopeData,
    motion_runtime: Option<Arc<MotionRuntimeProfile>>,
    capabilities: MaterialCapabilities,
    virtual_window: ListVirtualWindow,
    page_step: usize,
    typeahead_timeout: Duration,
    on_reorder: Option<Rc<dyn Fn(ListReorder)>>,
    on_tree_toggle: Option<Rc<dyn Fn(ListTreeToggle)>>,
}

impl List {
    pub fn new(
        label: impl Into<String>,
        selection: Reactive<Option<u64>>,
        item_ids: impl IntoIterator<Item = u64>,
        theme: Arc<Theme>,
    ) -> Result<Self, ListError> {
        Self::from_entries(
            label,
            selection,
            item_ids
                .into_iter()
                .map(|identity| ListEntry::new(identity, "")),
            theme,
        )
    }

    pub fn from_entries(
        label: impl Into<String>,
        selection: impl Into<ListSelection>,
        entries: impl IntoIterator<Item = ListEntry>,
        theme: Arc<Theme>,
    ) -> Result<Self, ListError> {
        let entries = entries.into_iter().collect::<Vec<_>>();
        let unique = entries
            .iter()
            .map(|entry| entry.identity)
            .collect::<HashSet<_>>();
        if unique.len() != entries.len() {
            return Err(ListError::DuplicateItemIdentity);
        }
        Ok(Self {
            label: label.into(),
            selection: selection.into(),
            entries,
            theme,
            material_tier: MaterialTier::ContentSurface,
            paint_panel_surface: true,
            square_selection_node: false,
            conserved_fluid_selection: false,
            fluid_selection_scope: MotionScopeData::transition(
                MotionSemanticFamilyData::ListTransfer,
                "list",
                "selection",
            ),
            motion_runtime: None,
            capabilities: MaterialCapabilities::default(),
            virtual_window: ListVirtualWindow::default(),
            page_step: 5,
            typeahead_timeout: DEFAULT_TYPEAHEAD_TIMEOUT,
            on_reorder: None,
            on_tree_toggle: None,
        })
    }

    pub fn capabilities(mut self, capabilities: MaterialCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Shared panel material for this list family. Ordinary object lists keep
    /// `ContentSurface`; embedded navigation may use `Ghost` without replacing
    /// List's continuous selection, focus and keyboard behavior.
    pub fn material_tier(mut self, tier: MaterialTier) -> Self {
        self.material_tier = tier;
        self
    }

    /// Selects whether List paints its own shared panel. A list embedded in a
    /// material-owning shell may delegate that one background while retaining
    /// selection mass, separators, focus, scrolling and keyboard behavior.
    pub fn panel_surface(mut self, visible: bool) -> Self {
        self.paint_panel_surface = visible;
        self
    }

    /// Uses a centered, elevated square for compact icon navigation while
    /// preserving the default full-row selection geometry for ordinary lists.
    pub fn square_selection_node(mut self, square: bool) -> Self {
        self.square_selection_node = square;
        self
    }

    /// Moves one conserved selection quantity through the visible topology.
    /// Rapid retargeting keeps the current distribution and tangent instead
    /// of restarting a canned clip. This is opt-in because object lists retain
    /// their quieter scalar selection by default.
    pub fn conserved_fluid_selection(mut self, enabled: bool) -> Self {
        self.conserved_fluid_selection = enabled;
        self
    }

    /// Gives a fluid list a stable component scope for professional motion
    /// overrides. Invalid scopes fail at the same runtime resolution boundary
    /// as all other authored motion rather than silently falling back.
    pub fn fluid_selection_scope(mut self, scope: MotionScopeData) -> Self {
        self.fluid_selection_scope = scope;
        self
    }

    pub fn virtual_window(mut self, window: ListVirtualWindow) -> Self {
        self.virtual_window = window;
        self
    }

    pub fn page_step(mut self, rows: usize) -> Result<Self, ListError> {
        if rows == 0 {
            return Err(ListError::InvalidPageStep);
        }
        self.page_step = rows;
        Ok(self)
    }

    pub fn typeahead_timeout(mut self, timeout: Duration) -> Result<Self, ListError> {
        if timeout.is_zero() {
            return Err(ListError::InvalidTypeaheadTimeout);
        }
        self.typeahead_timeout = timeout;
        Ok(self)
    }

    pub fn on_reorder(mut self, callback: impl Fn(ListReorder) + 'static) -> Self {
        self.on_reorder = Some(Rc::new(callback));
        self
    }

    pub fn on_tree_toggle(mut self, callback: impl Fn(ListTreeToggle) + 'static) -> Self {
        self.on_tree_toggle = Some(Rc::new(callback));
        self
    }

    fn entry_position(&self, identity: u64) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.identity == identity)
    }

    fn interactive_position_from(&self, start: usize, direction: isize) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        let mut position = start as isize;
        while (0..self.entries.len() as isize).contains(&position) {
            let index = position as usize;
            if self.entries[index].interactive() {
                return Some(index);
            }
            position += direction;
        }
        None
    }

    fn navigate_position(&self, key: &Key) -> Option<usize> {
        let current = self
            .selection
            .cursor()
            .and_then(|id| self.entry_position(id));
        match key {
            Key::ArrowUp => self.interactive_position_from(
                current.map_or(0, |position| position.saturating_sub(1)),
                -1,
            ),
            Key::ArrowDown => self.interactive_position_from(
                current.map_or(0, |position| position.saturating_add(1)),
                1,
            ),
            Key::Home => self.interactive_position_from(0, 1),
            Key::End => self.interactive_position_from(self.entries.len().saturating_sub(1), -1),
            Key::PageUp => self
                .interactive_position_from(current.unwrap_or(0).saturating_sub(self.page_step), -1),
            Key::PageDown => self.interactive_position_from(
                current
                    .map_or(0, |position| position.saturating_add(self.page_step))
                    .min(self.entries.len().saturating_sub(1)),
                1,
            ),
            _ => None,
        }
    }

    fn reorder_transaction(&self, position: usize, direction: isize) -> Option<ListReorder> {
        let identity = self.entries.get(position)?.identity;
        match direction {
            -1 if position > 0 => Some(ListReorder {
                identity,
                before: Some(self.entries[position - 1].identity),
            }),
            1 if position + 1 < self.entries.len() => Some(ListReorder {
                identity,
                before: self.entries.get(position + 2).map(|entry| entry.identity),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ReorderDrag {
    source: usize,
    origin_y: f32,
    delta_y: f32,
    insertion: usize,
}

#[derive(Debug)]
struct ListState {
    selection_y: ScalarMotion,
    selection_height: ScalarMotion,
    selection_opacity: ScalarMotion,
    last_selection: Option<u64>,
    last_position: Option<usize>,
    initialized: bool,
    fluid_selection: Option<SelectionMassMotion>,
    row_rects: Vec<Rect>,
    reorder_drag: Option<ReorderDrag>,
    typeahead: String,
    last_typeahead: Duration,
}

impl Default for ListState {
    fn default() -> Self {
        Self {
            selection_y: ScalarMotion::settled(0.0),
            selection_height: ScalarMotion::settled(0.0),
            selection_opacity: ScalarMotion::settled(0.0),
            last_selection: None,
            last_position: None,
            initialized: false,
            fluid_selection: None,
            row_rects: Vec::new(),
            reorder_drag: None,
            typeahead: String::new(),
            last_typeahead: Duration::ZERO,
        }
    }
}

impl Widget for List {
    fn theme_reads(&self) -> ThemeReadSet {
        let mut reads = surface_theme_reads(self.material_tier);
        reads.extend([
            "radii.group",
            "radii.control",
            "palette.edge",
            "palette.accent_secondary",
            "motion.mode",
            "motion.speed_multiplier",
            "motion.settle",
            "motion.durations.list_transfer",
        ]);
        reads
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn apply_theme_snapshot(&mut self, snapshot: Arc<ThemeSnapshot>) {
        self.theme = snapshot.theme();
        self.motion_runtime = Some(snapshot.motion_runtime());
    }

    fn create_state(&self) -> Box<dyn Any> {
        Box::<ListState>::default()
    }

    fn update(&self, previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        let previous = previous
            .downcast_ref::<Self>()
            .expect("widget type is reconciled");
        if previous.entries != self.entries || previous.virtual_window != self.virtual_window {
            ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
        } else {
            ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
        }
        if !self.theme.motion.spatial_motion_enabled() {
            let state = ctx.state_mut::<ListState>().expect("List owns ListState");
            state.selection_y.settle(state.selection_y.target());
            state
                .selection_height
                .settle(state.selection_height.target());
            state
                .selection_opacity
                .settle(state.selection_opacity.target());
            if let (Some(selection), Some(fluid)) =
                (self.selection.cursor(), state.fluid_selection.as_mut())
            {
                fluid.settle(selection.to_string());
            }
        } else if !self.conserved_fluid_selection {
            ctx.state_mut::<ListState>()
                .expect("List owns ListState")
                .fluid_selection = None;
        }
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() != self.entries.len() {
            return Err(UiError::ChildCountMismatch {
                expected: self.entries.len(),
                actual: ctx.child_count(),
            });
        }
        let mut width = constraints.min().width;
        let mut height = self.virtual_window.before_extent + self.virtual_window.after_extent;
        for index in 0..ctx.child_count() {
            let child = ctx.measure_child(
                index,
                Constraints::new(Size::new(constraints.min().width, 0.0), constraints.max())?,
            )?;
            width = width.max(child.width);
            height += child.height;
            if !height.is_finite() {
                return Err(UiError::InvalidSize);
            }
        }
        Ok(constraints.constrain(Size::new(width, height)))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        let mut y = rect.y + self.virtual_window.before_extent;
        let mut row_rects = Vec::with_capacity(ctx.child_count());
        for index in 0..ctx.child_count() {
            let size = ctx.child_size(index)?;
            let child_rect = Rect::new(rect.x, y, rect.width, size.height);
            ctx.arrange_child(index, child_rect)?;
            row_rects.push(child_rect);
            y += size.height;
        }
        ctx.state_mut::<ListState>()?.row_rects = row_rects;
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let selection = self.selection.watch_paint(ctx);
        let rect = ctx.rect();
        if self.paint_panel_surface {
            paint_surface(
                ctx.builder(),
                rect,
                CornerRadii::all(self.theme.radii.group),
                &self.theme,
                self.material_tier,
                self.capabilities,
                SurfaceState::default(),
            )?;
        }

        let selected_position = selection
            .cursor()
            .and_then(|identity| self.entry_position(identity))
            .filter(|position| *position < ctx.child_count());
        let selected_rect = selected_position
            .map(|position| ctx.child_rect(position))
            .transpose()?;
        let now = ctx.now();
        let single = matches!(selection, SelectionSnapshot::Single(_));
        let fluid_policy = if self.conserved_fluid_selection && single {
            self.motion_runtime
                .as_ref()
                .map(|runtime| {
                    Ok::<_, UiError>((
                        runtime
                            .resolve(&self.fluid_selection_scope, MotionPropertyDomain::Spatial)
                            .map_err(|error| UiError::Text(error.to_string()))?,
                        runtime
                            .resolve_fluid(&self.fluid_selection_scope)
                            .map_err(|error| UiError::Text(error.to_string()))?,
                        runtime.allows(MotionFeature::FluidTopology),
                    ))
                })
                .transpose()?
        } else {
            None
        };
        let (single_mass, fluid_sample, active, drag) = {
            let state = ctx.state_mut::<ListState>()?;
            if single && !state.initialized {
                if let Some(selected) = selected_rect {
                    state.selection_y.settle(selected.y);
                    state.selection_height.settle(selected.height);
                    state.selection_opacity.settle(1.0);
                }
                if fluid_policy.is_some()
                    && let Some(identity) = selection.cursor()
                {
                    state.fluid_selection = Some(
                        SelectionMassMotion::new(identity.to_string(), 1.0)
                            .map_err(|error| UiError::Text(error.to_string()))?,
                    );
                }
                state.last_selection = selection.cursor();
                state.last_position = selected_position;
                state.initialized = true;
            } else if single
                && (state.last_selection != selection.cursor()
                    || state.last_position != selected_position)
            {
                if let Some(selected) = selected_rect {
                    if let (Some((spec, _, topology_allowed)), Some(identity)) =
                        (fluid_policy.as_ref(), selection.cursor())
                    {
                        let initial = state.last_selection.unwrap_or(identity).to_string();
                        let fluid = match state.fluid_selection.as_mut() {
                            Some(fluid) => fluid,
                            None => {
                                state.fluid_selection = Some(
                                    SelectionMassMotion::new(initial, 1.0)
                                        .map_err(|error| UiError::Text(error.to_string()))?,
                                );
                                state
                                    .fluid_selection
                                    .as_mut()
                                    .expect("fluid selection was just initialized")
                            }
                        };
                        if *topology_allowed && !spec.is_immediate() {
                            let _ = fluid.retarget(now, identity.to_string(), spec.clone());
                        } else {
                            fluid.settle(identity.to_string());
                        }
                        state.selection_y.settle(selected.y);
                        state.selection_height.settle(selected.height);
                    } else if state
                        .last_position
                        .is_some_and(|previous| previous.abs_diff(selected_position.unwrap()) <= 3)
                        && self.theme.motion.spatial_motion_enabled()
                    {
                        let spec = self.theme.motion.spec(MotionFamily::ListTransfer);
                        state.selection_y.retarget(now, selected.y, spec);
                        state.selection_height.retarget(now, selected.height, spec);
                    } else {
                        state.selection_y.settle(selected.y);
                        state.selection_height.settle(selected.height);
                    }
                    state.selection_opacity.retarget(
                        now,
                        1.0,
                        self.theme.motion.spec(MotionFamily::ListTransfer),
                    );
                } else {
                    state.selection_opacity.retarget(
                        now,
                        0.0,
                        self.theme.motion.spec(MotionFamily::ListTransfer),
                    );
                }
                state.last_selection = selection.cursor();
                state.last_position = selected_position;
            } else if !single {
                state.selection_opacity.settle(0.0);
                state.fluid_selection = None;
                state.last_selection = selection.cursor();
                state.last_position = selected_position;
                state.initialized = true;
            }
            if let (Some((spec, _, topology_allowed)), Some(identity), Some(fluid)) = (
                fluid_policy.as_ref(),
                selection.cursor(),
                state.fluid_selection.as_mut(),
            ) && (!*topology_allowed || spec.is_immediate())
            {
                fluid.settle(identity.to_string());
            }
            let fluid_sample = state
                .fluid_selection
                .as_mut()
                .map(|fluid| fluid.advance(now).sample);
            let fluid_active = fluid_sample
                .as_ref()
                .is_some_and(|sample| sample.active_run.is_some());
            (
                (
                    state.selection_y.value(now),
                    state.selection_height.value(now),
                    state.selection_opacity.value(now).clamp(0.0, 1.0),
                ),
                fluid_sample,
                state.selection_y.is_active(now)
                    || state.selection_height.is_active(now)
                    || state.selection_opacity.is_active(now)
                    || fluid_active,
                state.reorder_drag,
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        let fluid_distortion = if single {
            if let (Some(sample), Some((_, fluid, _))) =
                (fluid_sample.as_ref(), fluid_policy.as_ref())
            {
                paint_conserved_selection_mass(
                    ctx,
                    rect,
                    &self.entries,
                    sample,
                    single_mass.2,
                    &self.theme,
                    self.square_selection_node,
                    self.capabilities,
                    fluid,
                    now,
                )?
            } else {
                paint_selection_mass(
                    ctx,
                    rect,
                    single_mass.0,
                    single_mass.1,
                    single_mass.2,
                    &self.theme,
                    self.square_selection_node,
                )?;
                None
            }
        } else {
            for (index, entry) in self.entries.iter().enumerate() {
                if selection.contains(entry.identity) {
                    let row = ctx.child_rect(index)?;
                    paint_selection_mass(
                        ctx,
                        rect,
                        row.y,
                        row.height,
                        1.0,
                        &self.theme,
                        self.square_selection_node,
                    )?;
                }
            }
            None
        };

        for (index, entry) in self.entries.iter().enumerate() {
            let row = ctx.child_rect(index)?;
            for depth in 0..entry.depth {
                let x = row.x + 18.0 + f32::from(depth) * TREE_INDENT;
                ctx.builder().rect(
                    Rect::new(x, row.y, 1.0, row.height),
                    with_alpha(self.theme.palette.edge, 0.10),
                )?;
            }
            if index + 1 < self.entries.len() {
                ctx.builder().rect(
                    Rect::new(
                        rect.x + 12.0,
                        row.bottom(),
                        (rect.width - 24.0).max(0.0),
                        1.0,
                    ),
                    with_alpha(self.theme.palette.edge, 0.08),
                )?;
            }
        }

        if let Some(drag) = drag {
            let source = ctx.child_rect(drag.source)?;
            ctx.builder().border(
                source.inset(4.0),
                CornerRadii::all(self.theme.radii.control),
                1.0,
                with_alpha(self.theme.palette.edge, 0.34),
            )?;
        }
        for index in 0..ctx.child_count() {
            if drag.is_some_and(|drag| drag.source == index) {
                continue;
            }
            let row = ctx.child_rect(index)?;
            let (offset_x, offset_y) = fluid_distortion
                .map(|distortion| distortion.offset_for(row, index))
                .unwrap_or((0.0, 0.0));
            ctx.paint_child_translated(index, offset_x, offset_y)?;
        }
        if let Some(drag) = drag {
            ctx.paint_child_translated(drag.source, 0.0, drag.delta_y)?;
        }

        for (index, entry) in self.entries.iter().enumerate() {
            let row = ctx.child_rect(index)?;
            let adornment_row = if drag.is_some_and(|drag| drag.source == index) {
                let drag = drag.unwrap();
                Rect::new(row.x, row.y + drag.delta_y, row.width, row.height)
            } else {
                row
            };
            if entry.expandable {
                paint_disclosure(
                    ctx,
                    disclosure_rect(adornment_row, entry.depth),
                    entry.expanded,
                    &self.theme,
                )?;
                if entry.interactive() {
                    ctx.register_pointer_overlay(disclosure_rect(adornment_row, entry.depth))?;
                }
            }
            if self.on_reorder.is_some() && entry.interactive() {
                paint_reorder_handle(ctx, reorder_handle_rect(adornment_row), &self.theme)?;
                ctx.register_pointer_overlay(reorder_handle_rect(adornment_row))?;
            }
        }
        if let Some(drag) = drag {
            let y = insertion_y(drag, &self.entries, ctx)?;
            ctx.builder().rounded_rect(
                Rect::new(rect.x + 8.0, y - 1.0, (rect.width - 16.0).max(0.0), 2.0),
                CornerRadii::all(1.0),
                with_alpha(self.theme.palette.accent_secondary, 0.92),
            )?;
        }
        Ok(())
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        match event {
            UiEvent::PointerDown {
                position,
                button: PointerButton::Primary,
                modifiers,
                ..
            } => {
                let row_rects = ctx.state_mut::<ListState>()?.row_rects.clone();
                for (index, (entry, row)) in self
                    .entries
                    .iter()
                    .zip(row_rects.iter().copied())
                    .enumerate()
                {
                    if entry.expandable
                        && entry.interactive()
                        && disclosure_rect(row, entry.depth).contains(*position)
                    {
                        if let Some(callback) = &self.on_tree_toggle {
                            callback(ListTreeToggle {
                                identity: entry.identity,
                                expanded: !entry.expanded,
                            });
                        }
                        ctx.request_child_focus(index)?;
                        ctx.set_handled();
                        return Ok(());
                    }
                    if self.on_reorder.is_some()
                        && entry.interactive()
                        && reorder_handle_rect(row).contains(*position)
                    {
                        let insertion = reorder_insertion(index, position.y, &row_rects);
                        ctx.state_mut::<ListState>()?.reorder_drag = Some(ReorderDrag {
                            source: index,
                            origin_y: position.y,
                            delta_y: 0.0,
                            insertion,
                        });
                        ctx.request_child_focus(index)?;
                        ctx.capture_pointer();
                        ctx.set_handled();
                        ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                        return Ok(());
                    }
                }
                if self.selection.is_multiple()
                    && let Some(index) = row_rects.iter().position(|row| row.contains(*position))
                    && self.entries[index].interactive()
                {
                    self.selection.select_with_order(
                        self.entries[index].identity,
                        *modifiers,
                        &self.entries,
                    );
                    ctx.request_child_focus(index)?;
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerMoved { position } => {
                let row_rects = ctx.state_mut::<ListState>()?.row_rects.clone();
                if let Some(drag) = &mut ctx.state_mut::<ListState>()?.reorder_drag {
                    drag.delta_y = position.y - drag.origin_y;
                    drag.insertion = reorder_insertion(drag.source, position.y, &row_rects);
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT);
                }
            }
            UiEvent::PointerUp {
                position,
                button: PointerButton::Primary,
                ..
            } => {
                let drag = ctx.state_mut::<ListState>()?.reorder_drag.take();
                if let Some(mut drag) = drag {
                    let row_rects = ctx.state_mut::<ListState>()?.row_rects.clone();
                    drag.insertion = reorder_insertion(drag.source, position.y, &row_rects);
                    let transaction = reorder_from_drag(drag, &self.entries);
                    if let Some(callback) = &self.on_reorder
                        && !reorder_is_noop(transaction, drag.source, &self.entries)
                    {
                        callback(transaction);
                    }
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::PointerCancel => {
                if ctx.state_mut::<ListState>()?.reorder_drag.take().is_some() {
                    ctx.release_pointer();
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT);
                }
            }
            UiEvent::KeyDown {
                key: Key::ArrowUp | Key::ArrowDown,
                modifiers:
                    Modifiers {
                        control: true,
                        shift: true,
                        ..
                    },
                repeat: false,
            } if self.on_reorder.is_some() => {
                let direction = if matches!(
                    event,
                    UiEvent::KeyDown {
                        key: Key::ArrowUp,
                        ..
                    }
                ) {
                    -1
                } else {
                    1
                };
                if let Some(position) = self
                    .selection
                    .cursor()
                    .and_then(|id| self.entry_position(id))
                    && let Some(transaction) = self.reorder_transaction(position, direction)
                {
                    if let Some(callback) = &self.on_reorder {
                        callback(transaction);
                    }
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
                }
            }
            UiEvent::KeyDown {
                key: Key::ArrowLeft | Key::ArrowRight,
                repeat: false,
                ..
            } if self.on_tree_toggle.is_some() => {
                if let Some(position) = self
                    .selection
                    .cursor()
                    .and_then(|id| self.entry_position(id))
                {
                    let entry = &self.entries[position];
                    match key_from_event(event) {
                        Some(Key::ArrowRight) if entry.expandable && !entry.expanded => {
                            self.on_tree_toggle.as_ref().unwrap()(ListTreeToggle {
                                identity: entry.identity,
                                expanded: true,
                            });
                            ctx.set_handled();
                        }
                        Some(Key::ArrowRight) => {
                            let child = self.entries[position + 1..]
                                .iter()
                                .take_while(|candidate| candidate.depth > entry.depth)
                                .position(ListEntry::interactive)
                                .map(|offset| position + 1 + offset);
                            if let Some(child) = child {
                                self.selection.navigate(
                                    self.entries[child].identity,
                                    Modifiers::default(),
                                    &self.entries,
                                );
                                ctx.request_child_focus(child)?;
                                ctx.set_handled();
                            }
                        }
                        Some(Key::ArrowLeft) if entry.expandable && entry.expanded => {
                            self.on_tree_toggle.as_ref().unwrap()(ListTreeToggle {
                                identity: entry.identity,
                                expanded: false,
                            });
                            ctx.set_handled();
                        }
                        Some(Key::ArrowLeft) if entry.depth > 0 => {
                            if let Some(parent) = self.entries[..position]
                                .iter()
                                .rposition(|candidate| candidate.depth < entry.depth)
                            {
                                self.selection.navigate(
                                    self.entries[parent].identity,
                                    Modifiers::default(),
                                    &self.entries,
                                );
                                ctx.request_child_focus(parent)?;
                                ctx.set_handled();
                            }
                        }
                        _ => {}
                    }
                }
            }
            UiEvent::KeyDown {
                key,
                modifiers,
                repeat: false,
            } => {
                if let Some(next) = self.navigate_position(key) {
                    self.selection
                        .navigate(self.entries[next].identity, *modifiers, &self.entries);
                    ctx.request_child_focus(next)?;
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                } else if *key == Key::Escape {
                    ctx.state_mut::<ListState>()?.typeahead.clear();
                }
            }
            UiEvent::TextInput(text) | UiEvent::ImeCommit(text) => {
                let typed = text
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>()
                    .to_lowercase();
                if typed.is_empty() {
                    return Ok(());
                }
                let now = ctx.now();
                let query = {
                    let state = ctx.state_mut::<ListState>()?;
                    if now.saturating_sub(state.last_typeahead) > self.typeahead_timeout {
                        state.typeahead.clear();
                    }
                    state.typeahead.push_str(&typed);
                    state.last_typeahead = now;
                    if state
                        .typeahead
                        .chars()
                        .all(|character| state.typeahead.starts_with(character))
                    {
                        state
                            .typeahead
                            .chars()
                            .next()
                            .map(|character| character.to_string())
                            .unwrap_or_default()
                    } else {
                        state.typeahead.clone()
                    }
                };
                let start = self
                    .selection
                    .cursor()
                    .and_then(|identity| self.entry_position(identity))
                    .map_or(0, |position| (position + 1) % self.entries.len().max(1));
                let found = (0..self.entries.len())
                    .map(|offset| (start + offset) % self.entries.len())
                    .find(|index| {
                        let entry = &self.entries[*index];
                        entry.interactive() && entry.label.to_lowercase().starts_with(&query)
                    });
                if let Some(found) = found {
                    self.selection.navigate(
                        self.entries[found].identity,
                        Modifiers::default(),
                        &self.entries,
                    );
                    ctx.request_child_focus(found)?;
                    ctx.set_handled();
                    ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let selection = self.selection.watch_semantics(ctx);
        let value = match selection {
            SelectionSnapshot::Single(Some(identity)) => Some(format!("selected item {identity}")),
            SelectionSnapshot::Single(None) => None,
            SelectionSnapshot::Multiple(selection) => {
                Some(format!("{} items selected", selection.selected.len()))
            }
        };
        Semantics {
            role: SemanticRole::List,
            label: Some(self.label.clone()),
            value,
            enabled: true,
            focusable: false,
        }
    }

    fn clips_children(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy)]
struct FluidSelectionDistortion {
    center_y: f32,
    reach: f32,
    amplitude: f32,
    phase: f32,
}

impl FluidSelectionDistortion {
    fn offset_for(self, row: Rect, index: usize) -> (f32, f32) {
        if self.reach <= 0.0 || self.amplitude <= 0.0 {
            return (0.0, 0.0);
        }
        let row_center = row.y + row.height * 0.5;
        let influence = (1.0 - (row_center - self.center_y).abs() / self.reach).clamp(0.0, 1.0);
        let wave = (self.phase + index as f32 * 0.91).sin();
        (
            self.amplitude * influence * wave,
            self.amplitude * 0.18 * influence * (self.phase * 0.73 + index as f32).cos(),
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct FluidSelectionNode {
    center_y: f32,
    row_height: f32,
    mass: f32,
    velocity: f32,
}

#[allow(clippy::too_many_arguments)]
fn paint_conserved_selection_mass(
    ctx: &mut PaintCtx<'_>,
    list_rect: Rect,
    entries: &[ListEntry],
    sample: &SelectionMassSample,
    opacity: f32,
    theme: &Theme,
    square: bool,
    capabilities: MaterialCapabilities,
    fluid: &ResolvedSemanticFluid,
    now: Duration,
) -> Result<Option<FluidSelectionDistortion>, UiError> {
    if opacity <= 0.0 || sample.total_mass <= 0.0 {
        return Ok(None);
    }
    let mut nodes = Vec::with_capacity(sample.entries.len());
    for mass in &sample.entries {
        if mass.mass <= f64::EPSILON {
            continue;
        }
        let Ok(identity) = mass.id.parse::<u64>() else {
            continue;
        };
        let Some(index) = entries
            .iter()
            .position(|entry| entry.identity == identity)
            .filter(|index| *index < ctx.child_count())
        else {
            continue;
        };
        let row = ctx.child_rect(index)?;
        nodes.push(FluidSelectionNode {
            center_y: row.y + row.height * 0.5,
            row_height: row.height,
            mass: mass.mass as f32,
            velocity: mass.velocity as f32,
        });
    }
    if nodes.is_empty() {
        return Ok(None);
    }

    let total_mass = sample.total_mass as f32;
    let visible_mass = nodes.iter().map(|node| node.mass).sum::<f32>();
    let weighted_y = nodes
        .iter()
        .map(|node| node.center_y * node.mass)
        .sum::<f32>()
        / visible_mass;
    let row_height = nodes
        .iter()
        .map(|node| node.row_height * node.mass)
        .sum::<f32>()
        / visible_mass;
    let maximum_mass = nodes.iter().map(|node| node.mass).fold(0.0_f32, f32::max);
    let activity = if sample.active_run.is_some() {
        ((1.0 - maximum_mass / total_mass) * 2.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let minimum_y = nodes
        .iter()
        .map(|node| node.center_y)
        .fold(f32::INFINITY, f32::min);
    let maximum_y = nodes
        .iter()
        .map(|node| node.center_y)
        .fold(f32::NEG_INFINITY, f32::max);
    let span = (maximum_y - minimum_y).max(0.0);
    let seed = stable_selection_seed(&sample.target)
        ^ sample
            .active_run
            .map(|run| run.get().wrapping_mul(0x9e37_79b9_7f4a_7c15))
            .unwrap_or(0);
    let transfer_progress = sample
        .entries
        .iter()
        .find(|entry| entry.id == sample.target)
        .map(|entry| entry.mass / sample.total_mass)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    let envelope = fluid.sample(transfer_progress, seed);
    let path_offset = (envelope.path_offset as f32 * 0.18).clamp(-4.5, 4.5);
    let surface = (envelope.surface_displacement as f32).clamp(-4.0, 4.0);
    let base_height = (row_height - 8.0).max(1.0);
    let base_width = if square {
        (list_rect.width - 8.0).max(0.0).min(base_height)
    } else {
        (list_rect.width - 8.0).max(1.0)
    };
    let center_x = list_rect.x + list_rect.width * 0.5;

    if activity > 0.001 && span > 0.5 {
        let extension = ((envelope.neck_extension + envelope.trail_extension) as f32 * 0.16)
            .clamp(0.0, base_height * 0.55);
        let bridge_height = span + base_height * 0.58 + extension;
        let bridge_width_factor = if square { 0.30 } else { 0.27 };
        let bridge_width = (base_width * bridge_width_factor + surface.abs() * 0.22)
            .clamp(if square { 8.0 } else { 18.0 }, base_width * 0.62);
        let bridge = Rect::new(
            center_x + path_offset * 0.56 - bridge_width * 0.5,
            (minimum_y + maximum_y) * 0.5 - bridge_height * 0.5,
            bridge_width,
            bridge_height,
        );
        paint_gel_selection_shape(
            ctx,
            bridge,
            CornerRadii::all((bridge_width * 0.5).min(theme.radii.control)),
            theme,
            capabilities,
            opacity * (0.30 + activity * 0.22),
            true,
        )?;

        for node in &nodes {
            let fraction = (node.mass / total_mass).clamp(0.0, 1.0);
            if fraction <= 0.035 || fraction >= 0.965 {
                continue;
            }
            let lobe_width = base_width * fraction.sqrt() * if square { 0.52 } else { 0.34 };
            let lobe_height = base_height * fraction.sqrt() * 0.58;
            let direction = node.velocity.signum();
            let lobe = Rect::new(
                center_x + path_offset * direction * 0.24 - lobe_width * 0.5,
                node.center_y - lobe_height * 0.5,
                lobe_width.max(4.0),
                lobe_height.max(4.0),
            );
            paint_gel_selection_shape(
                ctx,
                lobe,
                CornerRadii::all((lobe_height * 0.5).min(theme.radii.control)),
                theme,
                capabilities,
                opacity * 0.28 * activity,
                true,
            )?;
        }
    }

    let core_width = base_width * (1.0 - activity * if square { 0.10 } else { 0.16 });
    let core_height = base_height * (1.0 + activity * 0.10) + surface.abs() * 0.16;
    let core = Rect::new(
        center_x + path_offset - core_width * 0.5,
        weighted_y + surface * 0.18 - core_height * 0.5,
        core_width,
        core_height,
    );
    let fluid_radius = theme.radii.control
        + activity * ((core_height * 0.5).min(core_width * 0.5) - theme.radii.control).max(0.0);
    paint_gel_selection_shape(
        ctx,
        core,
        CornerRadii::all(fluid_radius),
        theme,
        capabilities,
        opacity,
        false,
    )?;

    if activity <= 0.001 {
        return Ok(None);
    }
    let momentum = nodes
        .iter()
        .map(|node| node.velocity.abs())
        .fold(0.0_f32, f32::max);
    let phase = now.as_secs_f32() * 8.0 + (seed & 0xffff) as f32 * 0.000_17;
    Ok(Some(FluidSelectionDistortion {
        center_y: weighted_y,
        reach: span * 0.55 + base_height * 1.15,
        amplitude: activity.sqrt() * (1.35 + path_offset.abs() * 0.16 + momentum.min(8.0) * 0.08),
        phase,
    }))
}

fn stable_selection_seed(text: &str) -> u64 {
    text.as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[allow(clippy::too_many_arguments)]
fn paint_gel_selection_shape(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    radii: CornerRadii,
    theme: &Theme,
    capabilities: MaterialCapabilities,
    opacity: f32,
    attached: bool,
) -> Result<(), UiError> {
    if rect.is_empty() || opacity <= 0.0 {
        return Ok(());
    }
    let material = theme.resolve_material(MaterialTier::CompactNode, capabilities);
    let base = mix(material.fill, theme.palette.accent, 0.30);
    let tones = resolve_fluid_material_tones(theme, base, true);
    if !attached {
        ctx.builder().shadow(
            rect,
            radii,
            Shadow::new(3.0, 3.0, 7.0, 0.0, with_alpha(tones.shade, 0.23 * opacity)),
        )?;
    }
    ctx.builder().rounded_rect(
        rect,
        radii,
        with_alpha(base, (if attached { 0.46 } else { 0.76 }) * opacity),
    )?;
    ctx.builder().inset_shadow(
        rect,
        radii,
        Shadow::new(
            2.0,
            2.0,
            if attached { 4.0 } else { 5.0 },
            0.0,
            with_alpha(
                tones.highlight,
                tones.highlight_strength * if attached { 0.27 } else { 0.66 } * opacity,
            ),
        ),
    )?;
    ctx.builder().inset_shadow(
        rect,
        radii,
        Shadow::new(
            -2.0,
            -2.0,
            if attached { 4.0 } else { 5.0 },
            0.0,
            with_alpha(
                tones.shade,
                tones.shade_strength * if attached { 0.30 } else { 0.70 } * opacity,
            ),
        ),
    )?;
    if !attached {
        ctx.builder().border(
            rect,
            radii,
            1.0,
            with_alpha(tones.highlight, 0.04 * opacity),
        )?;
    }
    Ok(())
}

fn paint_selection_mass(
    ctx: &mut PaintCtx<'_>,
    list_rect: Rect,
    y: f32,
    height: f32,
    opacity: f32,
    theme: &Theme,
    square: bool,
) -> Result<(), UiError> {
    if opacity <= 0.0 || height <= 0.0 {
        return Ok(());
    }
    let available_width = (list_rect.width - 8.0).max(0.0);
    let available_height = (height - 8.0).max(0.0);
    let mass = if square {
        let side = available_width.min(available_height);
        Rect::new(
            list_rect.x + (list_rect.width - side).max(0.0) * 0.5,
            y + (height - side).max(0.0) * 0.5,
            side,
            side,
        )
    } else {
        Rect::new(
            list_rect.x + 4.0,
            y + 4.0,
            available_width,
            available_height,
        )
    };
    let base = mix(theme.palette.surface, theme.palette.accent, 0.24);
    let tones = resolve_fluid_material_tones(theme, base, true);
    if square {
        ctx.builder().shadow(
            mass,
            CornerRadii::all(theme.radii.control),
            Shadow::new(3.0, 3.0, 7.0, 0.0, with_alpha(tones.shade, 0.22 * opacity)),
        )?;
    }
    ctx.builder().rounded_rect(
        mass,
        CornerRadii::all(theme.radii.control),
        with_alpha(base, if square { 0.72 } else { 0.58 } * opacity),
    )?;
    ctx.builder().inset_shadow(
        mass,
        CornerRadii::all(theme.radii.control),
        Shadow::new(
            if square { 2.0 } else { 3.0 },
            if square { 2.0 } else { 3.0 },
            if square { 4.5 } else { 9.0 },
            0.0,
            with_alpha(
                tones.highlight,
                tones.highlight_strength * if square { 0.62 } else { 0.46 } * opacity,
            ),
        ),
    )?;
    ctx.builder().inset_shadow(
        mass,
        CornerRadii::all(theme.radii.control),
        Shadow::new(
            if square { -2.0 } else { -3.0 },
            if square { -2.0 } else { -3.0 },
            if square { 4.0 } else { 8.0 },
            0.0,
            with_alpha(
                tones.shade,
                tones.shade_strength * if square { 0.68 } else { 0.52 } * opacity,
            ),
        ),
    )?;
    ctx.builder().border(
        mass,
        CornerRadii::all(theme.radii.control),
        1.0,
        with_alpha(tones.highlight, 0.045 * opacity),
    )?;
    Ok(())
}

fn disclosure_rect(row: Rect, depth: u16) -> Rect {
    Rect::new(
        row.x + 8.0 + f32::from(depth) * TREE_INDENT,
        row.y + (row.height - DISCLOSURE_SIZE).max(0.0) * 0.5,
        DISCLOSURE_SIZE,
        DISCLOSURE_SIZE.min(row.height),
    )
}

fn paint_disclosure(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    expanded: bool,
    theme: &Theme,
) -> Result<(), UiError> {
    let center_x = rect.x + rect.width * 0.5;
    let center_y = rect.y + rect.height * 0.5;
    ctx.builder().rounded_rect(
        Rect::new(center_x - 4.0, center_y - 1.0, 8.0, 2.0),
        CornerRadii::all(1.0),
        with_alpha(theme.palette.text_secondary, 0.82),
    )?;
    if !expanded {
        ctx.builder().rounded_rect(
            Rect::new(center_x - 1.0, center_y - 4.0, 2.0, 8.0),
            CornerRadii::all(1.0),
            with_alpha(theme.palette.text_secondary, 0.82),
        )?;
    }
    Ok(())
}

fn reorder_handle_rect(row: Rect) -> Rect {
    Rect::new(
        row.right() - REORDER_HANDLE_WIDTH,
        row.y,
        REORDER_HANDLE_WIDTH,
        row.height,
    )
}

fn paint_reorder_handle(ctx: &mut PaintCtx<'_>, rect: Rect, theme: &Theme) -> Result<(), UiError> {
    let center_y = rect.y + rect.height * 0.5;
    for offset in [-4.0, 0.0, 4.0] {
        ctx.builder().rounded_rect(
            Rect::new(rect.x + 12.0, center_y + offset - 0.75, 12.0, 1.5),
            CornerRadii::all(0.75),
            with_alpha(theme.palette.text_muted, 0.46),
        )?;
    }
    Ok(())
}

fn reorder_insertion(source: usize, pointer_y: f32, rows: &[Rect]) -> usize {
    rows.iter()
        .enumerate()
        .filter(|(index, _)| *index != source)
        .filter(|(_, row)| pointer_y >= row.y + row.height * 0.5)
        .count()
}

fn reorder_from_drag(drag: ReorderDrag, entries: &[ListEntry]) -> ListReorder {
    let others = entries
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != drag.source)
        .map(|(_, entry)| entry.identity)
        .collect::<Vec<_>>();
    ListReorder {
        identity: entries[drag.source].identity,
        before: others.get(drag.insertion).copied(),
    }
}

fn reorder_is_noop(transaction: ListReorder, source: usize, entries: &[ListEntry]) -> bool {
    transaction.before == entries.get(source + 1).map(|entry| entry.identity)
}

fn insertion_y(
    drag: ReorderDrag,
    entries: &[ListEntry],
    ctx: &mut PaintCtx<'_>,
) -> Result<f32, UiError> {
    let others = (0..entries.len())
        .filter(|index| *index != drag.source)
        .collect::<Vec<_>>();
    if let Some(index) = others.get(drag.insertion) {
        Ok(ctx.child_rect(*index)?.y)
    } else if let Some(index) = others.last() {
        Ok(ctx.child_rect(*index)?.bottom())
    } else {
        Ok(ctx.rect().y)
    }
}

fn key_from_event(event: &UiEvent) -> Option<&Key> {
    match event {
        UiEvent::KeyDown { key, .. } => Some(key),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ListItemBehavior {
    #[default]
    Navigation,
    Object,
}

pub struct ListItem {
    identity: u64,
    label: String,
    selection: ListSelection,
    theme: Arc<Theme>,
    behavior: ListItemBehavior,
    enabled: bool,
    loading: bool,
    tree_depth: u16,
    has_disclosure: bool,
    on_activate: Option<Rc<dyn Fn()>>,
    on_context_menu: Option<Rc<dyn Fn(Point)>>,
}

impl ListItem {
    pub fn new(
        identity: u64,
        label: impl Into<String>,
        selection: Reactive<Option<u64>>,
        theme: Arc<Theme>,
    ) -> Self {
        Self::with_selection(identity, label, selection, theme)
    }

    pub fn with_selection(
        identity: u64,
        label: impl Into<String>,
        selection: impl Into<ListSelection>,
        theme: Arc<Theme>,
    ) -> Self {
        Self {
            identity,
            label: label.into(),
            selection: selection.into(),
            theme,
            behavior: ListItemBehavior::Navigation,
            enabled: true,
            loading: false,
            tree_depth: 0,
            has_disclosure: false,
            on_activate: None,
            on_context_menu: None,
        }
    }

    pub fn behavior(mut self, behavior: ListItemBehavior) -> Self {
        self.behavior = behavior;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn tree(mut self, depth: u16, has_disclosure: bool) -> Self {
        self.tree_depth = depth;
        self.has_disclosure = has_disclosure;
        self
    }

    pub fn on_activate(mut self, callback: impl Fn() + 'static) -> Self {
        self.on_activate = Some(Rc::new(callback));
        self
    }

    pub fn on_context_menu(mut self, callback: impl Fn(Point) + 'static) -> Self {
        self.on_context_menu = Some(Rc::new(callback));
        self
    }

    fn label_inset(&self) -> f32 {
        16.0 + f32::from(self.tree_depth) * TREE_INDENT
            + if self.has_disclosure {
                DISCLOSURE_SIZE
            } else {
                0.0
            }
    }

    fn activate(&self) {
        if let Some(callback) = &self.on_activate {
            callback();
        }
    }
}

#[derive(Debug, Clone)]
struct ListItemState {
    hovered: ScalarMotion,
    focused: bool,
    pressed: bool,
    armed: bool,
    press_modifiers: Modifiers,
    label_layout: Option<Arc<TextLayout>>,
}

impl Default for ListItemState {
    fn default() -> Self {
        Self {
            hovered: ScalarMotion::settled(0.0),
            focused: false,
            pressed: false,
            armed: false,
            press_modifiers: Modifiers::default(),
            label_layout: None,
        }
    }
}

impl Widget for ListItem {
    fn theme_reads(&self) -> ThemeReadSet {
        ThemeReadSet::from_paths([
            "density",
            "radii.control",
            "typography.ui_families",
            "typography.scale",
            "typography.body.font_size",
            "typography.body.line_height",
            "typography.body.weight",
            "palette.accent_secondary",
            "palette.surface_raised",
            "palette.text_muted",
            "palette.text_primary",
            "motion.mode",
            "motion.speed_multiplier",
            "motion.standard",
            "motion.exit",
            "motion.durations.hover_in",
            "motion.durations.hover_out",
        ])
    }

    fn apply_theme(&mut self, theme: Arc<Theme>) {
        self.theme = theme;
    }

    fn create_state(&self) -> Box<dyn Any> {
        Box::<ListItemState>::default()
    }

    fn update(&self, _previous: &dyn Any, ctx: &mut UpdateCtx<'_>) {
        if !self.theme.motion.spatial_motion_enabled() {
            let state = ctx
                .state_mut::<ListItemState>()
                .expect("ListItem owns ListItemState");
            state.hovered.settle(state.hovered.target());
        }
        ctx.invalidate(Invalidation::LAYOUT | Invalidation::SEMANTICS);
    }

    fn measure(&self, ctx: &mut MeasureCtx<'_>, constraints: Constraints) -> Result<Size, UiError> {
        if ctx.child_count() > 1 {
            return Err(UiError::UnexpectedChildCount {
                expected_maximum: 1,
                actual: ctx.child_count(),
            });
        }
        let height = self.theme.density_metrics().row_height;
        let child = if ctx.child_count() == 1 {
            ctx.measure_child(
                0,
                Constraints::new(Size::ZERO, Size::new(constraints.max().width, height))?,
            )?
        } else if self.loading {
            Size::new(120.0, height)
        } else {
            let mut style = self.theme.text_style(crate::TextRole::Body);
            style.wrap = TextWrap::None;
            let layout = ctx.layout_text(&self.label, &style, None)?;
            let size = Size::new(layout.width() + self.label_inset() + 16.0, layout.height());
            ctx.state_mut::<ListItemState>()?.label_layout = Some(layout);
            size
        };
        Ok(constraints.constrain(Size::new(child.width, height.max(child.height))))
    }

    fn arrange(&self, ctx: &mut ArrangeCtx<'_>, rect: Rect) -> Result<(), UiError> {
        if ctx.child_count() == 1 {
            let child = ctx.child_size(0)?;
            let left = self.label_inset();
            ctx.arrange_child(
                0,
                Rect::new(
                    rect.x + left,
                    rect.y + (rect.height - child.height).max(0.0) * 0.5,
                    child.width.min((rect.width - left - 16.0).max(0.0)),
                    child.height.min(rect.height),
                ),
            )?;
        }
        Ok(())
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) -> Result<(), UiError> {
        let selected = match &self.selection {
            ListSelection::Single(selection) => {
                ctx.watch(selection, Invalidation::PAINT) == Some(self.identity)
            }
            ListSelection::Multiple(selection) => ctx
                .watch(selection, Invalidation::PAINT)
                .contains(self.identity),
        };
        let now = ctx.now();
        let draw_internal_label = ctx.child_count() == 0;
        let (hovered, focused, pressed, active, label_layout) = {
            let state = ctx.state_mut::<ListItemState>()?;
            (
                state.hovered.value(now),
                state.focused,
                state.pressed,
                state.hovered.is_active(now),
                state.label_layout.clone(),
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        let rect = ctx.rect().inset(4.0);
        if hovered > 0.0 || focused || pressed {
            ctx.builder().rounded_rect(
                rect,
                CornerRadii::all(self.theme.radii.control),
                with_alpha(
                    if selected {
                        self.theme.palette.accent_secondary
                    } else {
                        self.theme.palette.surface_raised
                    },
                    0.06 + hovered * 0.04 + if pressed { 0.04 } else { 0.0 },
                ),
            )?;
        }
        if focused {
            ctx.builder().border(
                rect,
                CornerRadii::all(self.theme.radii.control),
                2.0,
                with_alpha(self.theme.palette.accent_secondary, 0.80),
            )?;
        }
        if self.loading {
            let row = ctx.rect();
            let y = row.y + row.height * 0.5 - 3.0;
            for (offset, width) in [(0.0, 72.0), (80.0, 28.0)] {
                ctx.builder().rounded_rect(
                    Rect::new(row.x + self.label_inset() + offset, y, width, 6.0),
                    CornerRadii::all(3.0),
                    with_alpha(self.theme.palette.text_muted, 0.18),
                )?;
            }
        } else if draw_internal_label && let Some(layout) = label_layout {
            ctx.draw_text(
                &layout,
                Point::new(
                    ctx.rect().x + self.label_inset(),
                    ctx.rect().y + (ctx.rect().height - layout.height()).max(0.0) * 0.5,
                ),
                if self.enabled {
                    self.theme.palette.text_primary
                } else {
                    self.theme.palette.text_muted
                },
                Some(ctx.rect()),
            )?;
        }
        ctx.paint_children()
    }

    fn event(&self, ctx: &mut EventCtx<'_>, event: &UiEvent) -> Result<(), UiError> {
        match event {
            UiEvent::HoverChanged(hovered) => {
                let now = ctx.now();
                let state = ctx.state_mut::<ListItemState>()?;
                state.hovered.retarget(
                    now,
                    if *hovered { 1.0 } else { 0.0 },
                    self.theme.motion.spec(if *hovered {
                        MotionFamily::HoverIn
                    } else {
                        MotionFamily::HoverOut
                    }),
                );
                if state.pressed {
                    state.armed = *hovered;
                }
                ctx.invalidate(Invalidation::PAINT);
                ctx.request_animation_frame();
            }
            UiEvent::FocusChanged(focused) => {
                ctx.state_mut::<ListItemState>()?.focused = *focused;
                if *focused {
                    self.selection.initialize_cursor(self.identity);
                }
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerDown {
                button: PointerButton::Primary,
                modifiers,
                ..
            } if self.enabled && !self.loading => {
                let state = ctx.state_mut::<ListItemState>()?;
                state.pressed = true;
                state.armed = true;
                state.press_modifiers = *modifiers;
                ctx.request_focus();
                ctx.capture_pointer();
                if self.selection.is_multiple() {
                    ctx.set_handled_and_continue();
                } else {
                    ctx.set_handled();
                }
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::PointerDown {
                position,
                button: PointerButton::Secondary,
                ..
            } if self.enabled && !self.loading && self.on_context_menu.is_some() => {
                if !self.selection.contains(self.identity) {
                    self.selection.select_only(self.identity);
                }
                ctx.request_focus();
                self.on_context_menu.as_ref().unwrap()(*position);
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerMoved { position } => {
                let rect = ctx.rect();
                let state = ctx.state_mut::<ListItemState>()?;
                if state.pressed {
                    state.armed = rect.contains(*position);
                    ctx.set_handled();
                }
            }
            UiEvent::PointerUp {
                position,
                button: PointerButton::Primary,
                click_count,
                ..
            } => {
                let rect = ctx.rect();
                let (activate, modifiers, double_click) = {
                    let state = ctx.state_mut::<ListItemState>()?;
                    let activate = state.pressed && state.armed && rect.contains(*position);
                    let modifiers = state.press_modifiers;
                    state.pressed = false;
                    state.armed = false;
                    let double_click = activate && *click_count >= 2;
                    (activate, modifiers, double_click)
                };
                if activate {
                    if !self.selection.is_multiple() {
                        self.selection
                            .select_without_range(self.identity, modifiers);
                    }
                    if self.behavior == ListItemBehavior::Navigation || double_click {
                        self.activate();
                    }
                }
                ctx.release_pointer();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::PointerCancel => {
                let state = ctx.state_mut::<ListItemState>()?;
                state.pressed = false;
                state.armed = false;
                ctx.release_pointer();
                ctx.invalidate(Invalidation::PAINT);
            }
            UiEvent::KeyDown {
                key: Key::Space,
                repeat: false,
                ..
            } if self.enabled && !self.loading => {
                self.selection.toggle(self.identity);
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::KeyDown {
                key: Key::Enter,
                repeat: false,
                ..
            } if self.enabled && !self.loading => {
                if !self.selection.is_multiple() {
                    self.selection.select_only(self.identity);
                }
                self.activate();
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            UiEvent::KeyDown {
                key: Key::Named(name),
                modifiers,
                repeat: false,
            } if self.enabled
                && !self.loading
                && self.on_context_menu.is_some()
                && (name == "ContextMenu" || (name == "F10" && modifiers.shift)) =>
            {
                if !self.selection.contains(self.identity) {
                    self.selection.select_only(self.identity);
                }
                let rect = ctx.rect();
                self.on_context_menu.as_ref().unwrap()(Point::new(
                    rect.x + rect.width * 0.5,
                    rect.y + rect.height * 0.5,
                ));
                ctx.set_handled();
                ctx.invalidate(Invalidation::PAINT | Invalidation::SEMANTICS);
            }
            _ => {}
        }
        Ok(())
    }

    fn semantics(&self, ctx: &mut SemanticsCtx<'_>) -> Semantics {
        let selected = match &self.selection {
            ListSelection::Single(selection) => {
                ctx.watch(selection, Invalidation::SEMANTICS) == Some(self.identity)
            }
            ListSelection::Multiple(selection) => ctx
                .watch(selection, Invalidation::SEMANTICS)
                .contains(self.identity),
        };
        Semantics {
            role: SemanticRole::ListItem,
            label: Some(self.label.clone()),
            value: if self.loading {
                Some("loading".to_owned())
            } else {
                selected.then(|| "selected".to_owned())
            },
            enabled: self.enabled && !self.loading,
            focusable: self.enabled && !self.loading,
        }
    }

    fn focusable(&self) -> bool {
        self.enabled && !self.loading
    }

    fn accepts_pointer(&self) -> bool {
        self.enabled && !self.loading
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListError {
    DuplicateItemIdentity,
    InvalidVirtualExtent,
    InvalidPageStep,
    InvalidTypeaheadTimeout,
}

impl fmt::Display for ListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DuplicateItemIdentity => "list item identities must be unique",
            Self::InvalidVirtualExtent => "virtual list extents must be finite and non-negative",
            Self::InvalidPageStep => "list page step must be greater than zero",
            Self::InvalidTypeaheadTimeout => "list typeahead timeout must be greater than zero",
        })
    }
}

impl std::error::Error for ListError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_stable_identities_are_rejected() {
        let result = List::new(
            "duplicate",
            Reactive::new(None),
            [7, 7],
            Arc::new(Theme::default()),
        );
        assert!(matches!(result, Err(ListError::DuplicateItemIdentity)));
    }

    #[test]
    fn invalid_virtual_extents_are_rejected() {
        assert!(matches!(
            ListVirtualWindow::new(f32::NAN, 0.0),
            Err(ListError::InvalidVirtualExtent)
        ));
    }

    #[test]
    fn zero_page_step_and_typeahead_timeout_are_rejected() {
        let list = List::new("list", Reactive::new(None), [1], Arc::new(Theme::default())).unwrap();
        assert!(matches!(list.page_step(0), Err(ListError::InvalidPageStep)));

        let list = List::new("list", Reactive::new(None), [1], Arc::new(Theme::default())).unwrap();
        assert!(matches!(
            list.typeahead_timeout(Duration::ZERO),
            Err(ListError::InvalidTypeaheadTimeout)
        ));
    }
}
