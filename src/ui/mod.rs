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

#[cfg(test)]
pub(super) fn builder_ids_in_source(src: &str) -> Vec<&str> {
    let body = src.split("#[cfg(test)]").next().unwrap_or(src);
    let mut ids = Vec::new();
    for needle in ["obj(&builder, \"", "obj(builder, \""] {
        let mut rest = body;
        while let Some(pos) = rest.find(needle) {
            let after = &rest[pos + needle.len()..];
            let Some(end) = after.find('"') else {
                break;
            };
            ids.push(&after[..end]);
            rest = &after[end + 1..];
        }
    }
    ids
}
