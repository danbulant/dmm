use rune::tok::Theme;
use rune::{El, TextEdit};
use rune_kit::Kit;

pub(super) fn field(el: El, edit: &TextEdit, k: &Kit, t: Theme) -> El {
    el.fill(t.card).border(
        1.0,
        if edit.focused {
            k.c.border_focus
        } else {
            k.c.border_base
        },
    )
}
