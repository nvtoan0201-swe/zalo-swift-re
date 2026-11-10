//! A clickable button inside a clickable card (the file card's "Lưu về máy…" button): the button's click must run
//! and must not also trigger the card's.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AppContext, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, StatefulInteractiveElement, Styled, TestApp, Window, div, point, px};

struct Tip;
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("tip")
    }
}

struct Card {
    card: Rc<Cell<usize>>,
    button: Rc<Cell<usize>>,
    stop_on_down: bool,
}

impl Render for Card {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (c, b) = (self.card.clone(), self.button.clone());
        let mut button = div().id("button").absolute().left(px(100.0)).top(px(10.0)).size(px(20.0));
        if self.stop_on_down {
            button = button.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        }
        let button = button.tooltip(|_, cx| cx.new(|_| Tip).into()).on_click(move |_, _, cx| {
            cx.stop_propagation();
            b.set(b.get() + 1);
        });
        div().size_full().child(div().id("card").relative().w(px(200.0)).h(px(40.0)).child(button).on_click(move |_, _, _| c.set(c.get() + 1)))
    }
}

fn clicks(stop_on_down: bool, at: gpui::Point<gpui::Pixels>) -> (usize, usize) {
    let mut app = TestApp::new();
    let (card, button) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let (c, b) = (card.clone(), button.clone());
    let mut window = app.open_window(move |_, _| Card { card: c, button: b, stop_on_down });
    window.draw();
    window.simulate_mouse_move(at);
    window.draw();
    window.simulate_click(at, MouseButton::Left);
    app.run_until_parked();
    (card.get(), button.get())
}

#[test]
fn button_inside_card() {
    let on_button = point(px(110.0), px(20.0));
    let on_card = point(px(30.0), px(20.0));
    eprintln!("stop on mouse-down: button press → (card, button) = {:?}", clicks(true, on_button));
    assert_eq!(clicks(false, on_button), (0, 1), "button press: only the button");
    assert_eq!(clicks(false, on_card), (1, 0), "card press: only the card");
}
