//! Clicks reach rows inside a scrolling area of an absolutely positioned overlay (the sheet layout used by the app).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, StatefulInteractiveElement, Styled, TestApp, Window, div, point, px};

struct Sheet {
    clicks: Rc<Cell<usize>>,
    scroll: bool,
    occlude: u8,
}

impl Render for Sheet {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col();
        for i in 0..40 {
            let c = self.clicks.clone();
            list = list.child(div().id(gpui::ElementId::NamedInteger("row".into(), i as u64)).h(px(30.0)).w_full().on_click(move |_, _, _| c.set(c.get() + 1)));
        }
        let area = div().id("area").flex_1().min_h(px(0.0));
        let area = if self.scroll { area.overflow_y_scroll() } else { area.overflow_hidden() };
        div().size_full().relative().child(div().size_full()).child(
            div().absolute().inset_0().flex().items_center().justify_center().child(
                {
                    let panel = div().id("panel").relative().w(px(400.0)).h(px(400.0)).flex().flex_col();
                    let panel = match self.occlude {
                        1 => panel.occlude(),
                        2 => panel.child(div().absolute().inset_0().occlude()),
                        _ => panel,
                    };
                    panel.child(div().h(px(50.0))).child(area.child(list)).child(div().h(px(50.0)))
                },
            ),
        )
    }
}

fn clicks_for(scroll: bool, occlude: u8) -> usize {
    let mut app = TestApp::new();
    let clicks = Rc::new(Cell::new(0));
    let c = clicks.clone();
    let mut window = app.open_window(move |_, _| Sheet { clicks: c, scroll, occlude });
    window.draw();
    let size = window.update(|_, w, _| w.viewport_size());
    // First row of the area: panel centered, 50 px header, row height 30.
    let p = point(size.width / 2.0, (size.height - px(400.0)) / 2.0 + px(65.0));
    window.simulate_mouse_move(p);
    window.draw();
    window.simulate_click(p, MouseButton::Left);
    app.run_until_parked();
    clicks.get()
}

#[test]
fn clicks_reach_rows_in_overlay_scroll_areas() {
    assert_eq!(clicks_for(false, 0), 1, "plain clipped area");
    assert_eq!(clicks_for(true, 0), 1, "scrolling area");
    assert_eq!(clicks_for(true, 2), 1, "scrolling area over an occluder layer");
    eprintln!("panel.occlude() + scroll: {}", clicks_for(true, 1));
    eprintln!("panel.occlude() + clipped: {}", clicks_for(false, 1));
}
