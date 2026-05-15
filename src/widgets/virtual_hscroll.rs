// Copyright 2026 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

// #![warn(missing_docs)]

use std::collections::BTreeMap;
use std::ops::Range;

use understory_virtual_list::{ScrollAlign, SparsePrefixSumExtentModel, VirtualList};

use xilem::dpi::PhysicalPosition;

use masonry::core::keyboard::{Key, KeyState, NamedKey};
use masonry::core::{
    AccessCtx, AccessEvent, ChildrenIds, ComposeCtx, EventCtx, KeyboardEvent, LayoutCtx,
    MeasureCtx, NewWidget, PaintCtx, PointerEvent, PointerScrollEvent, PropertiesMut,
    PropertiesRef, RegisterCtx, TextEvent, Update, UpdateCtx, Widget, WidgetMut, WidgetPod,
};
use masonry::kurbo::{Axis, Size};
use masonry::layout::{LenDef, LenReq, SizeDef};
use masonry::util::debug_panic;
use masonry::{accesskit, layout::Length};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    TopToBottom,
    BottomToTop,
    LeftToRight,
    RightToLeft,
}

pub struct VirtualHScroll {
    virtual_list: VirtualList<SparsePrefixSumExtentModel<f64>>,

    active_range: Range<usize>,

    action_handled: bool,

    items: BTreeMap<usize, WidgetPod<dyn Widget>>,

    anchor_index: usize,
    range_in_viewport: Range<usize>,

    start_at: f64,
    end_at: f64,
    direction: ScrollDirection,

    scrolling: bool,

    warned_not_dense: bool,

    missed_actions_count: u32,
}

impl std::fmt::Debug for VirtualHScroll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualHScroll")
            .field("virtual_list", &self.virtual_list)
            .field("active_range", &self.active_range)
            .field("action_handled", &self.action_handled)
            .field("items", &self.items.keys().collect::<Vec<_>>())
            .field("anchor_index", &self.anchor_index)
            .field("range_in_viewport", &self.range_in_viewport)
            .field("start_at", &self.start_at)
            .field("end_at", &self.end_at)
            .field("direction", &self.direction)
            .field("scrolling", &self.scrolling)
            .field("warned_not_dense", &self.warned_not_dense)
            .field("missed_actions_count", &self.missed_actions_count)
            .finish()
    }
}

const DEFAULT_MEAN_ITEM_LENGTH: f64 = 180.;

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
            SparsePrefixSumExtentModel::new(DEFAULT_MEAN_ITEM_LENGTH, len),
            0.,
            0.,
        );
        virtual_list.scroll_to_index(initial_anchor, ScrollAlign::Start);
        Self {
            virtual_list,
            // This range starts intentionally empty, as no items have been loaded.
            active_range: initial_anchor..initial_anchor,
            action_handled: true,
            missed_actions_count: 0,
            items: BTreeMap::default(),
            anchor_index: initial_anchor,
            range_in_viewport: initial_anchor..initial_anchor,
            start_at: 0.,
            end_at: 1.,
            direction: ScrollDirection::TopToBottom,
            scrolling: false,
            warned_not_dense: false,
        }
    }

    /// Sets the number of child ids which are valid.
    #[track_caller]
    pub fn with_len(mut self, len: usize) -> Self {
        self.virtual_list.model_mut().set_len(len);
        self
    }

    /// Sets the points (as ratios of the main-axis length) where the first item starts and
    /// the last item ends in the viewport.
    pub fn with_start_end(mut self, start_at: f64, end_at: f64) -> Self {
        self.start_at = start_at;
        self.end_at = end_at;
        self
    }

    /// Sets the direction in which children are laid out.
    pub fn with_direction(mut self, direction: ScrollDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Sets scrolling state.
    ///
    /// Adjusts pixel snapping for animations.
    pub fn with_scrolling(mut self, scrolling: bool) -> Self {
        self.scrolling = scrolling;
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
        reason = "The only time the VirtualHScroll unloads all children is when given an empty valid range."
    )]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Scroll by the specified amount of pixels.
    pub fn scroll_by(&mut self, delta: f64) {
        self.virtual_list
            .scroll_by(-self.direction.appropriate(delta));
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

    fn scroll_offset_from_anchor(&mut self) -> f64 {
        self.virtual_list.scroll_offset()
            - self.virtual_list.model_mut().offset_at(self.anchor_index)
    }
}

// -- MARK: IMPL OTHERS
impl ScrollDirection {
    fn axis(self) -> Axis {
        match self {
            Self::TopToBottom | Self::BottomToTop => Axis::Vertical,
            Self::LeftToRight | Self::RightToLeft => Axis::Horizontal,
        }
    }

    fn is_reverse(self) -> bool {
        matches!(self, Self::BottomToTop | Self::RightToLeft)
    }

    fn appropriate(self, delta: f64) -> f64 {
        if self.is_reverse() { -delta } else { delta }
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
    /// This is required because if multiple actions stack up, `VirtualHScroll` would assume that they have all been handled.
    /// In particular, this method existing allows layout operations to happen after each individual action is handled, which
    /// achieves several things:
    /// - It improves robustness, by allowing layout methods to know exactly which indices are valid.
    /// - It makes writing drivers easier, as the safety rails in `VirtualHScroll` can be more precise.
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
    /// This should be done only in the handling of a [`VirtualHScrollAction`].
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
            tracing::warn!("Tried to add child {idx} twice to VirtualHScroll");
        };
    }

    /// Removes the child widget with id `idx`.
    ///
    /// This will log an error if there was no child at the given index.
    /// This should only happen if the driver does not meet the usage contract.
    ///
    /// This should be done only in the handling of a [`VirtualHScrollAction`].
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

    /// Sets the point (as ratio of the main-axis length) where the first item starts in the viewport.
    pub fn set_start(this: &mut WidgetMut<'_, Self>, start_at: f64) {
        this.widget.start_at = start_at;
        this.ctx.request_layout();
    }

    /// Sets the point (as ratio of the main-axis length) where the last item ends in the viewport.
    pub fn set_end(this: &mut WidgetMut<'_, Self>, end_at: f64) {
        this.widget.end_at = end_at;
        this.ctx.request_layout();
    }

    /// Sets the direction in which children are laid out.
    pub fn set_direction(this: &mut WidgetMut<'_, Self>, direction: ScrollDirection) {
        this.widget.direction = direction;
        this.ctx.request_layout();
    }

    /// Sets scrolling state.
    ///
    /// Adjusts pixel snapping for animations.
    pub fn set_scrolling(this: &mut WidgetMut<'_, Self>, scrolling: bool) {
        this.widget.scrolling = scrolling;
        this.ctx.request_layout();
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
            let scale_factor = ctx.get_scale_factor();
            let line_px = PhysicalPosition {
                x: 120.0 * scale_factor,
                y: 120.0 * scale_factor,
            };
            let page_px = PhysicalPosition {
                x: size.width * scale_factor,
                y: size.height * scale_factor,
            };

            let delta_px = delta
                .to_pixel_delta(line_px, page_px)
                .to_logical::<f64>(scale_factor);
            let delta = -match (self.direction.axis(), delta_px.x == 0., delta_px.y == 0.) {
                (Axis::Horizontal, false, _) | (Axis::Vertical, _, true) => delta_px.x,
                (Axis::Horizontal, true, _) | (Axis::Vertical, _, false) => delta_px.y,
            };
            self.scroll_by(-delta);
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
        let TextEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key: Key::Named(key),
            ..
        }) = event
        else {
            return;
        };

        // For vertical layouts, PageDown/ArrowDown scroll forward and PageUp/ArrowUp scroll back.
        // For horizontal layouts, PageDown/ArrowRight scroll forward and PageUp/ArrowLeft scroll back.
        // In both cases "forward" means increasing scroll offset (towards end of list).
        // For BottomToTop and RightToLeft, direction.appropriate() negates the delta so that
        // the arrow key which moves visually "forward" still increases the scroll offset correctly.
        let delta = match (key, self.direction.axis()) {
            (NamedKey::PageDown, _) => Some((DELTA_PAGE, true)),
            (NamedKey::PageUp, _) => Some((-DELTA_PAGE, true)),
            (NamedKey::ArrowDown, Axis::Vertical) => Some((DELTA_LINE, false)),
            (NamedKey::ArrowUp, Axis::Vertical) => Some((-DELTA_LINE, false)),
            (NamedKey::ArrowLeft, Axis::Horizontal) => Some((DELTA_LINE, false)),
            (NamedKey::ArrowRight, Axis::Horizontal) => Some((-DELTA_LINE, false)),
            _ => None,
        };
        if let Some((delta, direct)) = delta {
            self.virtual_list.scroll_by(if direct {
                delta
            } else {
                self.direction.appropriate(delta)
            });
            self.event_post_scroll(ctx);
            ctx.set_handled();
        }
    }

    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _props: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        let backscroll = match event.action {
            accesskit::Action::ScrollLeft | accesskit::Action::ScrollUp => true,
            accesskit::Action::ScrollRight | accesskit::Action::ScrollDown => false,
            _ => return,
        };

        let delta = match event.data {
            Some(accesskit::ActionData::ScrollUnit(accesskit::ScrollUnit::Page)) => {
                ctx.content_box_size().get_coord(self.direction.axis())
            }
            _ => self.virtual_list.model().extent_at(self.anchor_index),
        };

        self.scroll_by(if backscroll { delta } else { -delta });
        self.event_post_scroll(ctx);
        ctx.set_handled();
    }

    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        for child in self.items.values_mut() {
            ctx.register_child(child);
        }
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _props: &mut PropertiesMut<'_>, event: &Update) {
        if let Update::RequestPanToChild(target) = event {
            let content_box_length = ctx.content_box_size().get_coord(self.direction.axis());
            let target = target.get_coords(self.direction.axis());
            let new_pos = compute_pan_range(0.0..content_box_length, target).start;
            self.virtual_list.scroll_by(new_pos);
            self.update_post_scroll(ctx);
        }
    }

    fn measure(
        &mut self,
        _ctx: &mut MeasureCtx<'_>,
        _props: &PropertiesRef<'_>,
        _axis: Axis,
        len_req: LenReq,
        _cross_length: Option<Length>,
    ) -> Length {
        const DEFAULT_LENGTH: Length = Length::const_px(100.);

        match len_req {
            LenReq::MinContent | LenReq::MaxContent => DEFAULT_LENGTH,
            LenReq::FitContent(space) => space,
        }
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _props: &PropertiesRef<'_>, size: Size) {
        ctx.set_clip_path(size.to_rect());

        let offset_of_anchor_re_viewport = self.scroll_offset_from_anchor();

        let mut total_children_length = 0.;
        let mut total_children_count = 0usize;
        // Calculate the sizes of all children
        self.virtual_list.model_mut().clear();
        for (idx, child) in &mut self.items {
            let auto_size = SizeDef::fit(size).with(self.direction.axis(), LenDef::MaxContent);
            let child_size = ctx.compute_size(child, auto_size, size.into());
            ctx.run_layout(child, child_size);
            let child_length = child_size.get_coord(self.direction.axis());
            self.virtual_list.model_mut().set_extent(*idx, child_length);
            total_children_length += child_length;
            total_children_count += 1;
        }

        if total_children_length != 0. && total_children_count != 0 {
            self.virtual_list
                .model_mut()
                .set_default_extent(total_children_length / total_children_count as f64);
        }

        let main_axis_length = size.get_coord(self.direction.axis());
        let start_at = self.start_at * main_axis_length;
        let end_at = self.end_at * main_axis_length;
        self.virtual_list.set_viewport_extent(end_at - start_at);
        self.virtual_list
            .set_overscan(main_axis_length + start_at, main_axis_length * 3. - end_at);

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
            ctx.submit_action::<VirtualHScrollAction>(VirtualHScrollAction::Fetch(
                VirtualHScrollFetchAction {
                    old_active: self.active_range.clone(),
                    target: active_range.clone(),
                },
            ));
            self.action_handled = false;
        }

        // place children
        let offset_of_anchor = self.virtual_list.model_mut().offset_at(self.anchor_index);
        for (idx, child) in &mut self.items {
            if active_range.contains(idx) {
                let pos = self.virtual_list.model_mut().offset_at(*idx) - offset_of_anchor;
                let placed_pos = if self.direction.is_reverse() {
                    -pos - self.virtual_list.model().extent_at(*idx)
                } else {
                    pos
                };
                let point = self.direction.axis().pack_point(placed_pos, 0.);
                ctx.place_child(child, point);
            } else {
                ctx.set_stashed(child, true);
            }
        }
    }

    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        let content_length = ctx.content_box_size().get_coord(self.direction.axis());
        let offset = self.scroll_offset_from_anchor() - self.start_at * content_length;
        let offset = if self.direction.is_reverse() {
            content_length - self.direction.appropriate(offset)
        } else {
            -self.direction.appropriate(offset)
        };
        let translation = self.direction.axis().pack_vec2(offset, 0.);
        for idx in self.active_range.clone() {
            if let Some(child) = self.items.get_mut(&idx) {
                if self.scrolling {
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
                ctx.submit_action::<VirtualHScrollAction>(VirtualHScrollAction::Scroll(
                    VirtualHScrollScrollAction {
                        range_in_viewport: new_range_in_viewport,
                    },
                ));
            }
        }
    }

    fn paint(
        &mut self,
        _ctx: &mut PaintCtx<'_>,
        _props: &PropertiesRef<'_>,
        _painter: &mut masonry::imaging::Painter<'_>,
    ) {
        // We run these checks in `paint` as they are outside of the pass-based fixedpoint loop
        if !self.action_handled {
            if self.missed_actions_count == 0 {
                tracing::warn!(
                    "VirtualHScroll got to painting without its action (i.e. it's request for items to be loaded) being handled.\n\
                    This means that there was a delay in handling its action for some reason.\n\
                    Maybe your driver only handles one action at a time?"
                );
            }
            if self.missed_actions_count > 10 {
                debug_panic!(
                    "VirtualHScroll's action is being missed repeatedly being handled.\n\
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
        node.set_orientation(match self.direction.axis() {
            Axis::Horizontal => accesskit::Orientation::Horizontal,
            Axis::Vertical => accesskit::Orientation::Vertical,
        });
        // Even when we support infinite scroll in both directions, we need
        // to set the scroll position somehow, so the platform adapter can know when
        // scrolling happened and fire the appropriate platform event;
        // this is particularly important on Android. Here, we assume that
        // in practice, the anchor index is in range for an f64.
        // TBD: Is there a better way to do this?
        if self.anchor_index != 0 && self.anchor_index != usize::MAX {
            let pos = (self.anchor_index as f64) * self.virtual_list.model().default_extent()
                + self.scroll_offset_from_anchor();
            match self.direction.axis() {
                Axis::Horizontal => node.set_scroll_x(pos),
                Axis::Vertical => node.set_scroll_y(pos),
            }
        }
        // not at top
        if self.anchor_index != 0 || self.scroll_offset_from_anchor() > 0. {
            node.add_action(match self.direction {
                ScrollDirection::TopToBottom => accesskit::Action::ScrollUp,
                ScrollDirection::BottomToTop => accesskit::Action::ScrollDown,
                ScrollDirection::LeftToRight => accesskit::Action::ScrollLeft,
                ScrollDirection::RightToLeft => accesskit::Action::ScrollRight,
            });
        }
        let last_visible_index = self.virtual_list.last_visible_index();
        let at_end = last_visible_index.map_or(false, |index| {
            index == self.virtual_list.model().len()
                && self.virtual_list.model_mut().offset_at(index)
                    + self.virtual_list.model().extent_at(index)
                    - self.virtual_list.scroll_offset()
                    - self.virtual_list.viewport_extent()
                    != 0.
        });
        if !at_end {
            node.add_action(match self.direction {
                ScrollDirection::TopToBottom => accesskit::Action::ScrollDown,
                ScrollDirection::BottomToTop => accesskit::Action::ScrollUp,
                ScrollDirection::LeftToRight => accesskit::Action::ScrollRight,
                ScrollDirection::RightToLeft => accesskit::Action::ScrollLeft,
            });
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

pub(crate) fn compute_pan_range(mut viewport: Range<f64>, target: (f64, f64)) -> Range<f64> {
    // if either range contains the other, the viewport doesn't move
    if target.0 <= viewport.start && viewport.end <= target.1 {
        return viewport;
    }
    if viewport.start <= target.0 && target.1 <= viewport.end {
        return viewport;
    }

    // we compute the length that we need to "fit" in our viewport
    let target_length = (viewport.end - viewport.start).min(target.1 - target.0);
    let viewport_length = viewport.end - viewport.start;

    // Because of the early returns, there are only two cases to consider: we need
    // to move the viewport "left" or "right"
    if viewport.start >= target.0 {
        viewport.start = target.1 - target_length;
        viewport.end = viewport.start + viewport_length;
    } else {
        viewport.end = target.0 + target_length;
        viewport.start = viewport.end - viewport_length;
    }

    viewport
}
