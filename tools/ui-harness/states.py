"""The scenarios of the render matrix. Every state is reached through the front end's own
controls and the backend's events, never by editing the DOM, so it exercises the real code paths."""
import fixtures
from core import CellFailure, NARROW, VIEWPORT, HarnessError, raf2


def fire(loaded, event, payload=None):
    loaded.page.evaluate("([e, p]) => window.__MOCK__.fire(e, p)", [event, payload])
    loaded.check()


def populate(loaded, batch=True):
    """The user adds files; with batch=True a batch is also running and row 1 is mid-encode."""
    page = loaded.page
    page.click("#btn-add-files")
    want = len(fixtures.QUEUE)
    page.wait_for_function(f"document.querySelectorAll('#queue-body tr').length === {want}", timeout=5000)
    if batch:
        fire(loaded, "batch-started")
        page.evaluate("window.__MOCK__.setNow(1000)")
        fire(loaded, "batch-progress", {"current": 2, "total": 5})
        fire(loaded, "batch-status", fixtures.QUEUE[1]["fileName"])
        fire(loaded, "batch-command", "ffmpeg -i Documentary.S01E03.1080p.mkv -c:v hevc_nvenc out.mkv")
        fire(loaded, "queue-item-updated", [1, "Encoding"])
        page.evaluate("window.__MOCK__.setNow(61000)")
        fire(loaded, "file-progress", {"percent": 41.7, "timeSecs": 1500.0, "totalSecs": 3600.0, "pass": None})
    raf2(page)
    loaded.check()


def expect(page, js, what):
    if not page.evaluate(js):
        raise CellFailure(f"state did not take effect: {what}")


def s_empty(loaded):
    expect(loaded.page, "document.querySelectorAll('#queue-body tr').length === 0", "queue is empty")


def s_rows(loaded):
    populate(loaded)
    statuses = loaded.page.evaluate("[...document.querySelectorAll('#queue-body tr')].map(r => r.dataset.status || r.className)")
    if len(statuses) != len(fixtures.QUEUE):
        raise CellFailure(f"expected {len(fixtures.QUEUE)} rows, got {len(statuses)}")


def s_drawer(loaded):
    populate(loaded)
    loaded.page.click("#advanced-toggle")
    raf2(loaded.page)
    expect(loaded.page, "document.querySelector('#advanced-toggle').getAttribute('aria-expanded') === 'true' "
                        "&& document.querySelector('#settings-panel').offsetHeight > 0", "Advanced settings are open")


def s_rail_collapsed(loaded):
    populate(loaded)
    page = loaded.page
    before = page.evaluate("document.querySelector('#right-panel').getBoundingClientRect().width")
    page.click("#splitter-toggle")
    raf2(page, 150)
    after = page.evaluate("document.querySelector('#right-panel').getBoundingClientRect().width")
    if not after < before:
        raise CellFailure(f"rail did not shrink when collapsed ({before} -> {after})")


def s_narrow(loaded):
    populate(loaded)


def s_modal_overwrite(loaded):
    populate(loaded)
    fire(loaded, "overwrite-prompt", "Documentary.S01E03.1080p.mkv")
    raf2(loaded.page)
    expect(loaded.page, "document.querySelector('#modal-overwrite').classList.contains('visible')", "overwrite modal visible")


def s_modal_fallback(loaded):
    populate(loaded)
    fire(loaded, "fallback-prompt", "Documentary.S01E03.1080p.mkv")
    raf2(loaded.page)
    expect(loaded.page, "document.querySelector('#modal-fallback').classList.contains('visible')", "fallback modal visible")


def s_modal_preflight(loaded):
    # No batch: Start must be enabled for the click to reach the preflight check.
    populate(loaded, batch=False)
    loaded.page.evaluate("window.__MOCK__.fixtures.preflightWarnings = " + __import__("json").dumps(fixtures.PREFLIGHT_WARNINGS))
    loaded.page.click("#btn-start")
    loaded.page.wait_for_function("document.querySelector('#modal-preflight').classList.contains('visible')", timeout=5000)
    raf2(loaded.page)


def s_log(loaded):
    populate(loaded)
    for line in fixtures.LOG_LINES:
        fire(loaded, "log", line)
    loaded.page.click("#bottom-panel-toggle")
    raf2(loaded.page, 150)
    expect(loaded.page, "document.querySelector('#bottom-panel-body').classList.contains('open') "
                        "&& document.querySelectorAll('#log-content > div').length >= %d" % len(fixtures.LOG_LINES),
           "log panel open with lines")


# name -> (viewport, setup)
STATES = {
    "empty": (VIEWPORT, s_empty),
    "rows": (VIEWPORT, s_rows),
    "drawer": (VIEWPORT, s_drawer),
    "rail-collapsed": (VIEWPORT, s_rail_collapsed),
    "narrow-800": (NARROW, s_narrow),
    "modal-overwrite": (VIEWPORT, s_modal_overwrite),
    "modal-fallback": (VIEWPORT, s_modal_fallback),
    "modal-preflight": (VIEWPORT, s_modal_preflight),
    "log": (VIEWPORT, s_log),
}

THEMES = ["Default Dark", "Default Light", "Nord", "Jessica Dark", "Creditor", "Classic Dark", "Classic Light"]
STYLES = ["doublet", "classic"]
