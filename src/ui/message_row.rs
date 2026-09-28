use gtk4::prelude::*;

/// Create a message row widget for the conversation list.
/// `role` should be "user" or "assistant".
/// Returns the container box and the label, so the caller can update the
/// label's text incrementally as tokens stream in.
pub fn new_message_row(role: &str, content: &str) -> (gtk4::Box, gtk4::Label) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    row.set_margin_top(4);
    row.set_margin_bottom(4);
    row.set_margin_start(8);
    row.set_margin_end(8);

    // Avatar showing "U" for user, "A" for assistant
    let avatar_text = if role == "user" { "U" } else { "A" };
    let avatar = libadwaita::Avatar::new(32, Some(avatar_text), true);
    avatar.set_valign(gtk4::Align::Start);
    row.append(&avatar);

    // Message label
    let label = gtk4::Label::new(Some(content));
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_valign(gtk4::Align::Start);

    // Style: user messages slightly indented, assistant left-aligned
    if role == "user" {
        label.add_css_class("caption");
    }

    row.append(&label);
    (row, label)
}
