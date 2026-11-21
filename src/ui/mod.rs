mod dialogs;
mod preview;
mod rows;
pub mod window;

use gtk::glib;
use gtk::glib::object::IsA;
use gtk::Builder;

pub(super) fn obj<T: IsA<glib::Object>>(builder: &Builder, id: &str) -> T {
    builder
        .object(id)
        .unwrap_or_else(|| panic!("missing UI object `{id}`"))
}
