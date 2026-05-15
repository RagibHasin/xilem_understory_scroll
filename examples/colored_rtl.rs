use color::{AlphaColor, Oklch, palette::css::GRAY};
use masonry::layout::AsUnit;
use xilem::view::{self, FlexExt, flex_col, label, radio_button};
use xilem::winit::error::EventLoopError;
use xilem::{EventLoop, WidgetView, WindowOptions, Xilem, style::Style};

use xilem_understory_scroll::view::{ScrollDirection, virtual_hscroll};

struct AppState {
    visible_range: std::ops::Range<usize>,
    direction: ScrollDirection,
}

fn app_logic(state: &mut AppState) -> impl WidgetView<AppState> + use<> {
    flex_col((
        virtual_hscroll(100, |_: &mut AppState, idx| {
            label(idx.to_string())
                .width(51.px())
                .border(GRAY, 1.px())
                .background_color(
                    AlphaColor::<Oklch>::new([0.5, 0.8, idx as f32 * 2., 1.]).convert(),
                )
        })
        .start_end(0.5, 0.5)
        .direction(state.direction)
        .scrolling(false)
        .on_scroll(|state: &mut AppState, range| {
            state.visible_range = range;
            xilem::core::MessageResult::Action(())
        })
        .flex(1.),
        label(format!("Visible range: {:?}", state.visible_range))
            .text_alignment(xilem::TextAlign::Center),
        view::radio_group(flex_col((
            radio_button(
                "Top to bottom",
                state.direction == ScrollDirection::TopToBottom,
                |state: &mut AppState| state.direction = ScrollDirection::TopToBottom,
            ),
            radio_button(
                "Bottom to top",
                state.direction == ScrollDirection::BottomToTop,
                |state: &mut AppState| state.direction = ScrollDirection::BottomToTop,
            ),
            radio_button(
                "Left to right",
                state.direction == ScrollDirection::LeftToRight,
                |state: &mut AppState| state.direction = ScrollDirection::LeftToRight,
            ),
            radio_button(
                "Right to left",
                state.direction == ScrollDirection::RightToLeft,
                |state: &mut AppState| state.direction = ScrollDirection::RightToLeft,
            ),
        ))),
    ))
    .cross_axis_alignment(view::CrossAxisAlignment::Center)
}

fn main() -> Result<(), EventLoopError> {
    let state = AppState {
        visible_range: 0..0,
        direction: ScrollDirection::TopToBottom,
    };
    let app = Xilem::new_simple(state, app_logic, WindowOptions::new("Virtual scroll"));
    app.run_in(EventLoop::with_user_event())?;
    Ok(())
}
