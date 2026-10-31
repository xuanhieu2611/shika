//! Shared widgets and constants for every surface: fonts, icons, key caps,
//! tooltips, hints, buttons, list rows, and dialog shells.
use super::*;

pub(super) const UI_FONT: &str = ".AppleSystemUIFont";

pub(super) const MONO: &str = "JetBrains Mono";

/// Heroicons cog-6-tooth (24px solid). MIT notice: assets/licenses/Heroicons-MIT.txt.
/// Drawn as an alpha mask and tinted by the element's text color.
pub(super) const SETTINGS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" fill-rule="evenodd" d="M11.078 2.25c-.917 0-1.699.663-1.85 1.567L9.05 4.889c-.02.12-.115.26-.297.348a7.493 7.493 0 0 0-.986.57c-.166.115-.334.126-.45.083L6.3 5.508a1.875 1.875 0 0 0-2.282.819l-.922 1.597a1.875 1.875 0 0 0 .432 2.385l.84.692c.095.078.17.229.154.43a7.598 7.598 0 0 0 0 1.139c.015.2-.059.352-.153.43l-.841.692a1.875 1.875 0 0 0-.432 2.385l.922 1.597a1.875 1.875 0 0 0 2.282.818l1.019-.382c.115-.043.283-.031.45.082.312.214.641.405.985.57.182.088.277.228.297.35l.178 1.071c.151.904.933 1.567 1.85 1.567h1.844c.916 0 1.699-.663 1.85-1.567l.178-1.072c.02-.12.114-.26.297-.349.344-.165.673-.356.985-.57.167-.114.335-.125.45-.082l1.02.382a1.875 1.875 0 0 0 2.28-.819l.923-1.597a1.875 1.875 0 0 0-.432-2.385l-.84-.692c-.095-.078-.17-.229-.154-.43a7.614 7.614 0 0 0 0-1.139c-.016-.2.059-.352.153-.43l.84-.692c.708-.582.891-1.59.433-2.385l-.922-1.597a1.875 1.875 0 0 0-2.282-.818l-1.02.382c-.114.043-.282.031-.449-.083a7.49 7.49 0 0 0-.985-.57c-.183-.087-.277-.227-.297-.348l-.179-1.072a1.875 1.875 0 0 0-1.85-1.567h-1.843ZM12 15.75a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5Z"/></svg>"##;

/// Heroicons folder-plus (24px outline), on the Projects header. Drawn as an
/// alpha mask and tinted by the element's text color.
/// MIT notice: assets/licenses/Heroicons-MIT.txt.
pub(super) const FOLDER_PLUS_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 10.5v6m3-3H9m4.06-7.19-2.12-2.12a1.5 1.5 0 0 0-1.061-.44H4.5A2.25 2.25 0 0 0 2.25 6v12a2.25 2.25 0 0 0 2.25 2.25h15A2.25 2.25 0 0 0 21.75 18V9a2.25 2.25 0 0 0-2.25-2.25h-5.379a1.5 1.5 0 0 1-1.06-.44Z"/></svg>"##;

/// The agent column toggle: a window with its left pane split off. Drawn for
/// Shika at 16px.
pub(super) const COLUMN_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="#000" fill-rule="evenodd" d="M3 2h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm3 1.5H3a.5.5 0 0 0-.5.5v8a.5.5 0 0 0 .5.5h3v-9Zm1.5 0v9H13a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H7.5Z"/></svg>"##;

/// Original Shika glyph: a repository folder with a terminal prompt.
pub(super) const CHECKOUT_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><g fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M1.5 5V3.5h5l1.5 2h6.5v7h-13V5Z"/><path d="m5 8 1.5 1.5L5 11m4 0h2"/></g></svg>"##;

/// Octicons git-branch, before the base branch on a project header. Drawn as
/// an alpha mask and tinted by the element's text color.
/// MIT notice: assets/licenses/Octicons-MIT.txt.
pub(super) const BRANCH_ICON: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill="#000" fill-rule="evenodd" d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.493 2.493 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z"/></svg>"##;

/// A tooltip naming a control and its key, such as `Settings ⌘,`.
pub(super) struct KeyTip {
    pub(super) bg: Rgba,
    pub(super) fg: Rgba,
    pub(super) text: SharedString,
}

impl Render for KeyTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(self.bg)
            .text_color(self.fg)
            .text_size(px(12.))
            .font_family(UI_FONT)
            .child(self.text.clone())
    }
}

impl Shika {
    /// Cancel, or Done in Settings. Reads "Working..." while a close runs.
    pub(super) fn cancel_button(
        &self,
        label: &'static str,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        dialog_button(
            "cancel",
            if self.busy { "Working..." } else { label },
            "esc",
            chrome,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            if !this.busy {
                this.cancel_overlay(window, cx);
            }
        }))
    }
}

/// One key hint at the right end of the selected row's second line.
pub(super) fn row_hint(key: &'static str, label: &'static str, chrome: &Chrome) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .ml(px(6.))
        .text_size(px(11.))
        .text_color(chrome.ink_3)
        .child(kbd(key, chrome.sunken, chrome.ink_2).py_0())
        .child(label)
}

/// The "Leads" or "Projects" header: a quiet sentence-case label, 12px
/// medium ink-3, aligned with the project header's text. Its actions follow
/// at the right, padded like a project header so the `+` buttons line up.
pub(super) fn section_header(text: &'static str, chrome: &Chrome) -> gpui::Div {
    div()
        .min_h(px(24.))
        .flex()
        .items_center()
        .gap(px(2.))
        .pl(px(8.))
        .pr(px(4.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(chrome.ink_3)
                .child(text),
        )
}

/// A key cap: mono 10.5px on a small rounded fill.
pub(super) fn kbd(text: impl Into<SharedString>, bg: Rgba, fg: Rgba) -> gpui::Div {
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .font_family(MONO)
        .text_size(px(10.5))
        .line_height(px(14.))
        .child(text.into())
}

/// A key cap and the verb it does, as in the footer: `⌘] ⌘[ move`. An empty
/// key leaves only the words. The pair stays on one line and does not shrink,
/// so a wrapped row hides nothing.
pub(super) fn hint(key: &str, label: &str, chrome: &Chrome) -> gpui::Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(11.))
        .line_height(px(14.))
        .text_color(chrome.ink_3)
        .when(!key.is_empty(), |d| {
            d.child(kbd(key.to_string(), chrome.sunken, chrome.ink_2))
        })
        .child(label.to_string())
}

/// A secondary button without its label: raised fill, control border, and
/// the light-mode control shadow.
pub(super) fn secondary_button(
    id: impl Into<gpui::ElementId>,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let hover = chrome.raised_hover;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .border_1()
        .border_color(chrome.line_control)
        .bg(chrome.raised)
        .rounded(px(7.))
        .text_size(px(12.5))
        .line_height(px(16.))
        .text_color(chrome.ink_1)
        .cursor_pointer()
        .shadow(vec![
            BoxShadow::new(px(0.), px(1.), chrome.control_shadow.into()).blur_radius(px(1.)),
        ])
        .hover(move |style| style.bg(hover))
}

/// A list row in the picker, Settings, and the leftovers list.
pub(super) fn list_row(selected: bool, chrome: &Chrome) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(7.))
        .text_size(px(13.))
        .line_height(px(19.))
        .when(selected, |d| d.bg(chrome.row_selected))
}

/// The floating surface shared by the picker, every dialog, and the update notice.
pub(super) fn dialog_shell(
    width: f32,
    max_h: Pixels,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    panel_shell("overlay-panel", width, max_h, chrome)
}

/// A confirm dialog: 400px, padded for a title, facts, and buttons.
pub(super) fn dialog(chrome: &Chrome, max_h: Pixels) -> gpui::Stateful<gpui::Div> {
    dialog_shell(400., max_h, chrome)
        .pt(px(20.))
        .px(px(20.))
        .pb(px(16.))
        .gap(px(8.))
}

pub(super) fn dialog_title(d: gpui::Div) -> gpui::Div {
    d.text_size(px(14.))
        .line_height(px(20.))
        .font_weight(FontWeight::SEMIBOLD)
}

pub(super) fn dialog_text(chrome: &Chrome) -> gpui::Div {
    div()
        .text_size(px(13.))
        .line_height(px(19.))
        .text_color(chrome.ink_2)
}

pub(super) fn dialog_buttons() -> gpui::Div {
    div().flex().justify_end().gap(px(8.)).mt(px(10.))
}

/// `$HOME`, to write paths with `~`.
pub(super) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// A 24px column button: ink-3, ink-1 on the hover wash, with its name and
/// key in a tooltip. `icon` is an alpha-mask glyph at 15px; without one it
/// is the 16px `+` the project headers use.
pub(super) fn icon_button(
    id: &'static str,
    icon: Option<&'static [u8]>,
    tip: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let (hover, ink_1, ink_3) = (chrome.hover, chrome.ink_1, chrome.ink_3);
    let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
    let button = div()
        .id(id)
        .group(id)
        .occlude()
        .flex_none()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(ink_3)
        .hover(move |style| style.bg(hover).text_color(ink_1))
        .tooltip(move |_, cx| {
            cx.new(|_| KeyTip {
                bg: tip_bg,
                fg: tip_fg,
                text: tip.into(),
            })
            .into()
        });
    match icon {
        Some(data) => button.child(
            gpui::svg()
                .data(data)
                .size(px(15.))
                .text_color(ink_3)
                .group_hover(id, move |style| style.text_color(ink_1)),
        ),
        None => button.text_size(px(16.)).child("+"),
    }
}

/// A dialog's secondary action with its key after the label.
pub(super) fn dialog_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    key: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, chrome)
        .gap(px(8.))
        .pl(px(12.))
        .pr(px(10.))
        .py(px(6.))
        .child(label.into())
        .child(
            div()
                .font_family(MONO)
                .text_size(px(10.5))
                .text_color(chrome.ink_3)
                .child(key),
        )
}

/// A notice button with no key. The update card does not take focus.
pub(super) fn notice_secondary(
    id: &'static str,
    label: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, chrome)
        .px(px(12.))
        .py(px(6.))
        .child(label)
}

/// The ink-filled notice button, with the primary button's padding and no key.
pub(super) fn notice_primary(
    id: &'static str,
    label: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let hover = chrome.primary_hover;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(7.))
        .px(px(12.))
        .py(px(7.))
        .bg(chrome.primary_bg)
        .text_color(chrome.primary_fg)
        .text_size(px(12.5))
        .line_height(px(16.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label)
}

/// The ink-filled primary action of a dialog.
pub(super) fn primary_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    key: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    let hover = chrome.primary_hover;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(7.))
        .pl(px(12.))
        .pr(px(10.))
        .py(px(7.))
        .bg(chrome.primary_bg)
        .text_color(chrome.primary_fg)
        .text_size(px(12.5))
        .line_height(px(16.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label.into())
        .child(
            div()
                .font_family(MONO)
                .text_size(px(10.5))
                .text_color(chrome.kbd_inverse)
                .child(key),
        )
}

/// A settings `-` or `+`: a small secondary button.
pub(super) fn step_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    secondary_button(id, chrome)
        .size(px(26.))
        .justify_center()
        .rounded(px(6.))
        .text_size(px(13.))
        .text_color(chrome.ink_2)
        .child(label)
}

/// A settings number or text field.
pub(super) fn text_field(
    id: impl Into<gpui::ElementId>,
    width: f32,
    editing: bool,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(width))
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(2.))
        .rounded(px(6.))
        .bg(chrome.raised)
        .border_1()
        .border_color(if editing {
            chrome.focus
        } else {
            chrome.line_control
        })
        .font_family(MONO)
        .text_size(px(12.))
        .cursor_text()
}

/// One segment of a segmented control.
pub(super) fn segment(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    active_bg: Rgba,
    active_fg: Rgba,
    idle_fg: Rgba,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(5.))
        .text_size(px(12.))
        .line_height(px(16.))
        .cursor_pointer()
        .when(active, |d| d.bg(active_bg).text_color(active_fg))
        .when(!active, |d| d.text_color(idle_fg))
        .child(label.into())
}

pub(super) fn panel_shell(
    id: &'static str,
    width: f32,
    max_h: Pixels,
    chrome: &Chrome,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(width))
        .max_h(max_h)
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .bg(chrome.overlay)
        .text_color(chrome.ink_1)
        .shadow(vec![
            BoxShadow::new(px(0.), px(24.), chrome.dialog_shadow.into()).blur_radius(px(60.)),
            BoxShadow::new(px(0.), px(0.), chrome.dialog_ring.into()).spread_radius(px(0.5)),
        ])
}
