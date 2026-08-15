use gpui::*;
use gpui_component::{Root, button::Button, button::ButtonVariants};

struct Metrolist;

impl Render for Metrolist {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .child("Metrolist")
            .child(Button::new("get-started").primary().label("Get started"))
    }
}

fn main() {
    gpui_platform::application().run(|cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| Metrolist);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open the main window");
        })
        .detach();
    });
}
