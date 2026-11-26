"""Render one cell of the matrix (ref x style x theme x state) and return its geometry."""
from core import check_applied, dump, open_page
from states import STATES


def render_cell(session, site, tree, theme, ui_style, state, shot_path=None, drop=None, delays=None):
    viewport, setup = STATES[state]
    loaded = open_page(session, site, tree, theme, ui_style, viewport, delays=delays, drop=drop)
    try:
        check_applied(loaded.page, theme, ui_style, tree)
        setup(loaded)
        loaded.check()
        d = dump(loaded.page)
        d["meta"]["theme"] = theme
        d["meta"]["state"] = state
        if shot_path:
            shot_path.parent.mkdir(parents=True, exist_ok=True)
            loaded.page.screenshot(path=str(shot_path))
        loaded.check()
        return d
    finally:
        loaded.close()
