use std::ops::Range;

use color::{AlphaColor, Oklch, palette::css::GRAY};
use masonry::layout::Length;
use xilem::{EventLoop, WidgetView, WindowOptions, Xilem, style::Style, view::label};
use xilem::{view::flex_col, winit::error::EventLoopError};

use xilem_understory_scroll::view::virtual_hscroll;

struct AppState {
    visible_range: Range<usize>,
}

fn app_logic(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    flex_col((
        virtual_hscroll(100, |_: &mut AppState, idx| {
            label(idx.to_string())
                .width(Length::px(51.))
                .border(GRAY, 1.)
                .background_color(
                    AlphaColor::<Oklch>::new([0.5, 0.8, idx as f32 * 2., 1.]).convert(),
                )
        })
        .start_end(0.5, 0.5)
        .left_to_right(false)
        .autoscroll_velocity(10.)
        .on_scroll(|state: &mut AppState, range| {
            state.visible_range = range;
            xilem::core::MessageResult::Action(())
        }),
        label(format!("{:?}", state.visible_range)),
    ))
}

fn main() -> Result<(), EventLoopError> {
    let state = AppState {
        visible_range: 0..0,
    };
    let app = Xilem::new_simple(state, app_logic, WindowOptions::new("Counter app"));
    app.run_in(EventLoop::with_user_event())?;
    Ok(())
}
