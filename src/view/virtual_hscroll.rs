#![expect(clippy::type_complexity)]

use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Range;

use masonry::core::{Widget, WidgetPod};
use masonry::util::debug_panic;

use xilem::core::{MessageCtx, MessageResult, Mut, View, ViewId, ViewMarker, ViewPathTracker};
use xilem::{Pod, ViewCtx, WidgetView};

use crate::widgets;

/// The view type for [`virtual_hscroll`].
///
/// See its documentation for details.
pub struct VirtualHScroll<State, Action, ChildrenViews, F, G> {
    #[expect(clippy::type_complexity)]
    phantom: PhantomData<fn() -> (WidgetPod<dyn Widget>, State, Action, ChildrenViews)>,
    func: F,
    anchor_index: Option<usize>,
    len: usize,
    start_at: f64,
    end_at: f64,
    left_to_right: bool,
    autoscroll_velocity: f64,
    on_scroll: G,
}

/// A (vertical) virtual scrolling View, for Masonry's [`VirtualScroll`](widgets::VirtualHScroll).
///
/// Virtual scrolling is a technique to improve performance when scrolling through long lists, by
/// only loading (and therefore laying out, drawing, processing for event handling), the items visible to the user.
///
/// The implementation has some caveats, which are discussed in the documentation of the [underlying widget](widgets::VirtualHScroll).
///
/// Whenever this view is rebuilt, all of the loaded children are rebuild.
/// The child creation function is a "component" context, (alike to the usual `app_logic` functions), which means
/// that changing the app's state in this function will *not* cause a rebuild or rerunning of the app
/// logic (this avoids infinite loops).
/// It is correct for `func` to capture, if necessary.
/// However, it also has access to the app's state, so this is unlikely to be needed.
///
/// Arguments:
/// - `len` is the range of ids which are supported.
/// - `func` is the component for this element's children.
///   It is provided with the app's state and the index of the child.
///
/// In rare circumstances, the index of the child could be outside of the requested valid range (this is
/// most likely to happen if the valid range changes due to something in `app_logic` updating it - e.g.
/// if a counter which decrements every time a parent component is called is used for the valid range).
/// As such, you should avoid panicking if the index is outside of a range you expect, and you are
/// changing the valid range. We expect this limitation to be lifted in the future.
///
/// For full details, see the documentation on the [view type](VirtualScroll).
pub fn virtual_hscroll<State, Action, ChildrenViews, F>(
    len: usize,
    func: F,
) -> VirtualHScroll<
    State,
    Action,
    ChildrenViews,
    F,
    impl Fn(&mut State, Range<usize>) -> MessageResult<Action> + Send + Sync + 'static,
>
where
    ChildrenViews: WidgetView<State, Action>,
    F: Fn(&mut State, usize) -> ChildrenViews + 'static,
    State: 'static,
    Action: 'static,
{
    VirtualHScroll {
        phantom: PhantomData,
        func,
        anchor_index: None,
        len,
        start_at: 0.,
        end_at: 1.,
        left_to_right: true,
        autoscroll_velocity: 0.,
        on_scroll: private::do_nothing::<State, Action>,
    }
}

/// Component for a [`VirtualScroll`] with unlimited children.
///
/// Arguments:
/// - `func` is the component for this element's children.
///   It is provided with the app's state and the index of the child.
///
/// For full details, see the documentation on the [view type](VirtualScroll).
pub fn unlimited_virtual_hscroll<State, Action, ChildrenViews, F>(
    func: F,
) -> VirtualHScroll<
    State,
    Action,
    ChildrenViews,
    F,
    impl Fn(&mut State, Range<usize>) -> MessageResult<Action> + Send + Sync + 'static,
>
where
    ChildrenViews: WidgetView<State, Action>,
    F: Fn(&mut State, usize) -> ChildrenViews + 'static,
    State: 'static,
    Action: 'static,
{
    VirtualHScroll {
        phantom: PhantomData,
        func,
        anchor_index: None,
        len: usize::MAX,
        start_at: 0.,
        end_at: 1.,
        left_to_right: true,
        autoscroll_velocity: 0.,
        on_scroll: private::do_nothing::<State, Action>,
    }
}

impl<State, Action, ChildrenViews, F, G> VirtualHScroll<State, Action, ChildrenViews, F, G> {
    pub fn jump_to(mut self, anchor_index: Option<usize>) -> Self {
        self.anchor_index = anchor_index;
        self
    }

    /// Sets the points (as ratios of width) where the first item starts and
    /// the last item ends in the viewport.
    pub fn start_end(mut self, start_at: f64, end_at: f64) -> Self {
        self.start_at = start_at;
        self.end_at = end_at;
        self
    }
    pub fn left_to_right(mut self, left_to_right: bool) -> Self {
        self.left_to_right = left_to_right;
        self
    }

    pub fn autoscroll_velocity(mut self, autoscroll_velocity: f64) -> Self {
        self.autoscroll_velocity = autoscroll_velocity;
        self
    }

    pub fn on_scroll<H>(self, on_scroll: H) -> VirtualHScroll<State, Action, ChildrenViews, F, H> {
        VirtualHScroll {
            phantom: self.phantom,
            func: self.func,
            anchor_index: self.anchor_index,
            len: self.len,
            start_at: self.start_at,
            end_at: self.end_at,
            left_to_right: self.left_to_right,
            autoscroll_velocity: self.autoscroll_velocity,
            on_scroll,
        }
    }
}

mod private {
    use std::{collections::HashMap, ops::Range};

    use xilem::core::MessageResult;

    pub fn do_nothing<State: 'static + 'static, Action>(
        _: &mut State,
        _: Range<usize>,
    ) -> MessageResult<Action> {
        MessageResult::Nop
    }

    #[expect(
        unnameable_types,
        reason = "Not meaningful public API; required to be public due to design of View trait"
    )]
    pub struct VirtualScrollState<View, State> {
        pub(super) pending_action: Option<super::widgets::VirtualHScrollFetchAction>,
        pub(super) children: HashMap<usize, ChildState<View, State>>,
    }

    pub(super) struct ChildState<View, State> {
        pub(super) view: View,
        pub(super) state: State,
    }
}

/// Create the view id used for child views.
const fn view_id_for_index(idx: usize) -> ViewId {
    ViewId::new(idx as _)
}

/// Get the index stored in the view id.
const fn index_for_view_id(id: ViewId) -> usize {
    id.routing_id() as _
}

impl<State, Action, ChildrenViews, F, G> ViewMarker
    for VirtualHScroll<State, Action, ChildrenViews, F, G>
{
}
impl<State, Action, ChildrenViews, F, G> View<State, Action, ViewCtx>
    for VirtualHScroll<State, Action, ChildrenViews, F, G>
where
    State: 'static,
    Action: 'static,
    ChildrenViews: WidgetView<State, Action>,
    F: Fn(&mut State, usize) -> ChildrenViews + 'static,
    G: Fn(&mut State, Range<usize>) -> MessageResult<Action> + Send + Sync + 'static,
{
    type Element = Pod<widgets::VirtualHScroll>;

    type ViewState = private::VirtualScrollState<ChildrenViews, ChildrenViews::ViewState>;

    fn build(&self, ctx: &mut ViewCtx, _: &mut State) -> (Self::Element, Self::ViewState) {
        // TODO: How does the anchor interact with Xilem?
        // Setting that seems like an imperative action?
        let pod = Pod::new(
            widgets::VirtualHScroll::new(self.anchor_index.unwrap_or(0), self.len)
                .with_start_end(self.start_at, self.end_at)
                .with_direction(self.left_to_right)
                .with_autoscroll_velocity(self.autoscroll_velocity),
        );
        ctx.record_action_source(pod.new_widget.id());
        (
            pod,
            private::VirtualScrollState {
                pending_action: None,
                children: HashMap::default(),
            },
        )
    }

    fn rebuild(
        &self,
        prev: &Self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
        mut app_state: &mut State,
    ) {
        if self.anchor_index != prev.anchor_index
            && let Some(idx) = self.anchor_index
        {
            widgets::VirtualHScroll::scroll_to(&mut element, idx);
        }

        let len_changed = self.len != prev.len;
        if len_changed {
            widgets::VirtualHScroll::set_len(&mut element, self.len);
        }

        let start_at = self.start_at != prev.start_at;
        if start_at {
            widgets::VirtualHScroll::set_start(&mut element, self.start_at);
        }

        let end_at = self.end_at != prev.end_at;
        if end_at {
            widgets::VirtualHScroll::set_end(&mut element, self.end_at);
        }

        let direction_changed = self.left_to_right != prev.left_to_right;
        if direction_changed {
            widgets::VirtualHScroll::set_direction(&mut element, self.left_to_right);
        }

        let autoscroll_velocity_changed = self.autoscroll_velocity != prev.autoscroll_velocity;
        if autoscroll_velocity_changed {
            widgets::VirtualHScroll::set_autoscroll_velocity(
                &mut element,
                self.autoscroll_velocity,
            );
        }

        // TODO: This code should be moved into `Self::message` once it becomes possible to
        // make a build/rebuild/teardown context there.
        //
        // This is because we could now be requesting items which are outside the claimed "valid range".
        // Naïvely, one might expect this to be impossible (because we only request rebuild, so the `app_logic` isn't ran)
        // However, even in these cases, things like `lens` will still generate a new view, so it's conceivable that
        // the valid range has changed. As such, we document the possibility of these requests above.
        if let Some(pending_action) = view_state.pending_action.take() {
            widgets::VirtualHScroll::will_handle_action(&mut element, &pending_action);
            // Teardown the old items
            for idx in pending_action.old_active.clone() {
                if !pending_action.target.contains(&idx) {
                    let Some(mut child_state) = view_state.children.remove(&idx) else {
                        debug_panic!(
                            "Tried to remove {idx} from virtual scroll {pending_action:?}, but it wasn't already present."
                        );
                        continue;
                    };
                    ctx.with_id(view_id_for_index(idx), |ctx| {
                        child_state.view.teardown(
                            &mut child_state.state,
                            ctx,
                            widgets::VirtualHScroll::child_mut(&mut element, idx).downcast(),
                        );
                        widgets::VirtualHScroll::remove_child(&mut element, idx);
                    });
                }
            }
            // Build all new items. Whilst we're here, rebuild all the others.
            // This avoids needing to carefully track which ones we just built.
            for idx in pending_action.target.clone() {
                if let Some(child) = view_state.children.get_mut(&idx) {
                    debug_assert!(
                        pending_action.old_active.contains(&idx),
                        "{idx} was asked to be removed in {pending_action:?}, but wasn't already present."
                    );
                    let next_child = (self.func)(&mut app_state, idx);
                    // Rebuild this existing item
                    ctx.with_id(view_id_for_index(idx), |ctx| {
                        next_child.rebuild(
                            &child.view,
                            &mut child.state,
                            ctx,
                            widgets::VirtualHScroll::child_mut(&mut element, idx).downcast(),
                            &mut app_state,
                        );
                        child.view = next_child;
                    });
                } else {
                    let new_child = (self.func)(&mut app_state, idx);
                    // Build the new item
                    ctx.with_id(view_id_for_index(idx), |ctx| {
                        let (new_element, child_state) = new_child.build(ctx, &mut app_state);
                        widgets::VirtualHScroll::add_child(
                            &mut element,
                            idx,
                            new_element.new_widget.erased(),
                        );
                        view_state.children.insert(
                            idx,
                            private::ChildState {
                                view: new_child,
                                state: child_state,
                            },
                        )
                    });
                }
            }
        } else {
            // Rebuild all existing items
            for (&idx, child) in &mut view_state.children {
                let next_child = (self.func)(&mut app_state, idx);
                ctx.with_id(view_id_for_index(idx), |ctx| {
                    next_child.rebuild(
                        &child.view,
                        &mut child.state,
                        ctx,
                        widgets::VirtualHScroll::child_mut(&mut element, idx).downcast(),
                        &mut app_state,
                    );
                    child.view = next_child;
                });
            }
        }
        debug_assert_eq!(
            element.widget.len(),
            view_state.children.len(),
            "VirtualScroll: Child added outside of the control of Xilem."
        );
    }

    fn teardown(
        &self,
        view_state: &mut Self::ViewState,
        ctx: &mut ViewCtx,
        mut element: Mut<'_, Self::Element>,
    ) {
        for (&idx, child) in &mut view_state.children {
            ctx.with_id(view_id_for_index(idx), |ctx| {
                child.view.teardown(
                    &mut child.state,
                    ctx,
                    widgets::VirtualHScroll::child_mut(&mut element, idx).downcast(),
                );
            });
        }
        ctx.teardown_action_source(element);
    }

    fn message(
        &self,
        view_state: &mut Self::ViewState,
        message: &mut MessageCtx,
        mut element: Mut<'_, Self::Element>,
        app_state: &mut State,
    ) -> MessageResult<Action> {
        if let Some(first) = message.take_first() {
            let child_idx = index_for_view_id(first);
            let target = view_state.children.get_mut(&child_idx);
            // TODO: Unfortunately, this isn't robust, because the message might be trying to reach a previous child.
            // We definitely don't want an O(n) storage of data for previous generations, but using a u64 generation
            // can never reasonably overflow (i.e. we should use two viewids here).
            if let Some(target) = target {
                let result = target.view.message(
                    &mut target.state,
                    message,
                    widgets::VirtualHScroll::child_mut(&mut element, child_idx).downcast(),
                    app_state,
                );
                return result;
            } else {
                tracing::error!(
                    "Message sent to unloaded view in `VirtualScroll::message`: {message:?}"
                );
                return MessageResult::Stale;
            }
        }
        if let Some(action) = message.take_message::<widgets::VirtualHScrollAction>() {
            match *action {
                widgets::VirtualHScrollAction::Fetch(action) => {
                    // TODO: We should be able to rebuild here (we have the element)
                    // but we currently can't make a `ViewCtx`
                    view_state.pending_action = Some(action);
                    // We need rebuild to be called now.
                    MessageResult::RequestRebuild
                }
                widgets::VirtualHScrollAction::Scroll(widgets::VirtualHScrollScrollAction {
                    range_in_viewport,
                }) => (self.on_scroll)(app_state, range_in_viewport),
            }
        } else {
            tracing::error!(?message, "Wrong message type in VirtualScroll::message");
            MessageResult::Stale
        }
    }
}
