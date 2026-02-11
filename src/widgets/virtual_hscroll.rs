// Copyright 2026 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

// #![warn(missing_docs)]

use std::collections::BTreeMap;
use std::ops::Range;

use understory_virtual_list::{ScrollAlign, VirtualList};

use xilem::dpi::PhysicalPosition;

use masonry::core::keyboard::{Key, KeyState, NamedKey};
use masonry::core::{
    AccessCtx, AccessEvent, ChildrenIds, ComposeCtx, EventCtx, KeyboardEvent, LayoutCtx,
    MeasureCtx, NewWidget, PaintCtx, PointerEvent, PointerScrollEvent, PropertiesMut,
    PropertiesRef, RegisterCtx, TextEvent, Update, UpdateCtx, Widget, WidgetMut, WidgetPod,
};
use masonry::kurbo::{Axis, Point, Size, Vec2};
use masonry::layout::{LenDef, LenReq, SizeDef};
use masonry::util::debug_panic;

mod sparse;

#[derive(Debug)]
pub struct VirtualHScrollFetchAction {
    /// The range of children ids which were "active" before this change.
    /// That is, the items which the driver wanted to have available, to properly load what it needs.
    /// Note that many of these items will likely still be active even after this event;
    /// only those which aren't also in `target` must be removed.
    pub old_active: Range<usize>,
    /// The range of items which are now active.
    ///
    /// Note that many of these items will have previously been active before this event (and so require no action);
    /// only those which aren't also in `target` must be removed.
    pub target: Range<usize>,
}

#[derive(Debug)]
pub struct VirtualHScrollScrollAction {
    pub range_in_viewport: Range<usize>,
}

#[derive(Debug)]
pub enum VirtualHScrollAction {
    Fetch(VirtualHScrollFetchAction),
    Scroll(VirtualHScrollScrollAction),
}

pub struct VirtualHScroll {
    virtual_list: VirtualList<sparse::SparsePrefixSumExtentModel<f64>>,

    active_range: Range<usize>,

    action_handled: bool,

    items: BTreeMap<usize, WidgetPod<dyn Widget>>,

    // focused_item: Option<(usize, WidgetPod<dyn Widget>)>,
    anchor_index: usize,
    range_in_viewport: Range<usize>,

    left_to_right: bool,

    autoscroll_velocity: f64,

    warned_not_dense: bool,

    missed_actions_count: u32,
}

impl std::fmt::Debug for VirtualHScroll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualHScroll")
            .field("virtual_list", &self.virtual_list).field("active_range", &self.active_range)
            .field("action_handled", &self.action_handled)
            .field("items", &self.items.keys().collect::<Vec<_>>())
            .field("anchor_index", &self.anchor_index)
            .field("range_in_viewport", &self.range_in_viewport)
            .field("left_to_right", &self.left_to_right)
            .field("autoscroll_velocity", &self.autoscroll_velocity)
            .field("warned_not_dense", &self.warned_not_dense)
            .field("missed_actions_count", &self.missed_actions_count)
            .finish()
    }
}

const DEFAULT_MEAN_ITEM_WIDTH: f64 = 180.;

// --- MARK: BUILDERS
impl VirtualHScroll {
    /// Creates a new virtual scrolling list.
    ///
    /// The item at `initial_anchor` will have its top aligned with the top of
    /// the scroll area to start with.
    ///
    /// Note that it is not possible to add children before the widget is "live".
    /// This is for simplicity, as the set of the children which should be loaded has
    /// not yet been determined.
    pub fn new(initial_anchor: usize, len: usize) -> Self {
        let mut virtual_list = VirtualList::new(
            sparse::SparsePrefixSumExtentModel::new(DEFAULT_MEAN_ITEM_WIDTH, len),
            0.,
            0.,
        );
        virtual_list.scroll_to_index(initial_anchor, ScrollAlign::Nearest);
        Self {
            virtual_list,
            // This range starts intentionally empty, as no items have been loaded.
            active_range: initial_anchor..initial_anchor,
            action_handled: true,
            missed_actions_count: 0,
            items: BTreeMap::default(),
            anchor_index: initial_anchor,
            range_in_viewport: initial_anchor..initial_anchor,
            // scroll_offset_from_anchor: 0.0,
            // mean_item_width: DEFAULT_MEAN_ITEM_WIDTH,
            left_to_right: true,
            autoscroll_velocity: 0.,
            // anchor_width: DEFAULT_MEAN_ITEM_WIDTH,
            warned_not_dense: false,
        }
    }

    /// Sets the range of child ids which are valid.
    ///
    /// Note that this is a half-open range, so the end id of the range is not valid.
    ///
    /// # Panics
    ///
    /// If `valid_range.start >= valid_range.end`.
    /// Note that other empty ranges are fine, although the exact behaviour hasn't been carefully validated.
    #[track_caller]
    pub fn with_len(mut self, len: usize) -> Self {
        self.virtual_list.model_mut().set_len(len);
        self
    }

    /// Sets the direction in which children are laid out.
    pub fn with_direction(mut self, left_to_right: bool) -> Self {
        self.left_to_right = left_to_right;
        self
    }

    /// Sets the auto-scroll velocity.
    pub fn with_autoscroll_velocity(mut self, autoscroll_velocity: f64) -> Self {
        self.autoscroll_velocity = autoscroll_velocity;
        self
    }
}

// --- MARK: METHODS
impl VirtualHScroll {
    /// The number of currently active children in this widget.
    ///
    /// This is intended for sanity-checking of higher-level processes (i.e. so that inconsistencies can be caught early).
    #[expect(
        clippy::len_without_is_empty,
        reason = "The only time the VirtualScroll unloads all children is when given an empty valid range."
    )]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Ensures that the correct follow-up passes are requested after the scroll position changes.
    ///
    /// `size` is the current viewport's size.
    fn post_scroll(&mut self) -> PostScrollResult {
        self.virtual_list.clamp_scroll_to_content();

        let scroll_offset = self.virtual_list.scroll_offset();
        let offset_of_anchor = self.virtual_list.model_mut().offset_at(self.anchor_index);
        if scroll_offset < offset_of_anchor
            || scroll_offset
                >= offset_of_anchor + self.virtual_list.model().extent_at(self.anchor_index)
        {
            PostScrollResult::Layout
        } else {
            PostScrollResult::NoLayout
        }
    }

    /// A wrapper to use [`post_scroll`](Self::post_scroll) in event methods.
    fn event_post_scroll(&mut self, ctx: &mut EventCtx<'_>) {
        match self.post_scroll() {
            PostScrollResult::Layout => ctx.request_layout(),
            PostScrollResult::NoLayout => {}
        }
        ctx.request_compose();
    }

    /// A wrapper to use [`post_scroll`](Self::post_scroll) in update methods.
    fn update_post_scroll(&mut self, ctx: &mut UpdateCtx<'_>) {
        match self.post_scroll() {
            PostScrollResult::Layout => {
                ctx.request_layout();
            }
            PostScrollResult::NoLayout => {}
        }
        ctx.request_compose();
    }

    fn direction_appropriate(&self, delta: f64) -> f64 {
        if self.left_to_right { delta } else { -delta }
    }

    fn scroll_offset_from_anchor(&mut self) -> f64 {
        self.virtual_list.scroll_offset()
            - self.virtual_list.model_mut().offset_at(self.anchor_index)
    }
}

enum PostScrollResult {
    Layout,
    NoLayout,
}

// --- MARK: WIDGETMUT
impl VirtualHScroll {
    /// Indicates that `action` is about to be handled by the driver (which is calling this method).
    ///
    /// This is required because if multiple actions stack up, `VirtualScroll` would assume that they have all been handled.
    /// In particular, this method existing allows layout operations to happen after each individual action is handled, which
    /// achieves several things:
    /// - It improves robustness, by allowing layout methods to know exactly which indices are valid.
    /// - It makes writing drivers easier, as the safety rails in `VirtualScroll` can be more precise.
    // (It also simplifies writing tests)
    // TODO: This could instead take ownership of the action, and return some kind of `{to_remove, to_add}` iterator index pair.
    pub fn will_handle_action(this: &mut WidgetMut<'_, Self>, action: &VirtualHScrollFetchAction) {
        if this.widget.active_range != action.old_active {
            debug_panic!(
                "Handling a VirtualHScrollFetchAction with the wrong range; got {:?}, expected {:?} for widget {}.\n\
                Maybe this has been routed to the wrong `VirtualHScroll`?",
                action.old_active,
                this.widget.active_range,
                this.ctx.widget_id(),
            );
        }
        this.widget.action_handled = true;
        if this.widget.missed_actions_count > 0 {
            // Avoid spamming the "handling single action delay" warning.
            this.widget.missed_actions_count = 1;
        }
        this.widget.active_range = action.target.clone();
        this.ctx.request_layout();
    }

    /// Add the child widget for the given index.
    ///
    /// This should be done only in the handling of a [`VirtualScrollAction`].
    /// This must be called after [`VirtualHScroll::will_handle_action`].
    #[track_caller]
    pub fn add_child(this: &mut WidgetMut<'_, Self>, idx: usize, child: NewWidget<dyn Widget>) {
        // TODO: Maybe just warn?
        debug_assert!(
            this.widget.action_handled,
            "You must call `will_handle_action` before `add_child`."
        );
        debug_assert!(
            this.widget.active_range.contains(&idx),
            "`add_child` should only be called with an index requested by the controller."
        );
        this.ctx.children_changed();
        if this.widget.items.insert(idx, child.to_pod()).is_some() {
            tracing::warn!("Tried to add child {idx} twice to VirtualScroll");
        };
    }

    /// Removes the child widget with id `idx`.
    ///
    /// This will log an error if there was no child at the given index.
    /// This should only happen if the driver does not meet the usage contract.
    ///
    /// This should be done only in the handling of a [`VirtualScrollAction`].
    /// This must be called after [`VirtualHScroll::will_handle_action`].
    ///
    /// Note that if you are changing the valid range, you should *not* remove any active children
    /// outside of that range; instead the controller will send an action removing those children.
    #[track_caller]
    pub fn remove_child(this: &mut WidgetMut<'_, Self>, idx: usize) {
        // TODO: Maybe just warn?
        debug_assert!(
            this.widget.action_handled,
            "You must call `will_handle_action` before `remove_child`."
        );
        debug_assert!(
            !this.widget.active_range.contains(&idx),
            "`remove_child` should only be called with an index which is not active."
        );
        let child = this.widget.items.remove(&idx);
        if let Some(child) = child {
            this.ctx.remove_child(child);
        } else if !this.widget.warned_not_dense {
            // If we have already warned because there's a density problem, don't duplicate it with this error.
            tracing::error!(
                "Tried to remove child ({idx}) which has already been removed or was never added."
            );
        }
    }

    /// Returns mutable reference to the child widget at `idx`.
    ///
    /// # Panics
    ///
    /// If the widget at `idx` is not in the scroll area.
    #[track_caller]
    pub fn child_mut<'t>(
        this: &'t mut WidgetMut<'_, Self>,
        idx: usize,
    ) -> WidgetMut<'t, dyn Widget> {
        let child = this.widget.items.get_mut(&idx).unwrap_or_else(|| {
            panic!(
                "`VirtualHScroll::child_mut` called with non-present index {idx}.\n\
                Active range is {:?}.",
                &this.widget.active_range
            )
        });

        this.ctx.get_mut(child)
    }

    /// Sets the valid range of ids.
    ///
    /// That is, the children which the virtual scrolling area will request within.
    /// Runtime equivalent of [`with_valid_range`](Self::with_valid_range).
    ///
    /// # Panics
    ///
    /// If `valid_range.start >= valid_range.end`.
    /// Note that other empty ranges are fine, although the exact behaviour hasn't been carefully validated.
    pub fn set_len(this: &mut WidgetMut<'_, Self>, len: usize) {
        this.widget.virtual_list.model_mut().set_len(len);
        this.ctx.request_layout();
    }

    /// Sets the direction in which children are laid out.
    pub fn set_direction(this: &mut WidgetMut<'_, Self>, left_to_right: bool) {
        this.widget.left_to_right = left_to_right;
        this.ctx.request_layout();
    }

    /// Sets the auto-scroll velocity.
    pub fn set_autoscroll_velocity(this: &mut WidgetMut<'_, Self>, autoscroll_velocity: f64) {
        this.widget.autoscroll_velocity = autoscroll_velocity;
        this.ctx.request_anim_frame();
    }

    /// Forcefully aligns the top of the item at `idx` with the top of the
    /// virtual scroll area.
    ///
    /// That is, scroll to the item at `idx`, losing any scroll progress by the user.
    ///
    /// This method is mostly useful for tests, but can be used outside of tests
    /// (for example, in certain scrollbar schemes).
    pub fn scroll_to(this: &mut WidgetMut<'_, Self>, idx: usize) {
        this.widget.anchor_index = idx;
        this.widget
            .virtual_list
            .scroll_to_index(idx, ScrollAlign::Start);
        this.ctx.request_layout();
    }
}

// --- MARK: IMPL WIDGET
impl Widget for VirtualHScroll {
    type Action = VirtualHScrollAction;

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if let PointerEvent::Scroll(PointerScrollEvent { delta, .. }) = event {
            let size = ctx.content_box_size();
            // TODO - Remove reference to scale factor.
            // See https://github.com/linebender/xilem/issues/1264
            let scale_factor = ctx.get_scale_factor();
            let line_px = PhysicalPosition {
                x: 120.0 * scale_factor,
                y: 120.0 * scale_factor,
            };
            let page_px = PhysicalPosition {
                x: size.width * scale_factor,
                y: size.height * scale_factor,
            };

            let delta_px = delta.to_pixel_delta(line_px, page_px);
            let logical_delta_px = delta_px.to_logical::<f64>(scale_factor);
            let delta = -if logical_delta_px.x != 0. {
                logical_delta_px.x
            } else {
                logical_delta_px.y
            };
            self.virtual_list
                .scroll_by(self.direction_appropriate(delta));
            self.event_post_scroll(ctx);
        }
    }

    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        // We use an unreasonably large delta (logical pixels) here to allow testing that the case
        // where the scrolling "jumps" the area is handled correctly.
        // In future, this manual testing would be achieved through use of a scrollbar.
        const DELTA_PAGE: f64 = 2000.;

        const DELTA_LINE: f64 = 20.;

        // To get to this state, you currently need to press "tab" to focus this widget in the
        // example.
        let TextEvent::Keyboard(keyboard_event) = event else {
            return;
        };

        match keyboard_event {
            KeyboardEvent {
                state: KeyState::Down,
                key: Key::Named(NamedKey::PageDown),
                ..
            } => {
                self.virtual_list.scroll_by(DELTA_PAGE);
                self.event_post_scroll(ctx);
                ctx.set_handled();
            }
            KeyboardEvent {
                state: KeyState::Down,
                key: Key::Named(NamedKey::PageUp),
                ..
            } => {
                self.virtual_list.scroll_by(-DELTA_PAGE);
                self.event_post_scroll(ctx);
                ctx.set_handled();
            }
            KeyboardEvent {
                state: KeyState::Down,
                key: Key::Named(NamedKey::ArrowLeft),
                ..
            } => {
                self.virtual_list
                    .scroll_by(self.direction_appropriate(DELTA_LINE));
                self.event_post_scroll(ctx);
                ctx.set_handled();
            }
            KeyboardEvent {
                state: KeyState::Down,
                key: Key::Named(NamedKey::ArrowRight),
                ..
            } => {
                self.virtual_list
                    .scroll_by(-self.direction_appropriate(DELTA_LINE));
                self.event_post_scroll(ctx);
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if matches!(
            event.action,
            accesskit::Action::ScrollLeft | accesskit::Action::ScrollRight
        ) {
            let unit = if let Some(accesskit::ActionData::ScrollUnit(unit)) = &event.data {
                *unit
            } else {
                accesskit::ScrollUnit::Item
            };
            let amount = match unit {
                accesskit::ScrollUnit::Item => {
                    self.virtual_list.model().extent_at(self.anchor_index)
                }
                accesskit::ScrollUnit::Page => ctx.content_box_size().width,
            };
            if event.action == accesskit::Action::ScrollLeft {
                self.virtual_list
                    .scroll_by(-self.direction_appropriate(amount));
            } else {
                self.virtual_list
                    .scroll_by(self.direction_appropriate(amount));
            }
            self.event_post_scroll(ctx);
            ctx.set_handled();
        }
    }

    fn on_anim_frame(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        interval: u64,
    ) {
        if self.autoscroll_velocity == 0. {
            return;
        }

        let delta = interval as f64 * 1e-9 * self.autoscroll_velocity;
        self.virtual_list
            .scroll_by(-self.direction_appropriate(delta));
        self.update_post_scroll(ctx);
        ctx.request_anim_frame();
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        // TODO: Register in id order
        for child in self.items.values_mut() {
            ctx.register_child(child);
        }
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _props: &mut PropertiesMut<'_>, event: &Update) {
        if let Update::RequestPanToChild(target) = event {
            let new_pos_x = compute_pan_range(
                0.0..ctx.content_box_size().width,
                target.min_x()..target.max_x(),
            )
            .start;
            self.virtual_list.scroll_by(new_pos_x);
            self.update_post_scroll(ctx);
        }
    }

    fn measure(
        &mut self,
        _ctx: &mut MeasureCtx<'_>,
        _props: &PropertiesRef<'_>,
        _axis: Axis,
        len_req: LenReq,
        _cross_length: Option<f64>,
    ) -> f64 {
        const DEFAULT_LENGTH: f64 = 100.;

        // TODO: Remove HACK: Until scale factor rework happens, just pretend it's always 1.0.
        //       https://github.com/linebender/xilem/issues/1264
        let scale = 1.0;

        match len_req {
            LenReq::MinContent | LenReq::MaxContent => DEFAULT_LENGTH * scale,
            LenReq::FitContent(space) => space,
        }
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
        ctx.set_clip_path(size.to_rect());

        let offset_of_anchor_re_viewport = self.scroll_offset_from_anchor();

        let mut total_children_width = 0.;
        let mut total_children_count = 0usize;
        // Calculate the sizes of all children
        self.virtual_list.model_mut().clear();
        for (idx, child) in &mut self.items {
            let auto_size = SizeDef::fit(size).with_width(LenDef::MaxContent);
            let child_size = ctx.compute_size(child, auto_size, size.into());
            ctx.run_layout(child, child_size);
            self.virtual_list
                .model_mut()
                .set_extent(*idx, child_size.width);
            total_children_width += child_size.width;
            total_children_count += 1;
        }

        if total_children_width != 0. && total_children_count != 0 {
            self.virtual_list
                .model_mut()
                .set_default_extent(total_children_width / total_children_count as f64);
        }

        self.virtual_list.set_viewport_extent(size.width);
        self.virtual_list.set_overscan(size.width, size.width * 2.);

        let offset_of_anchor = self.virtual_list.model_mut().offset_at(self.anchor_index);
        self.virtual_list
            .set_scroll_offset(offset_of_anchor_re_viewport + offset_of_anchor);

        let mut visible_indices = self.virtual_list.visible_indices();
        if let Some(anchor_index) =
            visible_indices.find(|i| self.virtual_list.is_index_partially_visible(*i))
        {
            self.anchor_index = anchor_index;
        }

        let active_range =
            self.virtual_list.visible_strip().start..self.virtual_list.visible_strip().end;
        if self.active_range != active_range {
            ctx.submit_action::<VirtualHScrollAction>(VirtualHScrollAction::Fetch(VirtualHScrollFetchAction {
                old_active: self.active_range.clone(),
                target: active_range.clone(),
            }));
            self.action_handled = false;
        }

        // place children
        let offset_of_anchor = self.virtual_list.model_mut().offset_at(self.anchor_index);
        for (idx, child) in &mut self.items {
            if active_range.contains(idx) {
                let x = self.virtual_list.model_mut().offset_at(*idx) - offset_of_anchor;
                let placed_x = if self.left_to_right {
                    x
                } else {
                    -x - self.virtual_list.model().extent_at(*idx)
                };
                ctx.place_child(child, Point::new(placed_x, 0.));
            } else {
                ctx.set_stashed(child, true);
            }
        }
    }

    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        let x = self.scroll_offset_from_anchor();
        let x = -self.direction_appropriate(x)
            + if self.left_to_right {
                0.
            } else {
                ctx.content_box_size().width
            };
        let translation = Vec2::new(x, 0.);
        for idx in self.active_range.clone() {
            if let Some(child) = self.items.get_mut(&idx) {
                if self.autoscroll_velocity != 0. {
                    ctx.set_animated_child_scroll_translation(child, translation);
                } else {
                    ctx.set_child_scroll_translation(child, translation);
                }
            }
        }

        let mut visible_indices = self.virtual_list.visible_indices();
        if let Some(anchor_index) =
            visible_indices.find(|i| self.virtual_list.is_index_partially_visible(*i))
        {
            let last_visible_index = visible_indices
                .rfind(|i| self.virtual_list.is_index_partially_visible(*i))
                .unwrap_or(anchor_index);
            let new_range_in_viewport = anchor_index..last_visible_index;
            if self.range_in_viewport != new_range_in_viewport.clone() {
                self.range_in_viewport = new_range_in_viewport.clone();
                ctx.submit_action::<VirtualHScrollAction>(VirtualHScrollAction::Scroll(VirtualHScrollScrollAction {
                    range_in_viewport: new_range_in_viewport,
                }));
            }
        }
    }

    fn paint(
        &mut self,
        _ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        _scene: &mut xilem::vello::Scene,
    ) {
        // We run these checks in `paint` as they are outside of the pass-based fixedpoint loop
        if !self.action_handled {
            if self.missed_actions_count == 0 {
                tracing::warn!(
                    "VirtualScroll got to painting without its action (i.e. it's request for items to be loaded) being handled.\n\
                    This means that there was a delay in handling its action for some reason.\n\
                    Maybe your driver only handles one action at a time?"
                );
            }
            if self.missed_actions_count > 10 {
                debug_panic!(
                    "VirtualScroll's action is being missed repeatedly being handled.\n\
                    Note that to handle an action, you must call `VirtualHScroll::will_handle_action` with the action."
                );
                // In release mode, re-send the action, which will hopefully get things unstuck.
                self.action_handled = true;
            }
            self.missed_actions_count += 1;
        }
    }

    fn accessibility_role(&self) -> accesskit::Role {
        accesskit::Role::ScrollView
    }

    fn accessibility(
        &mut self,
        _ctx: &mut AccessCtx<'_>,
        _props: &PropertiesRef<'_>,
        node: &mut accesskit::Node,
    ) {
        node.set_clips_children();
        node.set_orientation(accesskit::Orientation::Vertical);
        // Even when we support infinite scroll in both directions, we need
        // to set scroll_x somehow, so the platform adapter can know when
        // scrolling happened and fire the appropriate platform event;
        // this is particularly important on Android. Here, we assume that
        // in practice, the anchor index is in range for an f64.
        // TBD: Is there a better way to do this?
        if self.anchor_index != 0 && self.anchor_index != usize::MAX {
            let x = (self.anchor_index as f64) * self.virtual_list.model().default_extent()
                + self.scroll_offset_from_anchor();
            node.set_scroll_x(x);
        }
        if self.anchor_index != 0 || self.scroll_offset_from_anchor() > 0. {
            node.add_action(accesskit::Action::ScrollUp);
        }
        let last_visible_index = self.virtual_list.last_visible_index();
        let at_end = last_visible_index.map_or(false, |i| {
            i == self.virtual_list.model().len()
                && self.virtual_list.model_mut().offset_at(i)
                    + self.virtual_list.model().extent_at(i)
                    - self.virtual_list.scroll_offset()
                    - self.virtual_list.viewport_extent()
                    != 0.
        });
        if !at_end {
            node.add_action(accesskit::Action::ScrollDown);
        }
        node.add_child_action(accesskit::Action::ScrollIntoView);
    }

    fn children_ids(&self) -> ChildrenIds {
        self.items.values().map(|pod| pod.id()).collect()
    }

    fn accepts_text_input(&self) -> bool {
        false
    }

    fn accepts_focus(&self) -> bool {
        // Our focus behaviour is not carefully designed.
        // There are a few things to consider:
        // - We want this widget to accept e.g. pagedown events, even when there is no focusable child
        // - We want the keyboard focus to be able to "escape" the virtual list, rather than be trapped.
        // See also the caveat in the main docs for this widget.
        // This is true for now to allow PageDown events to be handled.
        true
    }

    // TODO: Optimise using binary search?
    // fn find_widget_under_pointer(..);

    fn get_debug_text(&self) -> Option<String> {
        Some(format!("{self:#?}"))
    }
}

pub(crate) fn compute_pan_range(mut viewport: Range<f64>, target: Range<f64>) -> Range<f64> {
    // if either range contains the other, the viewport doesn't move
    if target.start <= viewport.start && viewport.end <= target.end {
        return viewport;
    }
    if viewport.start <= target.start && target.end <= viewport.end {
        return viewport;
    }

    // we compute the length that we need to "fit" in our viewport
    let target_width = f64::min(viewport.end - viewport.start, target.end - target.start);
    let viewport_width = viewport.end - viewport.start;

    // Because of the early returns, there are only two cases to consider: we need
    // to move the viewport "left" or "right"
    if viewport.start >= target.start {
        viewport.start = target.end - target_width;
        viewport.end = viewport.start + viewport_width;
    } else {
        viewport.end = target.start + target_width;
        viewport.start = viewport.end - viewport_width;
    }

    viewport
}
