use std::{
    any::Any,
    collections::{BTreeSet, HashSet},
    fmt,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use nkdhr_render::{CornerRadii, Point, Rect};

use crate::text::{TextLayout, TextWrap};
use crate::theme::with_alpha;
use crate::{
    ArrangeCtx, Constraints, EventCtx, Invalidation, Key, MaterialCapabilities, MaterialTier,
    MeasureCtx, Modifiers, MotionFamily, PaintCtx, PointerButton, Reactive, ScalarMotion,
    SemanticRole, Semantics, SemanticsCtx, Size, Theme, ThemeReadSet, UiError, UiEvent, UpdateCtx,
    Widget,
};

use super::surface::{SurfaceState, paint_surface, surface_theme_reads};

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
        paint_surface(
            ctx.builder(),
            rect,
            CornerRadii::all(self.theme.radii.group),
            &self.theme,
            self.material_tier,
            self.capabilities,
            SurfaceState::default(),
        )?;

        let selected_position = selection
            .cursor()
            .and_then(|identity| self.entry_position(identity))
            .filter(|position| *position < ctx.child_count());
        let selected_rect = selected_position
            .map(|position| ctx.child_rect(position))
            .transpose()?;
        let now = ctx.now();
        let (single_mass, active, drag) = {
            let state = ctx.state_mut::<ListState>()?;
            let single = matches!(selection, SelectionSnapshot::Single(_));
            if single && !state.initialized {
                if let Some(selected) = selected_rect {
                    state.selection_y.settle(selected.y);
                    state.selection_height.settle(selected.height);
                    state.selection_opacity.settle(1.0);
                }
                state.last_selection = selection.cursor();
                state.last_position = selected_position;
                state.initialized = true;
            } else if single
                && (state.last_selection != selection.cursor()
                    || state.last_position != selected_position)
            {
                if let Some(selected) = selected_rect {
                    let nearby = state
                        .last_position
                        .is_some_and(|previous| previous.abs_diff(selected_position.unwrap()) <= 3);
                    if nearby && self.theme.motion.spatial_motion_enabled() {
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
                state.last_selection = selection.cursor();
                state.last_position = selected_position;
                state.initialized = true;
            }
            (
                (
                    state.selection_y.value(now),
                    state.selection_height.value(now),
                    state.selection_opacity.value(now).clamp(0.0, 1.0),
                ),
                state.selection_y.is_active(now)
                    || state.selection_height.is_active(now)
                    || state.selection_opacity.is_active(now),
                state.reorder_drag,
            )
        };
        if active {
            ctx.request_animation_frame();
        }
        if matches!(selection, SelectionSnapshot::Single(_)) {
            paint_selection_mass(
                ctx,
                rect,
                single_mass.0,
                single_mass.1,
                single_mass.2,
                &self.theme,
            )?;
        } else {
            for (index, entry) in self.entries.iter().enumerate() {
                if selection.contains(entry.identity) {
                    let row = ctx.child_rect(index)?;
                    paint_selection_mass(ctx, rect, row.y, row.height, 1.0, &self.theme)?;
                }
            }
        }

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
            ctx.paint_child(index)?;
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

fn paint_selection_mass(
    ctx: &mut PaintCtx<'_>,
    list_rect: Rect,
    y: f32,
    height: f32,
    opacity: f32,
    theme: &Theme,
) -> Result<(), UiError> {
    if opacity <= 0.0 || height <= 0.0 {
        return Ok(());
    }
    let mass = Rect::new(
        list_rect.x + 4.0,
        y + 4.0,
        (list_rect.width - 8.0).max(0.0),
        (height - 8.0).max(0.0),
    );
    ctx.builder().rounded_rect(
        mass,
        CornerRadii::all(theme.radii.control),
        with_alpha(theme.palette.accent, 0.18 * opacity),
    )?;
    ctx.builder().border(
        mass,
        CornerRadii::all(theme.radii.control),
        1.0,
        with_alpha(theme.palette.accent_secondary, 0.60 * opacity),
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
