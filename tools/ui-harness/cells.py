"""Render one cell of the matrix (ref x style x theme x state) and return its geometry."""
from core import CellFailure, HarnessError, check_applied, dump, open_page
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
    except (CellFailure, HarnessError):
        raise
    except Exception as e:
        # A timeout is usually the page not doing what the state needs; if the
        # mock refused a call that is the real cause, so look there first.
        loaded.check()
        raise CellFailure(str(e).strip().splitlines()[0])
    finally:
        loaded.close()
